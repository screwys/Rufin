use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct BackupSettings {
    pub enabled: bool,
    pub retention_count: u32,
    pub schedule: crate::backup::format::BackupSchedule,
    pub destination_uri: Option<String>,
    pub contents: crate::backup::format::BackupContents,
    pub encrypt: bool,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_count: 2,
            schedule: Default::default(),
            destination_uri: None,
            contents: Default::default(),
            encrypt: false,
        }
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct BackupPreview {
    pub staged: Arc<crate::backup::format::StagedBackup>,
    pub removed_sources: Vec<String>,
    pub contents: crate::backup::format::BackupContents,
}

pub type BackupHandle = Arc<crate::backup::BackupOwner>;

#[cfg(test)]
mod tests {
    use super::BackupSettings;

    #[test]
    fn backup_defaults_and_existing_settings_preserve_retention() {
        let defaults = BackupSettings::default();
        assert!(!defaults.enabled);
        assert_eq!(defaults.retention_count, 2);
        let existing: BackupSettings = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(existing.enabled);
        assert_eq!(existing.retention_count, 2);
        let configured = BackupSettings {
            retention_count: 7,
            ..defaults
        };
        let restored: BackupSettings =
            serde_json::from_str(&serde_json::to_string(&configured).unwrap()).unwrap();
        assert_eq!(restored.retention_count, 7);
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl BackupPreview {
    pub fn manifest(&self) -> crate::backup::format::BackupManifest {
        self.staged.manifest.clone()
    }
    pub fn removed_sources(&self) -> Vec<String> {
        self.removed_sources.clone()
    }
    pub fn selected_contents(&self) -> crate::backup::format::BackupContents {
        self.contents
    }
}
