use serde::{Deserialize, Serialize};

/// One entry of `multi_package_config.json` (port of the original
/// `pac_json`). A package zip contains the files plus this manifest; when
/// downloaded, the tool unzips it and installs each entry by its declared
/// type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub filename: String,
    /// `FileKind` label, e.g. `"App"`.
    pub filetype: String,
}

pub type MultiPackageConfig = Vec<PackageEntry>;

/// Parses `multi_package_config.json` from an unpacked package directory.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn load_manifest(
    dir: impl AsRef<std::path::Path>,
) -> Result<MultiPackageConfig, ManifestError> {
    let path = dir.as_ref().join("multi_package_config.json");
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_original_manifest_shape() {
        let raw = r#"[{"filename":"app.apk","filetype":"App"}]"#;
        let entries: MultiPackageConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].filename, "app.apk");
        assert_eq!(entries[0].filetype, "App");
    }
}
