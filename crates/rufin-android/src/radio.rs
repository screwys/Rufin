use std::{
    future::Future,
    sync::{Arc, Mutex},
};

use rufin_core::{
    playback::PlaybackOwner,
    playlists::PlaylistArtworkEditing,
    route::Route,
    runtime::{ProductHandles, SelectedLibrary, SourceHandle},
    source::feed::Changed,
    stations::{self, RadioPage, StationFound, StationInput, StationProbe},
    view::{Refresh, refresh_unread},
};

use crate::host::{AndroidError, error};

/// The Radio page: core's station operations, and the page's place in the change feed.
#[derive(uniffi::Object)]
pub struct AndroidRadio {
    source: SourceHandle,
    playback: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    /// The library the page was last read in.
    opened_with: Mutex<Option<SelectedLibrary>>,
}

impl AndroidRadio {
    pub(crate) fn new(products: &ProductHandles) -> Self {
        Self {
            source: products.source.clone(),
            playback: products.playback.clone(),
            runtime: products.runtime.clone(),
            opened_with: Mutex::default(),
        }
    }

    /// Runs a station operation on Rufin's runtime: its requests and timers need one, and
    /// Kotlin's coroutines poll outside it.
    async fn run<T: Send + 'static>(
        &self,
        operation: impl Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<T, AndroidError> {
        self.runtime
            .spawn(operation)
            .await
            .map_err(error)?
            .map_err(error)
    }
}

#[uniffi::export]
impl AndroidRadio {
    /// The page, read in the library selected now.
    pub async fn page(&self) -> Result<RadioPage, AndroidError> {
        *self.opened_with.lock().unwrap_or_else(|p| p.into_inner()) =
            self.source.selected_library();
        let (source, playback) = (self.source.clone(), self.playback.clone());
        self.run(async move { stations::page(&source, &playback).await })
            .await
    }

    /// What a committed change means for the page, as core decides it for every client.
    pub fn refresh(&self, changed: Changed) -> Refresh {
        let opened_with = self
            .opened_with
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        refresh_unread(
            &Route::Radio,
            opened_with.as_ref(),
            self.source.selected_library().as_ref(),
            &changed,
        )
    }

    /// Reads the server's stations into the Store.
    pub async fn read_server(&self) -> Result<(), AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::refresh(&source).await })
            .await
    }

    pub async fn play(
        &self,
        key: Option<i64>,
        listed_id: Option<String>,
        channel: Option<String>,
    ) -> Result<(), AndroidError> {
        let (source, playback) = (self.source.clone(), self.playback.clone());
        self.run(async move { stations::play(&source, &playback, key, listed_id, channel).await })
            .await
    }

    pub async fn add(&self, input: StationInput) -> Result<(), AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::add(&source, input).await })
            .await
    }

    pub async fn add_listed(&self, id: String) -> Result<(), AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::add_listed(&source, id).await })
            .await
    }

    /// The pictures Edit shows: the one chosen for the station, and the one it has without.
    pub async fn picture_editing(&self, key: i64) -> Result<PlaylistArtworkEditing, AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::picture_editing(&source, key).await })
            .await
    }

    /// `channel` names the channel that becomes the station's own; None leaves it. `picture`
    /// sets or takes away the picture chosen for the station; None leaves it.
    pub async fn edit(
        &self,
        key: i64,
        input: StationInput,
        channel: Option<String>,
        picture: Option<sources::ArtworkChange>,
    ) -> Result<(), AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::edit(&source, key, input, channel, picture).await })
            .await
    }

    /// Orders the page's stations as `keys` lists them.
    pub async fn reorder(&self, keys: Vec<i64>) -> Result<(), AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::reorder(&source, keys).await })
            .await
    }

    pub async fn delete(&self, key: i64) -> Result<(), AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::delete(&source, key).await })
            .await
    }

    pub async fn probe(&self, address: String) -> Result<StationProbe, AndroidError> {
        self.run(stations::probe(address)).await
    }

    pub async fn search(&self, query: String) -> Result<Vec<StationFound>, AndroidError> {
        let source = self.source.clone();
        self.run(async move { stations::search(&source, query).await })
            .await
    }
}
