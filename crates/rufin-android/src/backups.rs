use std::sync::Arc;

use rufin_core::runtime::{BackupHandle, BackupPreview};

use crate::host::{AndroidError, error};

#[derive(Clone, Copy, uniffi::Record)]
pub struct AndroidBackupContents {
    pub settings: bool,
    pub saved_logins: bool,
    pub playlists: bool,
    pub favorites: bool,
    pub local_imports: bool,
    pub activity: bool,
    pub queue: bool,
}

impl From<AndroidBackupContents> for backup::BackupContents {
    fn from(value: AndroidBackupContents) -> Self {
        Self {
            settings: value.settings,
            saved_logins: value.saved_logins,
            playlists: value.playlists,
            favorites: value.favorites,
            local_imports: value.local_imports,
            activity: value.activity,
            queue: value.queue,
        }
    }
}

impl From<backup::BackupContents> for AndroidBackupContents {
    fn from(value: backup::BackupContents) -> Self {
        Self {
            settings: value.settings,
            saved_logins: value.saved_logins,
            playlists: value.playlists,
            favorites: value.favorites,
            local_imports: value.local_imports,
            activity: value.activity,
            queue: value.queue,
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidBackupSummary {
    pub version: u32,
    pub created_at: i64,
    pub playlist_count: u64,
    pub available: AndroidBackupContents,
    pub selected: AndroidBackupContents,
    pub removed_sources: Vec<String>,
}

#[derive(uniffi::Object)]
pub struct AndroidBackupPreview {
    preview: BackupPreview,
}

#[uniffi::export]
impl AndroidBackupPreview {
    pub fn summary(&self) -> AndroidBackupSummary {
        let manifest = &self.preview.staged.manifest;
        AndroidBackupSummary {
            version: manifest.version,
            created_at: manifest.created_at,
            playlist_count: manifest.playlist_count,
            available: manifest.contents.into(),
            selected: self.preview.contents.into(),
            removed_sources: self.preview.removed_sources.clone(),
        }
    }
}

#[derive(uniffi::Record)]
pub struct AndroidRestoreReport {
    pub playlists: u64,
    pub playlist_entries: u64,
    pub skipped_playlist_entries: u64,
    pub smart_playlists: u64,
    pub user_states: u64,
    pub listens: u64,
    pub skipped_listens: u64,
    pub local_locators: u64,
    pub warnings: Vec<String>,
}

#[derive(uniffi::Object)]
pub struct AndroidBackups {
    handle: BackupHandle,
}

impl AndroidBackups {
    pub(crate) fn new(handle: BackupHandle) -> Self {
        Self { handle }
    }
}

#[uniffi::export]
impl AndroidBackups {
    pub async fn export_document(
        &self,
        uri: String,
        passphrase: Option<String>,
        contents: AndroidBackupContents,
    ) -> Result<(), AndroidError> {
        self.handle
            .export_document(uri, passphrase, contents.into())
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn stage_document(
        &self,
        uri: String,
        passphrase: Option<String>,
        contents: AndroidBackupContents,
    ) -> Result<Arc<AndroidBackupPreview>, AndroidError> {
        let preview = self
            .handle
            .stage_document(uri, passphrase, contents.into())
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(Arc::new(AndroidBackupPreview { preview }))
    }

    pub async fn restore(
        &self,
        preview: Arc<AndroidBackupPreview>,
    ) -> Result<AndroidRestoreReport, AndroidError> {
        let result = self
            .handle
            .restore(BackupPreview {
                staged: Arc::clone(&preview.preview.staged),
                removed_sources: preview.preview.removed_sources.clone(),
                contents: preview.preview.contents,
            })
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(AndroidRestoreReport {
            playlists: result.playlists,
            playlist_entries: result.playlist_entries,
            skipped_playlist_entries: result.skipped_playlist_entries,
            smart_playlists: result.smart_playlists,
            user_states: result.user_states,
            listens: result.listens,
            skipped_listens: result.skipped_listens,
            local_locators: result.local_locators,
            warnings: result.warnings,
        })
    }

    pub fn schedule_error(&self) -> Option<String> {
        self.handle.schedule_error()
    }

    pub async fn save_schedule_password(&self, passphrase: String) -> Result<(), AndroidError> {
        self.handle
            .save_schedule_password(passphrase)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
}
