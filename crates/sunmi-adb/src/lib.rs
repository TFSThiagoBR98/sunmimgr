//! Device backends for Sunmi terminals.
//!
//! `Adb` is a dispatch enum over two backends sharing the same API:
//!
//! - `Spawned`: shells out to the `adb`/`fastboot` binaries from the
//!   system PATH (v1, battle-tested).
//! - `Native`: speaks the ADB protocol directly over USB via `nusb` (v2) —
//!   no external binary, real device-level control. Auth reuses
//!   `~/.android/adbkey` so already-authorized terminals connect silently.
//!
//! The Sunmi firmware exposes its privileged `systool` service through the
//! normal shell transport (`adb shell systool ...`); on production builds
//! generic shell is blocked but `shell:systool` stays whitelisted — both
//! backends work identically for the whitelist.
//!
//! Files are staged on the device before `systool install` runs:
//! `systool version` reports the staging dir (`systool_tmp_dir=...`), with
//! `/data/local/tmp` as fallback (validated on P2-A11 production).

pub mod adb;
pub mod fastboot;
pub mod native;
pub mod process;

pub use adb::{Adb, Device, DeviceState};
pub use fastboot::Fastboot;
pub use process::ToolError;
