//! Terminal information (`systool get posinfo`) — port of the decompiled
//! `tminfo_msg` / `tminfo_item` and the display mapping from `util.cs`,
//! plus the battery/storage pre-checks used before downloads.

use serde::{Deserialize, Serialize};

use crate::download::DownloadError;

/// Port of `tminfo_msg`: `{"ret":0,"posinfo":{...}}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TmInfoMsg {
    pub ret: i64,
    #[serde(default)]
    pub posinfo: Option<TmInfo>,
}

/// Port of `tminfo_item`: all fields are optional — the firmware omits keys
/// depending on model.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TmInfo {
    #[serde(rename = "dev_sn")]
    pub dev_sn: Option<String>,
    #[serde(rename = "dev_model")]
    pub dev_model: Option<String>,
    #[serde(rename = "dev_devicecode")]
    pub dev_devicecode: Option<String>,
    #[serde(rename = "dev_hardware")]
    pub dev_hardware: Option<String>,
    #[serde(rename = "os_fw_ver")]
    pub os_fw_ver: Option<String>,
    #[serde(rename = "os_crp_ver")]
    pub os_crp_ver: Option<String>,
    #[serde(rename = "os_crp_custom_ver")]
    pub os_crp_custom_ver: Option<String>,
    #[serde(rename = "os_crp_common_ver")]
    pub os_crp_common_ver: Option<String>,
    #[serde(rename = "os_arp_ver")]
    pub os_arp_ver: Option<String>,
    #[serde(rename = "os_language")]
    pub os_language: Option<String>,
    #[serde(rename = "os_timezone")]
    pub os_timezone: Option<String>,
    #[serde(rename = "sec_appdebug")]
    pub sec_appdebug: Option<String>,
    #[serde(rename = "sec_fwdebug")]
    pub sec_fwdebug: Option<String>,
    #[serde(rename = "se_base_ver")]
    pub se_base_ver: Option<String>,
    #[serde(rename = "se_bootversion")]
    pub se_bootversion: Option<String>,
    #[serde(rename = "se_cfg_ver")]
    pub se_cfg_ver: Option<String>,
    #[serde(rename = "se_fw_ver")]
    pub se_fw_ver: Option<String>,
    #[serde(rename = "se_seclevel")]
    pub se_seclevel: Option<String>,
    #[serde(rename = "dev_efuse")]
    pub dev_efuse: Option<String>,
    #[serde(rename = "cfg_CFG_VER")]
    pub cfg_cfg_ver: Option<String>,
    #[serde(rename = "cfg_HARDWARE_VER")]
    pub cfg_hardware_ver: Option<String>,
    #[serde(rename = "misc_cert_state")]
    pub misc_cert_state: Option<String>,
    #[serde(rename = "misc_batcapacity")]
    pub misc_batcapacity: Option<String>,
    #[serde(rename = "misc_available_space")]
    pub misc_available_space: Option<String>,
}

/// Display mapping from `util.cs` (key -> human label), in the original
/// iteration order.
const LABELS: [(&str, &str); 22] = [
    ("dev_sn", "SN"),
    ("dev_model", "Model"),
    ("dev_devicecode", "PCI DevName"),
    ("dev_hardware", "Name"),
    ("os_fw_ver", "OS Version"),
    ("os_crp_ver", "CRP Version"),
    ("os_crp_custom_ver", "CRP Version"),
    ("os_crp_common_ver", "Common CRP Version"),
    ("os_arp_ver", "ARP Version"),
    ("os_language", "Language"),
    ("os_timezone", "TimeZone"),
    ("sec_appdebug", "App Debug Status"),
    ("sec_fwdebug", "FW Debug Status"),
    ("se_base_ver", "Libbase Version"),
    ("se_bootversion", "SP Boot Version"),
    ("se_cfg_ver", "SP Cfg Version"),
    ("se_fw_ver", "SP LiteOS Version"),
    ("se_seclevel", "SecLevel"),
    ("dev_efuse", "Efuse Status"),
    ("cfg_CFG_VER", "Cfg Version"),
    ("cfg_HARDWARE_VER", "PCI Hardware Ver"),
    ("misc_cert_state", "CA Level"),
];

impl TmInfo {
    fn field(&self, key: &str) -> Option<&str> {
        match key {
            "dev_sn" => self.dev_sn.as_deref(),
            "dev_model" => self.dev_model.as_deref(),
            "dev_devicecode" => self.dev_devicecode.as_deref(),
            "dev_hardware" => self.dev_hardware.as_deref(),
            "os_fw_ver" => self.os_fw_ver.as_deref(),
            "os_crp_ver" => self.os_crp_ver.as_deref(),
            "os_crp_custom_ver" => self.os_crp_custom_ver.as_deref(),
            "os_crp_common_ver" => self.os_crp_common_ver.as_deref(),
            "os_arp_ver" => self.os_arp_ver.as_deref(),
            "os_language" => self.os_language.as_deref(),
            "os_timezone" => self.os_timezone.as_deref(),
            "sec_appdebug" => self.sec_appdebug.as_deref(),
            "sec_fwdebug" => self.sec_fwdebug.as_deref(),
            "se_base_ver" => self.se_base_ver.as_deref(),
            "se_bootversion" => self.se_bootversion.as_deref(),
            "se_cfg_ver" => self.se_cfg_ver.as_deref(),
            "se_fw_ver" => self.se_fw_ver.as_deref(),
            "se_seclevel" => self.se_seclevel.as_deref(),
            "dev_efuse" => self.dev_efuse.as_deref(),
            "cfg_CFG_VER" => self.cfg_cfg_ver.as_deref(),
            "cfg_HARDWARE_VER" => self.cfg_hardware_ver.as_deref(),
            "misc_cert_state" => self.misc_cert_state.as_deref(),
            _ => None,
        }
    }

    /// `(label, value)` pairs, skipping empty fields (port of
    /// `display_tminfo`'s null/empty checks).
    pub fn display_rows(&self) -> Vec<(&'static str, String)> {
        LABELS
            .iter()
            .filter_map(|(key, label)| {
                let value = self.field(key)?;
                (!value.is_empty()).then(|| (*label, value.to_string()))
            })
            .collect()
    }

    /// Battery level in percent (port of the `int.Parse(misc_batcapacity)`
    /// pre-check; the original blocks OTA below 20%).
    pub fn battery_percent(&self) -> Option<i64> {
        self.misc_batcapacity.as_ref()?.trim().parse().ok()
    }

    /// Free space (MB) of the `data` segment reported by the firmware,
    /// e.g. `misc_available_space = "5G/16G data 3072/4096"` -> 3072
    /// (port of the `CheckDownload` storage check).
    pub fn data_free_space_mb(&self) -> Option<u64> {
        let raw = self.misc_available_space.as_ref()?;
        for segment in raw.split('/') {
            if !segment.contains("data") {
                continue;
            }
            let digits: String = segment.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
            if digits.is_empty() {
                return None;
            }
            return digits.parse().ok();
        }
        None
    }
}

/// Parses the `get posinfo` output, skipping adb preamble lines
/// (`* daemon started` etc.).
pub fn parse_tm_info(output: &str) -> Result<TmInfo, DownloadError> {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .ok_or_else(|| DownloadError::InvalidOutput(output.to_string()))?;

    let msg: TmInfoMsg = serde_json::from_str(line)
        .map_err(|_| DownloadError::InvalidOutput(line.to_string()))?;
    if msg.ret != 0 {
        return Err(DownloadError::Systool {
            ret: msg.ret,
            msg: String::new(),
        });
    }
    msg.posinfo
        .ok_or_else(|| DownloadError::InvalidOutput(line.to_string()))
}

/// `yyyy/MM/dd-HH:mm:ss` in local time — the format the firmware expects for
/// `systool set time` (the original used `DateTime.Now.ToString(...)`).
pub fn timestamp_now() -> String {
    chrono::Local::now().format("%Y/%m/%d-%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        r#"{"ret":0,"posinfo":{"dev_sn":"P2A1B2C3","dev_model":"P2","os_fw_ver":"3.1.0","#,
        r#""cfg_HARDWARE_VER":"V2.1","misc_batcapacity":"85","#,
        r#""misc_available_space":"data 3072/4096"}}"#
    );

    #[test]
    fn parses_sample_and_skips_preamble() {
        let out = format!("* daemon started successfully\n{SAMPLE}\n");
        let info = parse_tm_info(&out).unwrap();
        assert_eq!(info.dev_sn.as_deref(), Some("P2A1B2C3"));
        assert_eq!(info.cfg_hardware_ver.as_deref(), Some("V2.1"));
    }

    #[test]
    fn rejects_non_zero_ret() {
        let info = parse_tm_info(r#"{"ret":7,"msg":"denied"}"#);
        assert!(matches!(
            info,
            Err(DownloadError::Systool { ret: 7, msg: _ })
        ));
    }

    #[test]
    fn display_rows_skip_empty_fields() {
        let info = parse_tm_info(SAMPLE).unwrap();
        let rows = info.display_rows();
        assert!(rows.contains(&("SN", "P2A1B2C3".to_string())));
        assert!(rows.contains(&("PCI Hardware Ver", "V2.1".to_string())));
        // os_language is absent -> no row with that label+empty value
        assert!(rows.iter().all(|(label, value)| !value.is_empty()
            || *label != "Language"));
    }

    #[test]
    fn battery_and_storage_helpers() {
        let info = parse_tm_info(SAMPLE).unwrap();
        assert_eq!(info.battery_percent(), Some(85));
        assert_eq!(info.data_free_space_mb(), Some(3072));
    }

    #[test]
    fn battery_and_storage_absent() {
        let info = parse_tm_info(r#"{"ret":0,"posinfo":{"dev_sn":"x"}}"#).unwrap();
        assert_eq!(info.battery_percent(), None);
        assert_eq!(info.data_free_space_mb(), None);
    }

    #[test]
    fn timestamp_format_matches_firmware() {
        let ts = timestamp_now();
        assert_eq!(ts.len(), 19); // yyyy/MM/dd-HH:mm:ss
        let bytes = ts.as_bytes();
        assert_eq!(bytes[4], b'/');
        assert_eq!(bytes[7], b'/');
        assert_eq!(bytes[10], b'-');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
    }
}
