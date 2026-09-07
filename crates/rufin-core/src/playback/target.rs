//! Explicit collection and queue operation inputs.
use library::{GenreKey, MoodKey, PlaylistKey, SmartPlaylistKey};
use std::sync::Arc;
#[derive(Clone, Debug, PartialEq)]
pub enum PlaybackTarget {
    Track(String),
    Album(String),
    AlbumKey(library::AlbumKey),
    Artist(String),
    AlbumArtist(String),
    ArtistKey(library::ArtistKey, bool),
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    SmartPlaylist(SmartPlaylistKey),
    Contextual {
        target: Box<PlaybackTarget>,
        context_id: String,
    },
}

impl PlaybackTarget {
    pub fn in_context(self, context_id: impl Into<String>) -> Self {
        Self::Contextual {
            target: Box::new(self),
            context_id: context_id.into(),
        }
    }

    pub fn context_id(&self) -> String {
        match self {
            Self::Track(id) => format!("track:{id}"),
            Self::Album(id) => format!("album:{id}"),
            Self::AlbumKey(id) => format!("album-key:{id}"),
            Self::Artist(id) => format!("artist:{id}"),
            Self::AlbumArtist(id) => format!("album-artist:{id}"),
            Self::ArtistKey(id, album_artist) => format!("artist-key:{id}:{album_artist}"),
            Self::Genre(id) => format!("genre:{id}"),
            Self::Mood(id) => format!("mood:{id}"),
            Self::Playlist(id) => format!("playlist:{id}"),
            Self::SmartPlaylist(id) => format!("smart-playlist:{id}"),
            Self::Contextual { context_id, .. } => context_id.clone(),
        }
    }

    pub fn play(
        &self,
        queue: &playback::QueueHandle,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
        placement: playback::QueuePlacement,
    ) {
        queue.play(playback::PlayRequest::ordered(
            self.queue_input(source, folder),
            0,
            placement,
            true,
        ));
    }

    pub async fn resolve_media_uris(
        &self,
        database: &library::Database,
        source_key: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> Result<Vec<String>, String> {
        let cancellation = library::ReadCancellation::new();
        let target = match self {
            Self::Contextual { target, .. } => target.as_ref(),
            target => target,
        };
        match (target, source_key) {
            (Self::AlbumKey(key), Some(source)) => database
                .album_track_route_page(
                    source,
                    *key,
                    folder,
                    "",
                    library::TrackSort::TrackNumber,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::ArtistKey(key, album_artist), Some(source)) => database
                .artist_track_route_page(
                    source,
                    *key,
                    *album_artist,
                    folder,
                    "",
                    library::TrackSort::Title,
                    false,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::Track(media_uri), _) => Ok(vec![media_uri.clone()]),
            (Self::Album(uri), _) => database
                .album_detail(uri, library::TrackSort::TrackNumber, false, &cancellation)
                .await
                .map(|detail| detail.map(|detail| detail.track_order).unwrap_or_default())
                .map_err(|error| error.to_string()),
            (Self::Artist(uri) | Self::AlbumArtist(uri), _) => {
                let Some(row) = database
                    .artist_row_by_media_uri(uri, &cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Ok(Vec::new());
                };
                database
                    .artist_track_route_page(
                        row.source_key,
                        row.artist_key,
                        matches!(target, Self::AlbumArtist(_)),
                        None,
                        "",
                        library::TrackSort::Title,
                        false,
                        false,
                        library::RouteSeedWindow::top(),
                        &cancellation,
                    )
                    .await
                    .map(|page| page.order)
                    .map_err(|error| error.to_string())
            }
            (Self::Genre(key), Some(source_key)) => database
                .genre_track_route_page(
                    source_key,
                    *key,
                    folder,
                    "",
                    library::TrackSort::Title,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::Mood(key), Some(source_key)) => database
                .mood_track_route_page(
                    source_key,
                    *key,
                    folder,
                    "",
                    library::TrackSort::Title,
                    false,
                    library::RouteSeedWindow::top(),
                    &cancellation,
                )
                .await
                .map(|page| page.order)
                .map_err(|error| error.to_string()),
            (Self::Playlist(key), _) => database
                .playlist_media_uri_order(*key, folder, &cancellation)
                .await
                .map_err(|error| error.to_string()),
            (Self::SmartPlaylist(key), _) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
                database
                    .smart_playlist_media_uri_order(source_key, *key, folder, now, &cancellation)
                    .await
                    .map_err(|error| error.to_string())
            }
            (Self::Contextual { .. }, _) => unreachable!(),
            (_, None) => Ok(Vec::new()),
        }
    }

    pub fn queue_input(
        &self,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> library::QueueInput {
        let target = match self {
            Self::Contextual { target, .. } => target.as_ref(),
            target => target,
        };
        let context_id = self.context_id().into();
        if let Self::SmartPlaylist(key) = target {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
            return library::QueueInput::Smart {
                key: *key,
                source,
                folder,
                now,
                context_id,
            };
        }
        let collection = match target {
            Self::Album(uri) => library::QueueCollection::Album(uri.clone()),
            Self::AlbumKey(key) => library::QueueCollection::AlbumKey(*key),
            Self::ArtistKey(key, album_artist) => library::QueueCollection::ArtistKey {
                key: *key,
                album_artist: *album_artist,
            },
            Self::Artist(uri) | Self::AlbumArtist(uri) => library::QueueCollection::Artist {
                media_uri: uri.clone(),
                album_artist: matches!(target, Self::AlbumArtist(_)),
            },
            Self::Genre(key) => library::QueueCollection::Genre(*key),
            Self::Mood(key) => library::QueueCollection::Mood(*key),
            Self::Playlist(key) => library::QueueCollection::Playlist(*key),
            Self::Track(uri) => {
                return library::QueueInput::Uris {
                    order: Arc::from([uri.clone()]),
                    context_id,
                    source_start: 0,
                };
            }
            Self::SmartPlaylist(_) | Self::Contextual { .. } => unreachable!(),
        };
        library::QueueInput::Collection {
            collection,
            folder,
            context_id,
        }
    }
}
