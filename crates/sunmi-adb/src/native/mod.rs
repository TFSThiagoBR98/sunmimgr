//! Native ADB client (v2): speaks the ADB v1 protocol directly over USB via
//! `nusb` — no `adb` binary or server needed. Auth reuses the standard
//! `~/.android/adbkey` host key so already-authorized terminals connect
//! without re-approval.

pub mod keys;
pub mod protocol;
pub mod services;
pub mod transport;

pub use transport::StreamEventData;
