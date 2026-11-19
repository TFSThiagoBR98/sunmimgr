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

    /// `systool` arguments forwarded over `adb shell`
    /// (port of the switch in `thread_dt_withtool`).
    pub fn systool_args(&self) -> Result<String, DownloadError> {
        if !SUPPORTED_KINDS.contains(&self.kind) {
            return Err(DownloadError::UnsupportedKind(self.kind.label()));
        }
        self.kind
            .systool_command(&self.path.display().to_string())
            .ok_or(DownloadError::UnsupportedKind(self.kind.label()))
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

async fn run_device(
    adb: sunmi_adb::Adb,
    serial: String,
    items: Arc<Vec<DownloadItem>>,
    options: DownloadOptions,
    tx: mpsc::Sender<DownloadEvent>,
) {
    let deadline = Instant::now() + options.timeout;
    let mut ok = true;

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

        let args = match item.systool_args() {
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

        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = tokio::select! {
            _ = options.stop.cancelled() => Err(DownloadError::Stopped),
            r = tokio::time::timeout(remaining, adb.systool(&serial, &args)) => match r {
                Ok(Ok(output)) => check_response(&output),
                Ok(Err(e)) => Err(e.into()),
                Err(_elapsed) => Err(DownloadError::Timeout),
            },
        };

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
