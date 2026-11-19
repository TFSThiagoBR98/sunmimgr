//! Integration tests running the download flow against a fake `adb`
//! binary, covering the full device-task orchestration.

use std::path::PathBuf;
use std::sync::Arc;

use sunmi_adb::Adb;
use sunmi_core::download::{
    validate, DownloadEvent, DownloadItem, DownloadOptions, Downloader,
};
use sunmi_core::filetype::FileKind;
use tokio_util::sync::CancellationToken;

/// Writes an executable fake `adb` shell script and returns its path.
fn fake_adb(name: &str, systool_output: &'static str, posinfo_output: &'static str) -> PathBuf {
    let dir = std::env::temp_dir().join("sunmi-core-it").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake_adb.sh");
    let script = format!(
        "#!/bin/sh\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    *posinfo*) printf '%s\\n' '{posinfo_output}'; exit 0;;\n  esac\ndone\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    *systool*) printf '%s\\n' '{systool_output}'; exit 0;;\n  esac\ndone\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    *devices*) printf 'List of devices attached\\nFAKE01\\tdevice\\n'; exit 0;;\n  esac\ndone\nexit 0\n"
    );
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    path
}

fn temp_apk() -> PathBuf {
    let dir = std::env::temp_dir().join("sunmi-core-it");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("app.apk");
    std::fs::write(&path, b"apk").unwrap();
    path
}

async fn collect(mut rx: tokio::sync::mpsc::Receiver<DownloadEvent>) -> Vec<DownloadEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        let done = event == DownloadEvent::Finished;
        events.push(event);
        if done {
            break;
        }
    }
    events
}

#[tokio::test]
async fn happy_path_reports_completed_items() {
    let adb = Adb::new(fake_adb("happy", "{\"ret\":0,\"msg\":\"install ok\"}", "{\"ret\":0,\"posinfo\":{}}"));
    let downloader = Downloader::new(adb);

    let apk = temp_apk();
    let item = DownloadItem::new(FileKind::App, &apk);
    validate(&[item.clone()]).unwrap();

    let rx = downloader.run(
        vec!["FAKE01".into()],
        Arc::new(vec![item.clone()]),
        DownloadOptions::default(),
    );

    let events = collect(rx).await;
    assert!(events.contains(&DownloadEvent::ItemStarted {
        serial: "FAKE01".into(),
        item: item.clone(),
    }));
    assert!(events.contains(&DownloadEvent::ItemCompleted {
        serial: "FAKE01".into(),
        item: item.clone(),
    }));
    assert!(events.contains(&DownloadEvent::DeviceFinished {
        serial: "FAKE01".into(),
        ok: true,
    }));
    assert_eq!(events.last(), Some(&DownloadEvent::Finished));
}

#[tokio::test]
async fn firmware_error_fails_the_item_and_stops_the_device() {
    let adb = Adb::new(fake_adb("err5", "{\"ret\":5,\"msg\":\"auth failed\"}", "{\"ret\":0,\"posinfo\":{}}"));
    let downloader = Downloader::new(adb);

    let apk = temp_apk();
    let item = DownloadItem::new(FileKind::App, &apk);

    let rx = downloader.run(
        vec!["FAKE01".into()],
        Arc::new(vec![item.clone()]),
        DownloadOptions::default(),
    );

    let events = collect(rx).await;
    assert!(events.contains(&DownloadEvent::ItemFailed {
        serial: "FAKE01".into(),
        item: item.clone(),
        error: "systool returned error ret=5: auth failed".into(),
    }));
    assert!(events.contains(&DownloadEvent::DeviceFinished {
        serial: "FAKE01".into(),
        ok: false,
    }));
    assert!(!events.contains(&DownloadEvent::ItemCompleted {
        serial: "FAKE01".into(),
        item,
    }));
}

#[tokio::test]
async fn stop_cancels_before_items_run() {
    let adb = Adb::new(fake_adb("stop", "{\"ret\":0,\"msg\":\"install ok\"}", "{\"ret\":0,\"posinfo\":{}}"));
    let downloader = Downloader::new(adb);

    let apk = temp_apk();
    let item = DownloadItem::new(FileKind::App, &apk);

    let stop = CancellationToken::new();
    stop.cancel();

    let rx = downloader.run(
        vec!["FAKE01".into()],
        Arc::new(vec![item.clone()]),
        DownloadOptions {
            stop,
            ..Default::default()
        },
    );

    let events = collect(rx).await;
    assert!(events.contains(&DownloadEvent::ItemFailed {
        serial: "FAKE01".into(),
        item: item.clone(),
        error: "download stopped".into(),
    }));
    assert!(events.contains(&DownloadEvent::DeviceFinished {
        serial: "FAKE01".into(),
        ok: false,
    }));
}

#[tokio::test]
async fn term_info_parses_fake_posinfo() {
    let adb = Adb::new(fake_adb(
        "posinfo",
        "{\"ret\":0,\"msg\":\"ok\"}",
        "{\"ret\":0,\"posinfo\":{\"dev_sn\":\"P2A1B2C3\",\"misc_batcapacity\":\"85\",\"misc_available_space\":\"data 3072/4096\"}}",
    ));
    let downloader = Downloader::new(adb);

    let info = downloader.term_info("FAKE01").await.unwrap();
    assert_eq!(info.dev_sn.as_deref(), Some("P2A1B2C3"));
    assert_eq!(info.battery_percent(), Some(85));
    assert_eq!(info.data_free_space_mb(), Some(3072));
}

#[tokio::test]
async fn sync_time_sends_timestamp() {
    let adb = Adb::new(fake_adb(
        "settime",
        "{\"ret\":0,\"msg\":\"set ok\"}",
        "{\"ret\":0,\"posinfo\":{}}",
    ));
    let downloader = Downloader::new(adb);

    let out = downloader.sync_time("FAKE01").await.unwrap();
    assert_eq!(out, "{\"ret\":0,\"msg\":\"set ok\"}\n");
}
