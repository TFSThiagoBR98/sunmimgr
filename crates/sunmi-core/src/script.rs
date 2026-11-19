use serde::{Deserialize, Serialize};

/// `*_script.json` (port of the original `script_file`).
///
/// The file must live in the same folder as the APK files it installs and its
/// name must end with `script.json`. Execution order per device:
/// 1. `delete`  -> `systool remove app <name>`
/// 2. `install` -> `systool install app <folder>/<name>`
/// 3. `custom`  -> `systool <raw command>`
/// 4. `reboot`  -> `reboot` when true
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScriptFile {
    pub reboot: bool,
    #[serde(default)]
    pub delete: Vec<String>,
    #[serde(default)]
    pub install: Vec<String>,
    #[serde(default)]
    pub custom: Vec<String>,
}

/// One execution step, in the original execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptStep {
    Delete(String),
    Install(String),
    Custom(String),
    Reboot,
}

impl ScriptFile {
    pub fn steps(&self) -> impl Iterator<Item = ScriptStep> + '_ {
        self.delete
            .iter()
            .cloned()
            .map(ScriptStep::Delete)
            .chain(self.install.iter().cloned().map(ScriptStep::Install))
            .chain(self.custom.iter().cloned().map(ScriptStep::Custom))
            .chain(self.reboot.then_some(ScriptStep::Reboot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_original_order() {
        let script = ScriptFile {
            reboot: true,
            delete: vec!["com.a".into()],
            install: vec!["app.apk".into()],
            custom: vec!["get posinfo".into()],
        };
        assert_eq!(
            script.steps().collect::<Vec<_>>(),
            vec![
                ScriptStep::Delete("com.a".into()),
                ScriptStep::Install("app.apk".into()),
                ScriptStep::Custom("get posinfo".into()),
                ScriptStep::Reboot,
            ]
        );
    }
}
