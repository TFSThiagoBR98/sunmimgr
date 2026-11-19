//! Device backends for Sunmi terminals.
//!
//! - `adb`: standard adb client wrapping the Sunmi firmware `systool`
//!   service (`adb -s <serial> shell systool ...`).
//! - `fastboot`: standard fastboot client for the ROM flash flow guided by
//!   `sunmiboot_scatter.txt`.
//!
//! v1 shells out to the `adb`/`fastboot` binaries from the system PATH.
//! v2 (planned): native protocol implementation over USB (`nusb`) with real
//! transfer progress.

pub mod adb;
pub mod fastboot;
mod process;

pub use adb::{Adb, Device, DeviceState};
pub use fastboot::Fastboot;
pub use process::ToolError;
