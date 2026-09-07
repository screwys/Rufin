use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSettings {
    pub enabled: bool,
    pub retention_count: u32,
    pub schedule: backup::BackupSchedule,
    pub destination_uri: Option<String>,
    pub contents: backup::BackupContents,
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

pub struct BackupPreview {
    pub staged: Arc<backup::StagedBackup>,
    pub removed_sources: Vec<String>,
    pub contents: backup::BackupContents,
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
