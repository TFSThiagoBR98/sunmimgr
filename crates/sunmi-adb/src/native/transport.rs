//! USB transport for one ADB device: nusb bulk endpoints, CNXN/AUTH
//! handshake and a stream multiplexer (OPEN/OKAY/WRTE/CLSE).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nusb::transfer::Bulk;
use rsa::RsaPrivateKey;
use tokio::io::AsyncReadExt as _;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

use super::keys;
use super::protocol::{self, Message};
use crate::process::ToolError;

/// Endpoint addresses of the ADB interface.
#[derive(Debug, Clone)]
pub struct AdbInterfaceRef {
    pub serial: Option<String>,
    pub interface_number: u8,
}

enum StreamEvent {
    Data(Vec<u8>),
    Closed,
}

/// Public stream event consumed by service implementations.
#[derive(Debug)]
pub enum StreamEventData {
    Data(Vec<u8>),
    Closed,
}

impl From<StreamEvent> for StreamEventData {
    fn from(value: StreamEvent) -> Self {
        match value {
            StreamEvent::Data(d) => StreamEventData::Data(d),
            StreamEvent::Closed => StreamEventData::Closed,
        }
    }
}

struct StreamState {
    tx: mpsc::UnboundedSender<StreamEvent>,
    ready: Option<oneshot::Sender<Result<u32, String>>>,
}

pub struct Transport {
    pub serial: String,
    writer: AsyncMutex<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>,
    key: Arc<RsaPrivateKey>,
    streams: Mutex<HashMap<u32, StreamState>>,
    next_local: AtomicU32,
    max_payload: std::sync::atomic::AtomicUsize,
    lifetime: CancellationToken,
}

const HOST_MAX_PAYLOAD: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

impl Transport {
    /// Opens the USB device, claims the interface, performs the CNXN/AUTH
    /// handshake and spawns the reader task.
    pub async fn connect(r#ref: AdbInterfaceRef) -> Result<Arc<Self>, ToolError> {
        let device_info = nusb::list_devices()
            .await
            .map_err(|e| ToolError::Protocol(format!("usb enumeration failed: {e}")))?
            .find(|d| d.serial_number().as_deref() == r#ref.serial.as_deref())
            .ok_or_else(|| {
                ToolError::Protocol("device disappeared during connect".to_string())
            })?;
        let device = device_info
            .open()
            .await
            .map_err(|e| ToolError::Protocol(format!("usb open failed: {e}")))?;

        // Resolve the bulk endpoints of the ADB interface from the config
        // descriptors.
        let mut in_endpoint = None;
        let mut out_endpoint = None;
        'configs: for config in device.configurations() {
            for interface in config.interfaces() {
                if interface.interface_number() != r#ref.interface_number {
                    continue;
                }
                let alt = interface.first_alt_setting();
                for endpoint in alt.endpoints() {
                    match endpoint.direction() {
                        nusb::transfer::Direction::In => {
                            in_endpoint = Some(endpoint.address())
                        }
                        nusb::transfer::Direction::Out => {
                            out_endpoint = Some(endpoint.address())
                        }
                    }
                }
                break 'configs;
            }
        }
        let (Some(in_endpoint), Some(out_endpoint)) = (in_endpoint, out_endpoint) else {
            return Err(ToolError::Protocol(
                "adb interface has no bulk endpoints".to_string(),
            ));
        };

        let interface = device
            .claim_interface(r#ref.interface_number)
            .await
            .map_err(|e| ToolError::Protocol(format!(
                "usb claim failed (is the stock adb server holding the device?): {e}"
            )))?;

        let writer: Box<dyn tokio::io::AsyncWrite + Unpin + Send> =
            Box::new(interface.endpoint::<Bulk, nusb::transfer::Out>(out_endpoint)
                .map_err(|e| ToolError::Protocol(format!("usb out endpoint: {e}")))?
                .writer(4096));
        let reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> =
            Box::new(interface.endpoint::<Bulk, nusb::transfer::In>(in_endpoint)
                .map_err(|e| ToolError::Protocol(format!("usb in endpoint: {e}")))?
                .reader(4096));

        let key = Arc::new(keys::host_key().await?.clone());
        let serial = r#ref.serial.clone().unwrap_or_else(|| "unknown".to_string());

        let transport = Arc::new(Self {
            serial: serial.clone(),
            writer: AsyncMutex::new(writer),
            key,
            streams: Mutex::new(HashMap::new()),
            next_local: AtomicU32::new(1),
            max_payload: std::sync::atomic::AtomicUsize::new(4096),
            lifetime: CancellationToken::new(),
        });

        // CNXN handshake. The Sunmi production adbd closes the connection
        // when the CNXN carries a non-empty identity payload (it treats the
        // trailing bytes as the next message header), so send the empty
        // banner — exactly reproducing the working stock adb session above.
        // Reader task: routes incoming messages to streams / connection.
        let mut reader = reader;
        let rt = Arc::clone(&transport);
        let (connected_tx, connected_rx) = oneshot::channel::<Result<(), String>>();
        let connected_tx = Mutex::new(Some(connected_tx));
        tokio::spawn(async move {
            let mut first_auth_token = true;
            loop {
                let msg = tokio::select! {
                    _ = rt.lifetime.cancelled() => break,
                    m = protocol::read_message(&mut reader) => match m {
                        Ok(m) => m,
                        Err(e) => {
                            rt.fail_all(&e.to_string());
                            let Some(tx) = connected_tx.lock().unwrap().take() else {
                                break;
                            };
                            if tx.send(Err(e.to_string())).is_err() {
                                // The connect future was dropped (e.g.
                                // discovery timeout): exit and release USB.
                                break;
                            }
                            break;
                        }
                    }
                };
                match msg.command {
                    protocol::CNXN => {
                        rt.max_payload.store(
                            msg.arg1 as usize,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                        if let Some(tx) = connected_tx.lock().unwrap().take() {
                            if tx.send(Ok(())).is_err() {
                                // Connect future dropped: exit and release.
                                break;
                            }
                        }
                    }
                    protocol::AUTH if msg.arg0 == protocol::AUTH_TOKEN => {
                        // First token: offer the existing key signature. If
                        // the device is not yet authorized it asks again;
                        // the second round sends the public key so the user
                        // approval dialog can store it.
                        let (reply, arg0) = if first_auth_token {
                            first_auth_token = false;
                            (
                                keys::sign_token(&msg.data).await,
                                protocol::AUTH_SIGNATURE,
                            )
                        } else {
                            (
                                keys::public_key_line().await.map(|mut line| {
                                    line.push('\0');
                                    line.into_bytes()
                                }),
                                protocol::AUTH_RSAPUBLICKEY,
                            )
                        };
                        match reply {
                            Ok(data) => {
                                let _ = rt.send(arg0, 0, 0, &data).await;
                            }
                            Err(e) => {
                                rt.fail_all(&e.to_string());
                                let Some(tx) = connected_tx.lock().unwrap().take() else {
                                    break;
                                };
                                if tx.send(Err(e.to_string())).is_err() {
                                    break;
                                }
                                break;
                            }
                        }
                    }
                    protocol::OKAY => {
                        if let Some(state) = rt.streams.lock().unwrap().get_mut(&msg.arg1) {
                            if let Some(ready) = state.ready.take() {
                                let _ = ready.send(Ok(msg.arg0));
                            }
                        }
                    }
                    protocol::WRTE => {
                        if let Some(state) = rt.streams.lock().unwrap().get(&msg.arg1) {
                            let _ = state.tx.send(StreamEvent::Data(msg.data));
                        }
                    }
                    protocol::CLSE => {
                        if let Some(state) = rt.streams.lock().unwrap().remove(&msg.arg1) {
                            let _ = state.tx.send(StreamEvent::Closed);
                        }
                    }
                    _ => {}
                }
            }
        });

        // CNXN handshake — empty banner (see note above; the Sunmi adbd
        // rejects any non-empty identity payload with an endpoint fault).
        transport
            .send(protocol::CNXN, 0x01000001, HOST_MAX_PAYLOAD as u32, &[])
            .await?;

        let connected = tokio::time::timeout(CONNECT_TIMEOUT, connected_rx).await;
        match connected {
            Err(_) => {
                transport.lifetime.cancel();
                Err(ToolError::Protocol(
                    "adb handshake timeout (authorize this computer on the terminal)"
                        .to_string(),
                ))
            }
            Ok(Err(e)) => {
                transport.lifetime.cancel();
                Err(ToolError::Protocol(e.to_string()))
            }
            Ok(Ok(Err(e))) => {
                transport.lifetime.cancel();
                Err(ToolError::Protocol(e))
            }
            Ok(Ok(Ok(()))) => Ok(transport),
        }
    }

    async fn send(&self, command: u32, arg0: u32, arg1: u32, data: &[u8]) -> Result<(), ToolError> {
        let msg = Message::new(command, arg0, arg1, data.to_vec());
        let mut writer = self.writer.lock().await;
        protocol::write_message(&mut **writer, &msg)
            .await
            .map_err(|e| ToolError::Protocol(format!("usb write failed: {e}")))
    }

    fn fail_all(&self, reason: &str) {
        let mut streams = self.streams.lock().unwrap();
        for (_, state) in streams.drain() {
            let _ = state.tx.send(StreamEvent::Closed);
        }
        let _ = reason;
    }
    fn max_payload(&self) -> usize {
        let n = self.max_payload.load(Ordering::Relaxed);
        n.clamp(1, HOST_MAX_PAYLOAD)
    }

    /// Opens a service stream (`shell:...`, `sync:`, `reboot:...`).
    pub async fn open_stream(
        self: &Arc<Self>,
        service: &str,
    ) -> Result<StreamHandle, ToolError> {
        let local = self.next_local.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        self.streams.lock().unwrap().insert(
            local,
            StreamState {
                tx,
                ready: Some(ready_tx),
            },
        );

        self.send(protocol::OPEN, local, 0, service.as_bytes())
            .await?;

        let remote = match tokio::time::timeout(OPEN_TIMEOUT, ready_rx).await {
            Ok(Ok(Ok(remote))) => remote,
            Ok(Ok(Err(e))) => {
                self.streams.lock().unwrap().remove(&local);
                return Err(ToolError::Protocol(format!("stream open failed: {e}")));
            }
            Ok(Err(_)) | Err(_) => {
                self.streams.lock().unwrap().remove(&local);
                return Err(ToolError::Protocol(format!(
                    "stream open timed out for '{service}'"
                )));
            }
        };

        Ok(StreamHandle {
            transport: Arc::clone(self),
            local,
            remote,
            rx,
        })
    }
}

pub struct StreamHandle {
    transport: Arc<Transport>,
    pub local: u32,
    pub remote: u32,
    rx: mpsc::UnboundedReceiver<StreamEvent>,
}

impl StreamHandle {
    /// Sends data as WRTE messages split at the negotiated max payload.
    ///
    /// Host->device messages use `arg0 = host local id, arg1 = device id`
    /// (the device looks its socket up by `arg1`).
    pub async fn send_data(&self, data: &[u8]) -> Result<(), ToolError> {
        for chunk in data.chunks(self.transport.max_payload()) {
            self.transport
                .send(protocol::WRTE, self.local, self.remote, chunk)
                .await?;
        }
        Ok(())
    }

    /// Collects all stream data until the device closes the stream
    /// (non-interactive shell semantics).
    pub async fn read_all(&mut self) -> Result<Vec<u8>, ToolError> {
        let mut out = Vec::new();
        while let Some(event) = self.rx.recv().await {
            match event {
                StreamEvent::Data(data) => out.extend_from_slice(&data),
                StreamEvent::Closed => break,
            }
        }
        Ok(out)
    }

    /// Receives the next stream event (used by the sync protocol).
    pub async fn recv(&mut self) -> Option<StreamEventData> {
        self.rx.recv().await.map(StreamEventData::from)
    }

    pub async fn close(&self) {
        let _ = self
            .transport
            .send(protocol::CLSE, self.local, self.remote, &[])
            .await;
    }
}
