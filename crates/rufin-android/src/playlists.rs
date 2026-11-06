use std::sync::Arc;

use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};
use library::{PlaylistKey, PlaylistPathMode, SourceId};
use rufin_core::{playlist_files, playlists, settings::presentation};

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
            library::LibraryField::Title,
        );
        // Smart playlists count in the music folder; the folder does not filter playlists.
        let list = if folder_kind == Some(true) {
            rufin_core::route::Route::SmartPlaylists
        } else {
            rufin_core::route::Route::Playlists
        };
        read.folder = rufin_core::view::Scope::of(&list, library::SearchMode::Title)
            .folder(selected.as_ref());
        read.filter = filter;
        read.playlist_scope = if folder_kind.is_some() {
            library::PlaylistScope::Folders
        } else {
            library::PlaylistScope::All
        };
        let catalog = self.catalog.clone();
        self.runtime
            .spawn(async move {
                let cancel = library::ReadCancellation::new();
                let order = catalog
                    .order(&library::Read::List(read), &cancel)
                    .await
                    .map_err(error)?;
                let offset = usize::try_from(offset).map_err(error)?;
                let rows = catalog
                    .rows(&order, offset..offset + limit as usize, true, &cancel)
                    .await
                    .map_err(error)?
                    .into_iter()
                    .map(Into::into)
                    .collect();
                let count = order.len() as u64;
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

    /// Removes the playlist's tracks that are no longer available; returns how many went.
    pub async fn remove_unavailable_tracks(
        &self,
        playlist: PlaylistKey,
    ) -> Result<u64, AndroidError> {
        playlists::remove_unavailable_tracks(&self.source, playlist)
            .recv()
            .await
            .map_err(error)?
            .map(|removed| removed as u64)
            .map_err(error)
    }

    pub async fn playlist_name(&self, playlist: PlaylistKey) -> Result<String, AndroidError> {
        self.catalog
            .playlist_name(playlist)
            .await
            .ok_or_else(|| error("Playlist no longer exists"))
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
        auto_refresh: bool,
        auto_save: Option<bool>,
        path_mode: PlaylistPathMode,
    ) -> Result<(), AndroidError> {
        playlist_files::update(&self.source, playlist, auto_refresh, auto_save, path_mode)
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
    pub async fn save_smart_playlist(
        &self,
        key: Option<library::SmartPlaylistKey>,
        name: String,
        definition: library::SmartPlaylistDefinition,
        folder: Option<library::PlaylistFolderKey>,
    ) -> Result<library::SmartPlaylistKey, AndroidError> {
        let change = match key {
            Some(key) => playlists::SmartPlaylistChange::Update {
                key,
                name,
                definition,
                artwork: None,
            },
            None => playlists::SmartPlaylistChange::Create { name, definition },
        };
        playlists::apply_smart_playlist_change(&self.source, change, folder)
            .await
            .map_err(error)?
            .ok_or_else(|| error("Playlist no longer exists"))
    }

    pub async fn delete_smart_playlist(
        &self,
        key: library::SmartPlaylistKey,
    ) -> Result<(), AndroidError> {
        playlists::apply_smart_playlist_change(
            &self.source,
            playlists::SmartPlaylistChange::Delete(key),
            None,
        )
        .await
        .map_err(error)?;
        Ok(())
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

#[uniffi::export]
pub fn smart_rule_field_title(field: library::SmartPlaylistRuleField) -> String {
    localization::tr(presentation::smart_rule_field_title(field))
}

#[uniffi::export]
pub fn smart_operator_title(
    field: library::SmartPlaylistRuleField,
    operator: library::SmartPlaylistRuleOperator,
) -> String {
    localization::tr(presentation::smart_operator_title(field, operator))
}

#[uniffi::export]
pub fn smart_sort_title(field: library::SmartPlaylistSort) -> String {
    localization::tr(presentation::smart_sort_title(field))
}

#[uniffi::export]
pub fn smart_period_title(period: library::SmartPlaylistActivityPeriod) -> String {
    localization::tr(presentation::smart_period_title(period))
}
