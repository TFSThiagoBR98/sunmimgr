//! Domain models for managing Sunmi POS terminals.
//!
//! Ports of the decompiled SunmiDownloader (.NET) logic: file type
//! classification, the `download_config.json` / `*_script.json` /
//! `multi_package_config.json` schemas, and the mapping from file kinds to
//! `systool` commands (executed on the device by the Sunmi firmware daemon).

pub mod config;
pub mod filetype;
pub mod package;
pub mod script;
