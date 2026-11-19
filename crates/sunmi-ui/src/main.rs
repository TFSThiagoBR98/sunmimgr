//! sunmi-ui — application crate.
//!
//! Today: CLI smoke test that proves the `sunmi-adb` backend wiring.
//! Next: GPUI window (dependency is commented out in Cargo.toml until the
//! Linux backend is validated).

use sunmi_adb::Adb;

#[tokio::main]
async fn main() {
    let Some(adb) = Adb::discover() else {
        eprintln!("'adb' not found in PATH. Install platform-tools or set PATH.");
        std::process::exit(1);
    };

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
