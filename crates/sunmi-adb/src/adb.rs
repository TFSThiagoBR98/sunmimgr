use std::path::Path;
use std::str::FromStr;

use crate::process::{find_in_path, run_program, ToolError};

/// Standard ADB client. The Sunmi firmware exposes its privileged `systool`
/// service through the normal `shell` transport, so no vendor fork of adb is
/// required.
#[derive(Debug, Clone)]
pub struct Adb {
    path: std::path::PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: DeviceState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Device,
    Offline,
    Unauthorized,
    Recovery,
    Bootloader,
    Other(String),
}

impl FromStr for DeviceState {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "device" => Self::Device,
            "offline" => Self::Offline,
            "unauthorized" => Self::Unauthorized,
            "recovery" => Self::Recovery,
            "bootloader" => Self::Bootloader,
            other => Self::Other(other.to_string()),
        })
    }
}

impl Adb {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Looks for `adb` on the system PATH.
    pub fn discover() -> Option<Self> {
        find_in_path("adb").map(Self::new)
    }

    async fn run(&self, args: &[&str]) -> Result<String, ToolError> {
        run_program(&self.path, args).await
    }

    /// Lists devices from `adb devices`, mirroring the original
    /// `find_sunmitool_device` (only `device` state is actionable).
    pub async fn devices(&self) -> Result<Vec<Device>, ToolError> {
        let out = self.run(&["devices"]).await?;
        Ok(parse_devices(&out))
    }

    /// The original tool kills any stock adb server before scanning so its
    /// fork owns the connection exclusively; with a standard client this is
    /// only needed when another server version is interfering.
    pub async fn kill_server(&self) -> Result<String, ToolError> {
        self.run(&["kill-server"]).await
    }

    /// Forwards the Sunmi firmware `systool` service:
    /// `adb -s <serial> shell systool <args>`.
    pub async fn systool(&self, serial: &str, args: &str) -> Result<String, ToolError> {
        self.run(&["-s", serial, "shell", "systool", args]).await
    }

    pub async fn shell(&self, serial: &str, command: &str) -> Result<String, ToolError> {
        self.run(&["-s", serial, "shell", command]).await
    }

    /// `adb -s <serial> push <local> <remote>`.
    pub async fn push(
        &self,
        serial: &str,
        local: &std::path::Path,
        remote: &str,
    ) -> Result<String, ToolError> {
        self.run(&[
            "-s",
            serial,
            "push",
            &local.display().to_string(),
            remote,
        ])
        .await
    }

    /// `systool version` — version info exchanged between the Sunmi PC tools
    /// and the firmware daemon (`systool_versioncode`, `systool_tmp_dir`).
    pub async fn version(&self, serial: &str) -> Result<String, ToolError> {
        self.run(&["-s", serial, "shell", "systool", "version"]).await
    }

    /// Port of `reboot_to_sunmiboot`.
    pub async fn reboot_bootloader(&self, serial: &str) -> Result<String, ToolError> {
        self.run(&["-s", serial, "reboot", "bootloader"]).await
    }

    /// `adb -s <serial> pull <remote> <local>` (used for `packlogs.zip`).
    pub async fn pull(
        &self,
        serial: &str,
        remote: &str,
        local: &Path,
    ) -> Result<String, ToolError> {
        self.run(&["-s", serial, "pull", remote, &local.display().to_string()])
            .await
    }
}

fn parse_devices(output: &str) -> Vec<Device> {
    output
        .lines()
        .skip_while(|line| !line.contains("List of devices attached"))
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;
            Some(Device {
                serial: serial.to_string(),
                state: DeviceState::from_str(state).ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices_output() {
        let out = "\
* daemon not running; starting now at tcp:5037
* daemon started successfully
List of devices attached
P2A12345\tdevice
P2B67890\toffline

";
        let devices = parse_devices(out);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "P2A12345");
        assert_eq!(devices[0].state, DeviceState::Device);
        assert_eq!(devices[1].state, DeviceState::Offline);
    }
}
