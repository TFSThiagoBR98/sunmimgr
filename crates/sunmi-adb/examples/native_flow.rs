//! Replicates the CLI flow: NativeBackend::new() -> devices() -> shell().
//! Usage: cargo run -p sunmi-adb --example native_flow

use sunmi_adb::native::services::NativeBackend;

#[tokio::main]
async fn main() {
    let backend = NativeBackend::new();
    let devices = backend.devices().await.unwrap();
    println!("devices: {devices:?}");

    for device in &devices {
        let serial = &device.serial;
        println!("-> shell (set time) on {serial}");
        match backend.shell(serial, "systool set time 2026/11/19-00:42:00").await {
            Ok(out) => println!("<- shell: {out}"),
            Err(e) => println!("<- shell FAILED: {e}"),
        }
        println!("-> shell (posinfo) on {serial}");
        match backend.shell(serial, "systool get posinfo").await {
            Ok(out) => println!("<- posinfo: {} bytes", out.len()),
            Err(e) => println!("<- posinfo FAILED: {e}"),
        }
    }
}
