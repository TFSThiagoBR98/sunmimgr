use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("I/O error running {program}: {source}")]
    Io {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{program} exited with {status}: {stderr}")]
    Failed {
        program: String,
        status: ExitStatus,
        stderr: String,
    },
}

/// Runs a program and captures its stdout, requiring exit status 0.
///
/// Note: `adb devices` prints device states on stdout with exit 0 even when
/// a server restart is needed; callers must parse the output, not just the
/// exit code.
pub async fn run_program(program: &Path, args: &[&str]) -> Result<String, ToolError> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| ToolError::Io {
            program: program.display().to_string(),
            source: e,
        })?;

    if !output.status.success() {
        return Err(ToolError::Failed {
            program: program.display().to_string(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Finds a program by name on the system PATH (`.exe` suffix on Windows).
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}
