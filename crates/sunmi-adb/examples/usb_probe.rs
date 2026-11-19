//! USB probe: lists ADB-capable interfaces seen by nusb and performs a
//! step-by-step CNXN/AUTH handshake with message-level traces.
//! Usage: cargo run -p sunmi-adb --example usb_probe

use nusb::transfer::Bulk;

fn main_guard() {}
use sha1::Digest as _;
use sunmi_adb::native::keys;
use sunmi_adb::native::protocol::{self, Message};

#[tokio::main]
async fn main() {
    env_logger::init();
    let mut _keys_used = false; let Some(r#ref) = sunmi_adb::native::services::list_adb_interfaces()
        .await
        .unwrap()
        .into_iter()
        .next()
    else {
        println!("no adb interfaces found");
        return;
    };
    println!("adb interface: {:?}", r#ref);

    let di = nusb::list_devices()
        .await
        .unwrap()
        .find(|d| d.serial_number().as_deref() == r#ref.serial.as_deref())
        .unwrap();
    let device = di.open().await.unwrap();

    // Port reset: recovers any device-side session state left by a killed
    // adb session. The device re-enumerates, so reopen afterwards.
    match device.reset().await {
        Ok(()) => println!("usb reset ok — re-enumerating"),
        Err(e) => println!("usb reset failed (continuing): {e}"),
    }
    drop(device);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let di = nusb::list_devices()
        .await
        .unwrap()
        .find(|d| d.serial_number().as_deref() == r#ref.serial.as_deref())
        .expect("device did not re-enumerate after reset");
    let device = di.open().await.unwrap();
    let interface = device.claim_interface(r#ref.interface_number).await.unwrap();
    // endpoint addresses
    let (mut in_ep, mut out_ep) = (None, None);
    for config in device.configurations() {
        for interface in config.interfaces() {
            if interface.interface_number() != r#ref.interface_number {
                continue;
            }
            for endpoint in interface.first_alt_setting().endpoints() {
                println!(
                    "endpoint addr={:#04x} dir={:?}",
                    endpoint.address(),
                    endpoint.direction()
                );
                match endpoint.direction() {
                    nusb::transfer::Direction::In => in_ep = Some(endpoint.address()),
                    nusb::transfer::Direction::Out => out_ep = Some(endpoint.address()),
                }
            }
        }
    }
    let (Some(in_ep), Some(out_ep)) = (in_ep, out_ep) else {
        println!("missing endpoints");
        return;
    };

    let variant = std::env::args().nth(1).unwrap_or_else(|| "modern".to_string());
    let (version, max, banner): (u32, u32, &str) = match variant.as_str() {
        "modern" => (0x01000001, 4096, "host::features=shell_v2,cmd,stat_v2,ls_v2,fixed_v2"),
        "oldver" => (0x01000000, 4096, "host::"),
        "empty" => (0x01000001, 4096, ""),
        "minbanner" => (0x01000001, 4096, "host::"),
        "bigmax" => (0x01000001, 1024 * 1024, "host::features="),
        other => panic!("unknown variant {other}"),
    };
    let mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> = Box::new(
        interface
            .endpoint::<Bulk, nusb::transfer::In>(in_ep)
            .unwrap()
            .reader(4096),
    );
    let mut writer: Box<dyn tokio::io::AsyncWrite + Unpin + Send> = Box::new(
        interface
            .endpoint::<Bulk, nusb::transfer::Out>(out_ep)
            .unwrap()
            .writer(4096),
    );

    // Empty banner: the Sunmi production adbd faults the endpoints when the
    // CNXN carries a non-empty identity payload.
    let banner: &str = "";
    let cnxn = Message::new(protocol::CNXN, version, max, banner.as_bytes().to_vec());
    println!("-> CNXN variant={variant} version={version:#x} max={max}");
    protocol::write_message(&mut writer, &cnxn).await.unwrap();

    for step in 0..12 {
        let msg = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            protocol::read_message(&mut reader),
        )
        .await
        {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => {
                println!("<- read error: {e}");
                return;
            }
            Err(_) => {
                println!("<- timeout");
                return;
            }
        };
        println!(
            "<- step {step}: cmd={:08x} arg0={} arg1={} len={} ({})",
            msg.command,
            msg.arg0,
            msg.arg1,
            msg.data.len(),
            String::from_utf8_lossy(&msg.data[..msg.data.len().min(60)])
        );

        match msg.command {
            protocol::AUTH if msg.arg0 == protocol::AUTH_TOKEN => {
                let key = keys::host_key().await.unwrap().clone();
                let params = rsa::pkcs1v15::Pkcs1v15Sign::new::<sha1::Sha1>();
                let digest = sha1::Sha1::digest(&msg.data);
                let sig = key.sign(params, &digest).unwrap();
                let reply = Message::new(protocol::AUTH, protocol::AUTH_SIGNATURE, 0, sig);
                println!("-> AUTH SIGNATURE ({} bytes)", reply.data.len());
                protocol::write_message(&mut writer, &reply).await.unwrap();
            }
            protocol::CNXN => {
                println!(
                    "!! CNXN received — device max={}, banner={:?}",
                    msg.arg1,
                    String::from_utf8_lossy(&msg.data)
                );
                let open = Message::new(
                    protocol::OPEN,
                    1,
                    0,
                    b"shell:systool set time 2026/11/19-00:40:00".to_vec(),
                );
                println!("-> OPEN local=1 shell:systool version");
                protocol::write_message(&mut writer, &open).await.unwrap();
            }
            protocol::AUTH if msg.arg0 == protocol::AUTH_TOKEN => {
                let key = keys::host_key().await.unwrap().clone();
                let params = rsa::pkcs1v15::Pkcs1v15Sign::new::<sha1::Sha1>();
                let digest = sha1::Sha1::digest(&msg.data);
                let sig = key.sign(params, &digest).unwrap();
                let reply = Message::new(protocol::AUTH, protocol::AUTH_SIGNATURE, 0, sig);
                println!("-> AUTH SIGNATURE ({} bytes)", reply.data.len());
                protocol::write_message(&mut writer, &reply).await.unwrap();
            }
            protocol::OKAY => {
                println!("!! OKAY remote={} local={}", msg.arg0, msg.arg1);
                let wrte = Message::new(protocol::WRTE, msg.arg0, msg.arg1, b"");
                protocol::write_message(&mut writer, &wrte).await.unwrap();
            }
            protocol::WRTE => {
                println!(
                    "!! WRTE: {}",
                    String::from_utf8_lossy(&msg.data)
                );
                return;
            }
            _ => {}
        }
    }
}