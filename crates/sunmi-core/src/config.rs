use serde::{Deserialize, Serialize};

/// One entry of the file list persisted in `download_config.json`
/// (port of the original `item_value`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemValue {
    pub select: bool,
    pub name: String,
    /// `FileKind` label, e.g. `"App"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Directory containing the file.
    #[serde(default)]
    pub folder: String,
}

/// Persistent tool state, stored as `download_config.json` next to the
/// binary (port of the original `downloader_config`).
///
/// Deliberate improvement over the original: paths are kept relative to the
/// config location instead of absolute Windows paths, so the config is
/// portable across machines and platforms.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadConfig {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub file: Vec<ItemValue>,
    /// Batch-add folder entries (original key is `Folder`).
    #[serde(rename = "Folder", default)]
    pub folder: Vec<ItemValue>,
    /// Path to a `*_script.json` loaded in the Script tab.
    #[serde(default)]
    pub script_file: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl DownloadConfig {
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), ConfigError> {
        let raw = serde_json::to_string_pretty(self)?;
        Ok(std::fs::write(path, raw)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_original_config_shape() {
        let raw = r#"{"index":0,"file":[{"select":true,"name":"app.apk","type":"App","folder":"/tmp"}],"Folder":[],"scriptFile":""}"#;
        let cfg: DownloadConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(cfg.file.len(), 1);
        assert_eq!(cfg.file[0].kind, "App");
        assert!(cfg.file[0].select);
        assert_eq!(cfg.script_file, "");
    }
}
