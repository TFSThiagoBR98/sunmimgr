//! Download loop: installs files on one or more devices via the Sunmi
//! firmware `systool` service.
//!
//! Port of the `thread_dt_withtool` flow: one task per device, items run
//! sequentially per device, results validated against the `{"ret":...}`
//! JSON contract, with a total deadline and cooperative stop (the original
//! used a `stop_thread` flag and a 600 s timeout).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::filetype::FileKind;

/// Kinds accepted by this flow (port scope: App, Apn, AppCrp).
pub const SUPPORTED_KINDS: [FileKind; 3] = [FileKind::App, FileKind::Apn, FileKind::AppCrp];

/// Where files are staged on the device before `systool install` runs.
///
/// The firmware reports its own staging dir via `systool version`
/// (`systool_tmp_dir=...`); when it is empty (observed on P2-A11 4.0.69,
/// production build) the tools fall back to this. `/data/local/tmp` was
/// validated against a production P2-A11: push via the sync service works
/// and the daemon reads the file from there.
///
/// Production Sunmi firmware blocks generic `shell:` commands (POS secure
/// builds), but the daemon's route (`shell:systool ...`) stays whitelisted —
/// that is why installation is push + systool, never a shell script.
pub const FALLBACK_TMP_DIR: &str = "/data/local/tmp";

/// One file queued for installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    pub kind: FileKind,
    pub path: PathBuf,
}

impl DownloadItem {
    pub fn new(kind: FileKind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
        }
    }

    /// `systool` arguments forwarded over `adb shell`, installing the file
    /// already staged on the device (port of the switch in
    /// `thread_dt_withtool`).
    pub fn systool_args_for(&self, device_path: &str) -> Result<String, DownloadError> {
        if !SUPPORTED_KINDS.contains(&self.kind) {
            return Err(DownloadError::UnsupportedKind(self.kind.label()));
        }
        self.kind
            .systool_command(device_path)
            .ok_or(DownloadError::UnsupportedKind(self.kind.label()))
    }

    /// `systool` arguments for the local file path (kept for tests; the real
    /// flow stages the file on the device first — see `run_device`).
    pub fn systool_args(&self) -> Result<String, DownloadError> {
        self.systool_args_for(&self.path.display().to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("file kind {0} is not supported by this flow (supported: App, Apn, AppCrp)")]
    UnsupportedKind(&'static str),
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("systool returned error ret={ret}: {msg}")]
    Systool { ret: i64, msg: String },
    #[error("systool returned invalid output: {0}")]
    InvalidOutput(String),
    #[error(transparent)]
    Transport(#[from] sunmi_adb::ToolError),
    #[error("download stopped")]
    Stopped,
    #[error("timeout")]
    Timeout,
}

/// Checks every item before starting a run (the original checked
/// `File.Exists` per file and rejected unsupported kind mixes).
pub fn validate(items: &[DownloadItem]) -> Result<(), DownloadError> {
    for item in items {
        if !SUPPORTED_KINDS.contains(&item.kind) {
            return Err(DownloadError::UnsupportedKind(item.kind.label()));
        }
        if !item.path.is_file() {
            return Err(DownloadError::FileNotFound(
                item.path.display().to_string(),
            ));
        }
    }
    Ok(())
}

/// Firmware response contract: `{"ret":0,...}` on success.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SystoolResponse {
    pub ret: i64,
    #[serde(default)]
    pub msg: String,
}

/// Result of `systool version`
/// (`systool_versioncode=N`, `systool_tmp_dir=path`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystoolVersion {
    pub version_code: Option<i64>,
    pub tmp_dir: Option<String>,
}

/// Parses the `systool version` output; the empty `systool_tmp_dir=`
/// reported by the firmware yields `None` (caller applies the fallback).
pub fn parse_systool_version(output: &str) -> SystoolVersion {
    let mut version = SystoolVersion::default();
    for line in output.lines() {
        let line = line.trim();
        if let Some(code) = line.strip_prefix("systool_versioncode=") {
            version.version_code = code.parse().ok();
        }
        if let Some(dir) = line.strip_prefix("systool_tmp_dir=") {
            let dir = dir.trim();
            if !dir.is_empty() {
                version.tmp_dir = Some(dir.to_string());
            }
        }
    }
    version
}

/// Port of the original result check: the first line starting with `{"ret`
/// is the response; adb may prepend daemon preamble lines.
pub fn parse_systool_response(output: &str) -> Result<SystoolResponse, DownloadError> {
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with('{') {
            return serde_json::from_str(line)
                .map_err(|_| DownloadError::InvalidOutput(line.to_string()));
        }
    }
    Err(DownloadError::InvalidOutput(output.to_string()))
}

fn check_response(output: &str) -> Result<(), DownloadError> {
    let response = parse_systool_response(output)?;
    if response.ret == 0 {
        Ok(())
    } else {
        Err(DownloadError::Systool {
            ret: response.ret,
            msg: response.msg,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadEvent {
    ItemStarted { serial: String, item: DownloadItem },
    ItemCompleted { serial: String, item: DownloadItem },
    ItemFailed {
        serial: String,
        item: DownloadItem,
        error: String,
    },
    /// Hard device-level failure before any item could run (e.g. could not
    /// resolve the device tmp dir).
    DeviceFailed { serial: String, error: String },
    /// Device finished its queue; `ok == false` means it stopped early
    /// (error, stop request or timeout), like the original per-device break.
    DeviceFinished { serial: String, ok: bool },
    /// All devices finished.
    Finished,
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Total deadline per device (the original used 600 s).
    pub timeout: Duration,
    /// Cooperative stop: checked between items and cancels the running
    /// systool invocation (the original's `stop_thread`).
    pub stop: CancellationToken,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            stop: CancellationToken::new(),
        }
    }
}

/// Runs the download flow against an adb client.
#[derive(Debug, Clone)]
pub struct Downloader {
    adb: sunmi_adb::Adb,
}

impl Downloader {
    pub fn new(adb: sunmi_adb::Adb) -> Self {
        Self { adb }
    }

    /// `systool get posinfo` parsed (port of the device-info refresh flow).
    pub async fn term_info(&self, serial: &str) -> Result<crate::terminfo::TmInfo, DownloadError> {
        let output = self.adb.systool(serial, "get posinfo").await?;
        crate::terminfo::parse_tm_info(&output)
    }

    /// Syncs the terminal clock to the workstation's local time
    /// (port of `updatetime`).
    pub async fn sync_time(&self, serial: &str) -> Result<String, sunmi_adb::ToolError> {
        let ts = crate::terminfo::timestamp_now();
        self.adb.systool(serial, &format!("set time {ts}")).await
    }

    /// Serials of devices currently in `device` state
    /// (port of `find_sunmitool_device`).
    pub async fn connected(&self) -> Result<Vec<String>, sunmi_adb::ToolError> {
        Ok(self
            .adb
            .devices()
            .await?
            .into_iter()
            .filter(|d| d.state == sunmi_adb::DeviceState::Device)
            .map(|d| d.serial)
            .collect())
    }

    /// Spawns one task per device and returns an event receiver. Each device
    /// executes the items sequentially; the first failure stops that device.
    pub fn run(
        &self,
        serials: Vec<String>,
        items: Arc<Vec<DownloadItem>>,
        options: DownloadOptions,
    ) -> mpsc::Receiver<DownloadEvent> {
        let (tx, rx) = mpsc::channel(64);
        let mut handles = Vec::with_capacity(serials.len());

        for serial in serials {
            let tx = tx.clone();
            let adb = self.adb.clone();
            let items = Arc::clone(&items);
            let options = options.clone();
            handles.push(tokio::spawn(run_device(adb, serial, items, options, tx)));
        }

        let finished_tx = tx.clone();
        tokio::spawn(async move {
            for handle in handles {
                let _ = handle.await;
            }
            let _ = finished_tx.send(DownloadEvent::Finished).await;
        });
        drop(tx);

        rx
    }
}

/// Runs one stop/timeout-guarded adb invocation against the device deadline.
macro_rules! guarded {
    ($options:expr, $deadline:expr, $fut:expr) => {
        tokio::select! {
            _ = $options.stop.cancelled() => Err(DownloadError::Stopped),
            r = tokio::time::timeout(
                $deadline.saturating_duration_since(Instant::now()),
                $fut,
            ) => match r {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(e)) => Err(e.into()),
                Err(_elapsed) => Err(DownloadError::Timeout),
            },
        }
    };
}

async fn run_device(
    adb: sunmi_adb::Adb,
    serial: String,
    items: Arc<Vec<DownloadItem>>,
    options: DownloadOptions,
    tx: mpsc::Sender<DownloadEvent>,
) {
    let deadline = Instant::now() + options.timeout;
    let mut ok = true;

    // Resolve the firmware's staging dir for pushed files. Hard failure
    // before any item -> DeviceFailed (the original aborted the device run).
    let tmp_dir = match guarded!(options, deadline, adb.version(&serial)) {
        Ok(output) => parse_systool_version(&output)
            .tmp_dir
            .unwrap_or_else(|| FALLBACK_TMP_DIR.to_string()),
        Err(error) => {
            let _ = tx
                .send(DownloadEvent::DeviceFailed {
                    serial: serial.clone(),
                    error: error.to_string(),
                })
                .await;
            let _ = tx
                .send(DownloadEvent::DeviceFinished {
                    serial,
                    ok: false,
                })
                .await;
            return;
        }
    };

    for item in items.iter() {
        if options.stop.is_cancelled() {
            let _ = tx
                .send(DownloadEvent::ItemFailed {
                    serial: serial.clone(),
                    item: item.clone(),
                    error: DownloadError::Stopped.to_string(),
                })
                .await;
            ok = false;
            break;
        }
        if Instant::now() >= deadline {
            let _ = tx
                .send(DownloadEvent::ItemFailed {
                    serial: serial.clone(),
                    item: item.clone(),
                    error: DownloadError::Timeout.to_string(),
                })
                .await;
            ok = false;
            break;
        }

        // Stage the local file on the device: the daemon reads paths on the
        // terminal itself, not PC paths (PC path -> ret=-3 FAILED).
        let file_name = item
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sunmi_file.bin");
        let remote_path = format!("{tmp_dir}/{file_name}");

        if let Err(error) = guarded!(options, deadline, adb.push(&serial, &item.path, &remote_path)) {
            let _ = tx
                .send(DownloadEvent::ItemFailed {
                    serial: serial.clone(),
                    item: item.clone(),
                    error: error.to_string(),
                })
                .await;
            ok = false;
            break;
        }

        let args = match item.systool_args_for(&remote_path) {
            Ok(args) => args,
            Err(error) => {
                let _ = tx
                    .send(DownloadEvent::ItemFailed {
                        serial: serial.clone(),
                        item: item.clone(),
                        error: error.to_string(),
                    })
                    .await;
                ok = false;
                break;
            }
        };

        let _ = tx
            .send(DownloadEvent::ItemStarted {
                serial: serial.clone(),
                item: item.clone(),
            })
            .await;

        let result = guarded!(options, deadline, adb.systool(&serial, &args))
            .and_then(|output| check_response(&output));

        match result {
            Ok(()) => {
                let _ = tx
                    .send(DownloadEvent::ItemCompleted {
                        serial: serial.clone(),
                        item: item.clone(),
                    })
                    .await;
            }
            Err(error) => {
                let _ = tx
                    .send(DownloadEvent::ItemFailed {
                        serial: serial.clone(),
                        item: item.clone(),
                        error: error.to_string(),
                    })
                    .await;
                ok = false;
                break;
            }
        }
    }

    let _ = tx
        .send(DownloadEvent::DeviceFinished { serial, ok })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systool_args_for_supported_kinds() {
        let app = DownloadItem::new(FileKind::App, "/tmp/app.apk");
        assert_eq!(
            app.systool_args().unwrap(),
            "install app \"/tmp/app.apk\""
        );
        let apn = DownloadItem::new(FileKind::Apn, "/tmp/carrier.apn");
        assert_eq!(
            apn.systool_args().unwrap(),
            "write apn \"/tmp/carrier.apn\""
        );
        let appcrp = DownloadItem::new(FileKind::AppCrp, "/tmp/res.zip");
        assert_eq!(
            appcrp.systool_args().unwrap(),
            "install arp \"/tmp/res.zip\""
        );
    }

    #[test]
    fn systool_args_rejects_unsupported_kinds() {
        let rom = DownloadItem::new(FileKind::Rom, "sunmiboot_scatter.txt");
        assert!(matches!(
            rom.systool_args(),
            Err(DownloadError::UnsupportedKind("Rom"))
        ));
    }

    #[test]
    fn parse_response_skips_adb_preamble() {
        let out = "* daemon not running; starting now\n\
                   * daemon started successfully\n\
                   {\"ret\":0,\"msg\":\"install ok\"}\n";
        let resp = parse_systool_response(out).unwrap();
        assert_eq!(resp.ret, 0);
        assert_eq!(resp.msg, "install ok");
    }

    #[test]
    fn parse_response_rejects_garbage() {
        assert!(matches!(
            parse_systool_response("ERROR: no systool service"),
            Err(DownloadError::InvalidOutput(_))
        ));
    }

    #[test]
    fn check_response_maps_ret() {
        assert!(check_response("{\"ret\":0,\"msg\":\"\"}").is_ok());
        assert!(matches!(
            check_response("{\"ret\":5,\"msg\":\"auth failed\"}"),
            Err(DownloadError::Systool { ret: 5, msg: _ })
        ));
    }

    #[test]
    fn parse_systool_version_extracts_fields() {
        let v = parse_systool_version(
            "systool_versioncode=1\nsystool_tmp_dir=/data/tmp/systool\n",
        );
        assert_eq!(v.version_code, Some(1));
        assert_eq!(v.tmp_dir.as_deref(), Some("/data/tmp/systool"));

        // P2-A11 4.0.69 reports an empty tmp dir -> fallback applies.
        let v = parse_systool_version("systool_versioncode=1\nsystool_tmp_dir=\n");
        assert_eq!(v.version_code, Some(1));
        assert_eq!(v.tmp_dir, None);
    }

    #[test]
    fn validate_checks_kind_and_existence() {
        let dir = std::env::temp_dir().join("sunmi-core-validate");
        std::fs::create_dir_all(&dir).unwrap();
        let apk = dir.join("app.apk");
        std::fs::write(&apk, b"x").unwrap();

        assert!(validate(&[DownloadItem::new(FileKind::App, &apk)]).is_ok());
        assert!(matches!(
            validate(&[DownloadItem::new(FileKind::App, dir.join("missing.apk"))]),
            Err(DownloadError::FileNotFound(_))
        ));
        assert!(matches!(
            validate(&[DownloadItem::new(FileKind::Ota, &apk)]),
            Err(DownloadError::UnsupportedKind("Ota"))
        ));
    }
}
