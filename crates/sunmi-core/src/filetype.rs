use std::fmt;
use std::path::Path;
use std::str::FromStr;

/// Kinds of files the downloader supports.
///
/// Port of the original SunmiDownloader `filetype` enum
/// (`App`, `Preapp`, `So`, `Apn`, `Cert`, `Crp`, `AppCrp`, `Ota`,
/// `Package`, `Rom`, `AppPara`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// General application (`.apk`).
    App,
    /// Pre-installed app; survives factory reset, app data is lost.
    Preapp,
    /// Dynamic library (`.so`), loaded from `/cache/customer/lib/`.
    So,
    /// IoT card APN file (`.apn`).
    Apn,
    /// Client application signature verification public key (`.pem`).
    Cert,
    /// Customer resource kit (`.crp`).
    Crp,
    /// Application resource kit (zip bundle, Brazilian customers only).
    AppCrp,
    /// OTA differential or full package (`*_ota.zip`, `inc-*.zip`).
    Ota,
    /// Tool-generated bundle of other kinds (never Rom).
    Package,
    /// Full ROM image, flashed via fastboot guided by `sunmiboot_scatter.txt`.
    Rom,
    /// Application parameter file (needs the target app chosen by the user).
    AppPara,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown file kind: {0}")]
pub struct ParseKindError(String);

impl FileKind {
    pub const ALL: [FileKind; 11] = [
        FileKind::App,
        FileKind::Preapp,
        FileKind::So,
        FileKind::Apn,
        FileKind::Cert,
        FileKind::Crp,
        FileKind::AppCrp,
        FileKind::Ota,
        FileKind::Package,
        FileKind::Rom,
        FileKind::AppPara,
    ];

    /// Label exactly as serialized in the original tool's JSON files.
    pub const fn label(self) -> &'static str {
        match self {
            FileKind::App => "App",
            FileKind::Preapp => "Preapp",
            FileKind::So => "So",
            FileKind::Apn => "Apn",
            FileKind::Cert => "Cert",
            FileKind::Crp => "Crp",
            FileKind::AppCrp => "AppCrp",
            FileKind::Ota => "Ota",
            FileKind::Package => "Package",
            FileKind::Rom => "Rom",
            FileKind::AppPara => "AppPara",
        }
    }

    /// Port of the original `GuessFileType` heuristics. Returns `None` when
    /// the type cannot be inferred and the user must pick it manually (the
    /// original `SelectType` dialog).
    pub fn detect(file_name: &str) -> Option<FileKind> {
        let name = Path::new(file_name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(file_name);

        if has_ext(name, "apk") {
            return Some(FileKind::App);
        }
        if has_ext(name, "so") {
            return Some(FileKind::So);
        }
        if has_ext(name, "apn") {
            return Some(FileKind::Apn);
        }
        if has_ext(name, "pem") {
            return Some(FileKind::Cert);
        }
        if has_ext(name, "crp") {
            return Some(FileKind::Crp);
        }
        if has_ext(name, "zip") {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with("_ota.zip") || lower.starts_with("inc-") {
                return Some(FileKind::Ota);
            }
            if lower.contains("package") {
                return Some(FileKind::Package);
            }
            // Any other .zip is treated as an application resource bundle.
            return Some(FileKind::AppCrp);
        }
        if name.eq_ignore_ascii_case("sunmiboot_scatter.txt") {
            return Some(FileKind::Rom);
        }
        None
    }

    /// Port of the download loop in `thread_dt_withtool`: maps a file kind to
    /// the `systool` invocation forwarded over `adb shell`.
    ///
    /// Returns `None` for kinds handled by a different flow:
    /// - `Rom` -> fastboot scatter flow (`sunmi-adb::fastboot`)
    /// - `Package` -> unpacked first, then expanded per entry
    /// - `AppPara` -> `install app_param:<app> <file>`, needs the target app
    pub fn systool_command(self, file_path: &str) -> Option<String> {
        let quoted = format!("\"{file_path}\"");
        match self {
            FileKind::App => Some(format!("install app {quoted}")),
            FileKind::Preapp => Some(format!("install preinstall-apk {quoted}")),
            FileKind::So => Some(format!("install lib {quoted}")),
            FileKind::Apn => Some(format!("write apn {quoted}")),
            FileKind::Cert => Some(format!("install cert {quoted}")),
            FileKind::Crp => Some(format!("install crp {quoted}")),
            FileKind::AppCrp => Some(format!("install arp {quoted}")),
            FileKind::Ota => Some(format!("update otapackage {quoted}")),
            FileKind::Package | FileKind::Rom | FileKind::AppPara => None,
        }
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for FileKind {
    type Err = ParseKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FileKind::ALL
            .iter()
            .copied()
            .find(|k| k.label().eq_ignore_ascii_case(s))
            .ok_or_else(|| ParseKindError(s.to_string()))
    }
}

fn has_ext(file_name: &str, ext: &str) -> bool {
    Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_matches_original_heuristics() {
        assert_eq!(FileKind::detect("FlyPay2.1.2.0.apk"), Some(FileKind::App));
        assert_eq!(FileKind::detect("libsunmibase.so"), Some(FileKind::So));
        assert_eq!(FileKind::detect("carrier.APN"), Some(FileKind::Apn));
        assert_eq!(FileKind::detect("client.pem"), Some(FileKind::Cert));
        assert_eq!(FileKind::detect("res.CRP"), Some(FileKind::Crp));
        assert_eq!(FileKind::detect("full_ota.zip"), Some(FileKind::Ota));
        assert_eq!(FileKind::detect("inc-1234.zip"), Some(FileKind::Ota));
        assert_eq!(
            FileKind::detect("multi_app_package_20251008.zip"),
            Some(FileKind::Package)
        );
        assert_eq!(FileKind::detect("other.zip"), Some(FileKind::AppCrp));
        assert_eq!(
            FileKind::detect("sunmiboot_scatter.txt"),
            Some(FileKind::Rom)
        );
        assert_eq!(FileKind::detect("unknown.bin"), None);
    }

    #[test]
    fn systool_command_matches_original_strings() {
        assert_eq!(
            FileKind::App.systool_command("/tmp/a.apk").unwrap(),
            "install app \"/tmp/a.apk\""
        );
        assert_eq!(
            FileKind::Ota.systool_command("/tmp/x.zip").unwrap(),
            "update otapackage \"/tmp/x.zip\""
        );
        assert_eq!(FileKind::Rom.systool_command("x"), None);
    }

    #[test]
    fn round_trip_label() {
        for kind in FileKind::ALL {
            assert_eq!(kind.to_string().parse::<FileKind>().unwrap(), kind);
        }
    }
}
