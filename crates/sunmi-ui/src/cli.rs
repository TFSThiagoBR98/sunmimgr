//! CLI flows kept for field testing (install/info/devices).

use std::path::PathBuf;
use std::sync::Arc;

use sunmi_adb::Adb;
use sunmi_core::download::{
    validate, DownloadEvent, DownloadItem, DownloadOptions, Downloader,
};
use sunmi_core::filetype::FileKind;

async fn adb_or_exit() -> Adb {
    let adb = Adb::auto().await;
    adb
}

pub async fn list_devices() {
    let adb = adb_or_exit().await;

    match adb.devices().await {
        Ok(devices) if devices.is_empty() => println!("No ADB devices found."),
        Ok(devices) => {
            println!("{} device(s):", devices.len());
            for device in &devices {
                println!("  {:<12} {:?}", device.serial, device.state);
            }

            for device in devices.iter().take(1) {
                match adb.systool(&device.serial, "get posinfo").await {
                    Ok(out) => println!("posinfo:\n{out}"),
                    Err(e) => eprintln!("systool get posinfo failed: {e}"),
                }
            }
        }
        Err(e) => {
            eprintln!("adb devices failed: {e}");
            std::process::exit(1);
        }
    }
}

/// `sunmi-ui info` — syncs the terminal clock and prints the `posinfo`
/// table (port of the original Refresh flow: `updatetime` + `get posinfo`).
pub async fn device_info() {
    let adb = adb_or_exit().await;
    let downloader = Downloader::new(adb);

    let serials = match downloader.connected().await {
        Ok(serials) => serials,
        Err(e) => {
            eprintln!("adb devices failed: {e}");
            std::process::exit(1);
        }
    };
    if serials.is_empty() {
        eprintln!("No devices in 'device' state.");
        std::process::exit(1);
    }

    for serial in &serials {
        match downloader.sync_time(serial).await {
            Ok(_) => println!("[{serial}] clock synced"),
            Err(e) => println!("[{serial}] clock sync failed: {e}"),
        }
        match downloader.term_info(serial).await {
            Ok(info) => {
                println!("[{serial}] terminal info:");
                for (label, value) in info.display_rows() {
                    println!("  {label:<18} {value}");
                }
                if let Some(battery) = info.battery_percent() {
                    println!("  {:<18} {battery}%", "Battery");
                }
                if let Some(free_mb) = info.data_free_space_mb() {
                    println!("  {:<18} {free_mb} MB", "Data free space");
                }
            }
            Err(e) => println!("[{serial}] get posinfo failed: {e}"),
        }
    }
}

/// `sunmi-ui install <file...>` — file kind is auto-detected
/// (App: .apk, Apn: .apn, AppCrp: other .zip) and installed on every
/// connected device.
pub async fn install_files(files: &[String]) {
    let adb = adb_or_exit().await;
    let downloader = Downloader::new(adb);

    let serials = match downloader.connected().await {
        Ok(serials) => serials,
        Err(e) => {
            eprintln!("adb devices failed: {e}");
            std::process::exit(1);
        }
    };
    if serials.is_empty() {
        eprintln!("No devices in 'device' state.");
        std::process::exit(1);
    }
    println!("Installing on {} device(s): {serials:?}", serials.len());

    let mut items = Vec::new();
    for file in files {
        let path = PathBuf::from(file).canonicalize().unwrap_or_else(|e| {
            eprintln!("File not found: {file} ({e})");
            std::process::exit(1);
        });
        let kind =
            FileKind::detect(path.file_name().and_then(|n| n.to_str()).unwrap_or(file))
                .unwrap_or_else(|| {
                    eprintln!("Unknown file type, please set it manually: {file}");
                    std::process::exit(1);
                });
        items.push(DownloadItem::new(kind, path));
    }

    if let Err(e) = validate(&items) {
        eprintln!("Invalid queue: {e}");
        std::process::exit(1);
    }

    let mut rx = downloader.run(serials, Arc::new(items), DownloadOptions::default());
    while let Some(event) = rx.recv().await {
        match event {
            DownloadEvent::ItemStarted { serial, item } => {
                println!("[{serial}] installing {} {:?} ...", item.kind, item.path);
            }
            DownloadEvent::ItemCompleted { serial, item } => {
                println!("[{serial}] done: {} {:?} ✅", item.kind, item.path);
            }
            DownloadEvent::ItemFailed {
                serial,
                item,
                error,
            } => {
                println!("[{serial}] failed: {} {:?}: {error}", item.kind, item.path);
            }
            DownloadEvent::DeviceFailed { serial, error } => {
                println!("[{serial}] device failed: {error}");
            }
            DownloadEvent::DeviceFinished { serial, ok } => {
                println!("[{serial}] finished ({})", if ok { "ok" } else { "failed" });
            }
            DownloadEvent::Finished => break,
        }
    }
}
