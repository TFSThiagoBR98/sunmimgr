use std::path::Path;

use crate::process::{find_in_path, run_program, ToolError};

/// Standard fastboot client, used for the ROM flash flow (the original
/// `sunmiboot.exe` is a fork of fastboot; the protocol is standard).
#[derive(Debug, Clone)]
pub struct Fastboot {
    path: std::path::PathBuf,
}

impl Fastboot {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Looks for `fastboot` on the system PATH.
    pub fn discover() -> Option<Self> {
        find_in_path("fastboot").map(Self::new)
    }

    async fn run(&self, args: &[&str]) -> Result<String, ToolError> {
        run_program(&self.path, args).await
    }

    /// Serials of devices currently in fastboot mode, mirroring the original
    /// `find_sunmiboot_device`.
    pub async fn devices(&self) -> Result<Vec<String>, ToolError> {
        let out = self.run(&["devices"]).await?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let serial = parts.next()?;
                let state = parts.next()?;
                (state == "fastboot").then(|| serial.to_string())
            })
            .collect())
    }

    pub async fn flash(
        &self,
        serial: &str,
        partition: &str,
        file: &Path,
    ) -> Result<String, ToolError> {
        self.run(&[
            "-s",
            serial,
            "flash",
            partition,
            &file.display().to_string(),
        ])
        .await
    }

    pub async fn erase(&self, serial: &str, partition: &str) -> Result<String, ToolError> {
        self.run(&["-s", serial, "erase", partition]).await
    }

    pub async fn reboot(&self, serial: &str) -> Result<String, ToolError> {
        self.run(&["-s", serial, "reboot"]).await
    }
}
