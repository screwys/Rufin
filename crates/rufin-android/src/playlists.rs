use std::sync::Arc;

use crate::{
    host::{AndroidError, error},
    library::AndroidLibrary,
};
use library::{PlaylistKey, PlaylistPathMode, SourceId};
use rufin_core::{playlist_files, playlists};

#[uniffi::export]
impl AndroidLibrary {
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
                library::CatalogQuery::keys(
                    library::CatalogKind::Playlist,
                    None,
                    (&[playlist]).iter().map(|key| key.raw()).collect(),
                )
                .rows(
                    &database,
                    0,
                    (&[playlist]).len(),
                    &library::ReadCancellation::new(),
                )
                .await
                .map(|rows| match rows {
                    library::CatalogRows::Playlists(rows) => rows,
                    _ => unreachable!("typed catalog projection"),
                })
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
