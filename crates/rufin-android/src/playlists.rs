use std::sync::Arc;

use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};
use library::{PlaylistKey, PlaylistPathMode, SourceId};
use rufin_core::{playlist_files, playlists};

#[derive(uniffi::Record)]
pub struct AndroidPlaylistChoices {
    pub count: u64,
    pub rows: Vec<crate::browse::AndroidBrowseRow>,
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn playlist_choices(
        &self,
        filter: String,
        folder_kind: Option<bool>,
        offset: u64,
        limit: u32,
    ) -> Result<AndroidPlaylistChoices, AndroidError> {
        let selected = self.source.selected_library();
        let mut read = library::ListRead::new(
            if folder_kind == Some(true) {
                library::ListKind::SmartPlaylists {
                    now: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(error)?
                        .as_secs() as i64,
                }
            } else {
                library::ListKind::Playlists
            },
            selected.as_ref().map(|selected| selected.source_key),
            library::CatalogSort::Title,
        );
        read.folder = selected.and_then(|selected| selected.music_folder_key);
        read.filter = filter;
        read.playlist_scope = if folder_kind.is_some() {
            library::PlaylistScope::Folders
        } else {
            library::PlaylistScope::All
        };
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let cancel = library::ReadCancellation::new();
                let count = read.count(&database, &cancel).await.map_err(error)? as u64;
                let rows = read
                    .playlist_items(&database, offset as usize, limit as usize, &cancel)
                    .await
                    .map_err(error)?
                    .into_iter()
                    .map(Into::into)
                    .collect();
                Ok(AndroidPlaylistChoices { count, rows })
            })
            .await
            .map_err(error)?
    }

    pub async fn playlist_file_settings(
        &self,
        playlist: PlaylistKey,
    ) -> Result<playlist_files::PlaylistFileSettings, AndroidError> {
        playlist_files::settings(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn playlist_artwork_editing(
        &self,
        playlist: PlaylistKey,
    ) -> Result<playlists::PlaylistArtworkEditing, AndroidError> {
        playlists::playlist_artwork_editing(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn playlist_public(
        &self,
        playlist: PlaylistKey,
    ) -> Result<Option<bool>, AndroidError> {
        playlists::playlist_public(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn playlist_name(&self, playlist: PlaylistKey) -> Result<String, AndroidError> {
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                database
                    .playlists(&[playlist], &library::ReadCancellation::new())
                    .await
                    .map_err(error)?
                    .pop()
                    .map(|row| row.name)
                    .ok_or_else(|| error("Playlist no longer exists"))
            })
            .await
            .map_err(error)?
    }

    pub fn playlist_artwork_limit(&self) -> u64 {
        rufin_core::metadata::MAX_ARTWORK_BYTES as u64
    }

    pub async fn prepare_playlist_artwork(
        &self,
        bytes: Vec<u8>,
    ) -> Result<Arc<sources::ImageBytes>, AndroidError> {
        rufin_core::metadata::prepare_artwork(&self.source, bytes)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn regenerate_playlist_artwork(
        &self,
        playlist: PlaylistKey,
    ) -> Result<Option<Arc<sources::ImageBytes>>, AndroidError> {
        playlists::regenerate_playlist_artwork(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn update_playlist_file_options(
        &self,
        playlist: PlaylistKey,
        name: String,
        auto_refresh: bool,
        auto_save: Option<bool>,
        path_mode: PlaylistPathMode,
        artwork: Option<sources::ArtworkChange>,
    ) -> Result<(), AndroidError> {
        playlist_files::update(
            &self.source,
            playlist,
            name,
            auto_refresh,
            auto_save,
            path_mode,
            artwork,
        )
        .recv()
        .await
        .map_err(error)?
        .map_err(error)
    }

    pub async fn link_playlist_file(
        &self,
        playlist: PlaylistKey,
        source: Option<SourceId>,
        path: String,
        replace: bool,
    ) -> Result<(), AndroidError> {
        let result = match source {
            Some(source) => {
                playlist_files::link_source(&self.source, playlist, source, path, replace)
            }
            None if path.starts_with("content:") => {
                playlist_files::link_document(&self.source, playlist, path, replace)
            }
            None => playlist_files::link_local(&self.source, playlist, path.into(), replace),
        };
        result.recv().await.map_err(error)?.map_err(error)
    }

    pub async fn unlink_playlist_file(&self, playlist: PlaylistKey) -> Result<(), AndroidError> {
        playlist_files::unlink(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub async fn save_playlist_file(
        &self,
        playlist: PlaylistKey,
        replace: bool,
    ) -> Result<(), AndroidError> {
        playlist_files::save(&self.source, playlist, replace)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub async fn reload_playlist_file(
        &self,
        playlist: PlaylistKey,
        discard: bool,
    ) -> Result<(), AndroidError> {
        playlist_files::reload(&self.source, playlist, discard)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub async fn delete_playlist_file(&self, playlist: PlaylistKey) -> Result<(), AndroidError> {
        playlist_files::delete_file(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub async fn rename_playlist_file(
        &self,
        playlist: PlaylistKey,
        name: String,
    ) -> Result<(), AndroidError> {
        playlist_files::rename_file(&self.source, playlist, name)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }
    pub fn playlist_file_is_conflict(&self, message: String) -> bool {
        playlist_files::is_conflict(&message)
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn playlist_member_folder(
        &self,
        member: library::PlaylistFolderMember,
    ) -> Result<Option<library::PlaylistFolderKey>, AndroidError> {
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                database
                    .playlist_member_folder(&member, &library::ReadCancellation::new())
                    .await
                    .map_err(error)
            })
            .await
            .map_err(error)?
    }

    pub async fn save_smart_playlist(
        &self,
        key: Option<library::SmartPlaylistKey>,
        name: String,
        definition: library::SmartPlaylistDefinition,
        folder: Option<library::PlaylistFolderKey>,
    ) -> Result<library::SmartPlaylistKey, AndroidError> {
        let source = self.source.clone();
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                let key = if let Some(key) = key {
                    if !database
                        .update_smart_playlist(key, &name, &definition, None)
                        .await
                        .map_err(error)?
                    {
                        return Err(error("Playlist no longer exists"));
                    }
                    key
                } else {
                    database
                        .create_smart_playlist(&name, &definition)
                        .await
                        .map_err(error)?
                };
                if let Some(folder) = folder {
                    playlists::move_playlist_to_folder(
                        &source,
                        library::PlaylistFolderMember::SmartPlaylist { key },
                        Some(folder),
                    )
                    .await
                    .map_err(error)?;
                } else {
                    playlists::playlist_folders_changed(&source).await;
                }
                Ok(key)
            })
            .await
            .map_err(error)?
    }

    pub async fn delete_smart_playlist(
        &self,
        key: library::SmartPlaylistKey,
    ) -> Result<(), AndroidError> {
        let source = self.source.clone();
        let database = self.database.clone();
        self.runtime
            .spawn(async move {
                database.delete_smart_playlist(key).await.map_err(error)?;
                playlists::playlist_folders_changed(&source).await;
                Ok(())
            })
            .await
            .map_err(error)?
    }

    pub fn smart_playlist_default_definition(&self) -> library::SmartPlaylistDefinition {
        library::SmartPlaylistDefinition::default()
    }

    pub fn smart_playlist_rule_operators(
        &self,
        field: library::SmartPlaylistRuleField,
    ) -> Vec<library::SmartPlaylistRuleOperator> {
        field.operators().to_vec()
    }

    pub fn smart_playlist_rule(
        &self,
        field: library::SmartPlaylistRuleField,
        operator: Option<library::SmartPlaylistRuleOperator>,
    ) -> library::SmartPlaylistRule {
        let operator = operator.unwrap_or(field.operators()[0]);
        library::SmartPlaylistRule {
            field,
            operator,
            value: field.default_value(operator),
        }
    }
}
