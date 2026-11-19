//! Native sync push smoke test.
//! Usage: cargo run -p sunmi-adb --example native_push -- <local-file> <remote-path>

use sunmi_adb::native::services::NativeBackend;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (local, remote) = match (args.first(), args.get(1)) {
        (Some(l), Some(r)) => (l.clone(), r.clone()),
        _ => (std::env::temp_dir().join("probe.txt").display().to_string(), "/data/local/tmp/probe.txt".to_string()),
    };
    std::fs::write(&local, b"sunmimgr native push test\n").unwrap();

    let backend = NativeBackend::new();
    println!("devices: {:?}", backend.devices().await.unwrap());
    println!("push {local} -> {remote}");
    match backend.push(&backend.devices().await.unwrap()[0].serial, std::path::Path::new(&local), &remote).await {
        Ok(msg) => println!("push OK: {msg}"),
        Err(e) => println!("push FAILED: {e}"),
    }
}
