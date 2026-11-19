//! Sunmi-relevant ADB services over the native USB transport: shell
//! (`systool` forwarding), file push/pull via the `sync:` protocol and
//! reboot. Connections are cached per serial so repeated commands reuse
//! the same USB session.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::transport::{AdbInterfaceRef, StreamHandle, Transport};
use crate::process::ToolError;

#[derive(Clone, Default)]
pub struct NativeBackend {
    conns: Arc<Mutex<HashMap<String, Arc<Transport>>>>,
}

impl std::fmt::Debug for NativeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBackend").finish()
    }
}

/// Finds ADB interfaces (class 0xFF / subclass 0x42 / protocol 0x01). Bulk
/// endpoint addresses are resolved from the config descriptors later, after
/// the device is opened.
pub async fn list_adb_interfaces() -> Result<Vec<AdbInterfaceRef>, ToolError> {
    let mut out = Vec::new();
    for di in nusb::list_devices()
        .await
        .map_err(|e| ToolError::Protocol(format!("usb enumeration failed: {e}")))?
    {
        if di.interfaces().any(|i| {
            i.class() == 0xFF && i.subclass() == 0x42 && i.protocol() == 0x01
        }) {
            // An adb device can expose the ADB interface more than once
            // (adb + adb-unsecure alt modes); take the first match.
            if let Some(interface) = di
                .interfaces()
                .into_iter()
                .find(|i| i.class() == 0xFF && i.subclass() == 0x42 && i.protocol() == 0x01)
            {
                out.push(AdbInterfaceRef {
                    serial: di.serial_number().map(str::to_string),
                    interface_number: interface.interface_number(),
                });
            }
        }
    }
    Ok(out)
}

impl NativeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    async fn transport(&self, serial: &str) -> Result<Arc<Transport>, ToolError> {
        {
            let conns = self.conns.lock().await;
            if let Some(t) = conns.get(serial) {
                return Ok(Arc::clone(t));
            }
        }
        let refs = list_adb_interfaces().await?;
        let r#ref = refs
            .into_iter()
            .find(|r| r.serial.as_deref() == Some(serial))
            .ok_or_else(|| {
                ToolError::Protocol(format!("no ADB interface found for serial {serial}"))
            })?;
        let transport = Transport::connect(r#ref).await?;
        let mut conns = self.conns.lock().await;
        conns.insert(serial.to_string(), Arc::clone(&transport));
        Ok(transport)
    }

    /// Lists currently reachable devices. Each connection handshake is
    /// bounded so an unauthorized terminal does not stall discovery, and
    /// successful connections are cached for subsequent commands.
    pub async fn devices(&self) -> Result<Vec<crate::adb::Device>, ToolError> {
        let refs = list_adb_interfaces().await?;
        let mut conns = self.conns.lock().await;
        let mut devices = Vec::new();
        for r in refs {
            let Some(serial) = r.serial.clone() else { continue };
            if conns.contains_key(&serial) {
                devices.push(crate::adb::Device {
                    serial,
                    state: crate::adb::DeviceState::Device,
                });
                continue;
            }
            let connect = Transport::connect(AdbInterfaceRef {
                serial: Some(serial.clone()),
                interface_number: r.interface_number,
            });
            if let Ok(Ok(transport)) =
                tokio::time::timeout(std::time::Duration::from_secs(3), connect).await
            {
                conns.insert(serial.clone(), transport);
                devices.push(crate::adb::Device {
                    serial,
                    state: crate::adb::DeviceState::Device,
                });
            }
        }
        Ok(devices)
    }

    pub async fn shell(&self, serial: &str, command: &str) -> Result<String, ToolError> {
        let transport = self.transport(serial).await?;
        let mut stream = transport.open_stream(&format!("shell:{command}")).await?;
        let out = stream.read_all().await?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub async fn systool(&self, serial: &str, args: &str) -> Result<String, ToolError> {
        self.shell(serial, &format!("systool {args}")).await
    }

    pub async fn version(&self, serial: &str) -> Result<String, ToolError> {
        self.systool(serial, "version").await
    }

    pub async fn reboot_bootloader(&self, serial: &str) -> Result<String, ToolError> {
        let transport = self.transport(serial).await?;
        let mut stream = transport.open_stream("reboot:bootloader").await?;
        let out = stream.read_all().await?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// `adb push` via the `sync:` protocol (SEND/DATA/DONE).
    pub async fn push(
        &self,
        serial: &str,
        local: &Path,
        remote: &str,
    ) -> Result<String, ToolError> {
        let data = tokio::fs::read(local).await.map_err(|e| ToolError::Io {
            program: local.display().to_string(),
            source: e,
        })?;
        let transport = self.transport(serial).await?;
        let mut stream = transport.open_stream("sync:").await?;

        // SEND: "SEND" + u32 len + "<path>,<mode octal>"
        let send_payload = format!("{remote},644");
        let mut send_msg = Vec::with_capacity(8 + send_payload.len());
        send_msg.extend_from_slice(b"SEND");
        send_msg.extend_from_slice(&(send_payload.len() as u32).to_le_bytes());
        send_msg.extend_from_slice(send_payload.as_bytes());
        stream.send_data(&send_msg).await?;

        // DATA: "DATA" + u32 len + chunk (chunked WRTEs are handled below)
        const CHUNK: usize = 60 * 1024;
        let mut offset = 0usize;
        while offset < data.len() {
            let end = (offset + CHUNK).min(data.len());
            let chunk = &data[offset..end];
            let mut msg = Vec::with_capacity(8 + chunk.len());
            msg.extend_from_slice(b"DATA");
            msg.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            msg.extend_from_slice(chunk);
            stream.send_data(&msg).await?;
            offset = end;
        }

        // DONE: "DONE" + u32 mtime
        let mtime = std::fs::metadata(local)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32)
            .unwrap_or(0);
        let mut done = Vec::with_capacity(8);
        done.extend_from_slice(b"DONE");
        done.extend_from_slice(&mtime.to_le_bytes());
        stream.send_data(&done).await?;

        read_sync_status(&mut stream).await?;

        // QUIT then close.
        stream.send_data(b"QUIT\0\0\0\0").await?;
        stream.close().await;
        Ok(format!("pushed {} -> {remote}", local.display()))
    }

    /// `adb pull` via the `sync:` protocol (RECV -> DATA*/DONE).
    pub async fn pull(
        &self,
        serial: &str,
        remote: &str,
        local: &Path,
    ) -> Result<String, ToolError> {
        let transport = self.transport(serial).await?;
        let mut stream = transport.open_stream("sync:").await?;

        let mut recv_msg = Vec::with_capacity(8 + remote.len());
        recv_msg.extend_from_slice(b"RECV");
        recv_msg.extend_from_slice(&(remote.len() as u32).to_le_bytes());
        recv_msg.extend_from_slice(remote.as_bytes());
        stream.send_data(&recv_msg).await?;

        let mut content: Vec<u8> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        let mut done = false;
        while !done {
            let Some(bytes) = stream.recv().await else { break };
            match bytes {
                super::transport::StreamEventData::Data(d) => buf.extend_from_slice(&d),
                super::transport::StreamEventData::Closed => break,
            }
            loop {
                if buf.len() < 8 {
                    break;
                }
                let id = &buf[0..4];
                let len =
                    u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
                if buf.len() < 8 + len {
                    break;
                }
                match id {
                    b"DATA" => content.extend_from_slice(&buf[8..8 + len]),
                    b"DONE" => {
                        done = true;
                    }
                    b"FAIL" => {
                        let msg = String::from_utf8_lossy(&buf[8..8 + len]).into_owned();
                        stream.close().await;
                        return Err(ToolError::Protocol(format!("sync pull failed: {msg}")));
                    }
                    _ => {
                        stream.close().await;
                        return Err(ToolError::Protocol(format!(
                            "unexpected sync id {:?}",
                            String::from_utf8_lossy(id)
                        )));
                    }
                }
                buf.drain(..8 + len);
            }
        }
        stream.send_data(b"QUIT\0\0\0\0").await?;
        stream.close().await;

        tokio::fs::write(local, &content)
            .await
            .map_err(|e| ToolError::Io {
                program: local.display().to_string(),
                source: e,
            })?;
        Ok(format!(
            "pulled {remote} -> {} ({} bytes)",
            local.display(),
            content.len()
        ))
    }

    /// Drops cached USB sessions (the native equivalent of `kill-server`
    /// when another adb server may take over the device).
    pub async fn kill_server(&self) -> Result<String, ToolError> {
        self.conns.lock().await.clear();
        Ok("native sessions cleared".to_string())
    }
}

/// Reads the sync-level reply after DONE: `OKAY` + u32 len(0) or
/// `FAIL` + len + message. Works across fragmented WRTE packets.
async fn read_sync_status(stream: &mut StreamHandle) -> Result<(), ToolError> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        if buf.len() >= 4 {
            let id = &buf[0..4];
            match id {
                b"OKAY" => return Ok(()),
                b"FAIL" => {
                    if buf.len() >= 8 {
                        let len =
                            u32::from_le_bytes(buf[4..8].try_into().unwrap()) as usize;
                        if buf.len() >= 8 + len {
                            let msg = String::from_utf8_lossy(&buf[8..8 + len]);
                            return Err(ToolError::Protocol(format!(
                                "sync push failed: {msg}"
                            )));
                        }
                    }
                }
                _ => {}
            }
        }
        match stream.recv().await {
            Some(super::transport::StreamEventData::Data(d)) => buf.extend_from_slice(&d),
            Some(super::transport::StreamEventData::Closed) => {
                return Err(ToolError::Protocol("sync stream closed".to_string()));
            }
            None => return Err(ToolError::Protocol("sync stream ended".to_string())),
        }
    }
}
