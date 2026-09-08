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
        match self.queue_input(source_key, folder) {
            library::QueueInput::Uris { order, .. } => Ok(order.to_vec()),
            library::QueueInput::Collection { collection, .. } => database
                .collection_media_uri_order(&collection, &cancellation)
                .await
                .map_err(|error| error.to_string()),
            library::QueueInput::Smart {
                key,
                source,
                folder,
                now,
                ..
            } => database
                .smart_playlist_media_uri_order(source, key, folder, now, &cancellation)
                .await
                .map_err(|error| error.to_string()),
            _ => {
                unreachable!("PlaybackTarget produces a track, collection, or smart playlist input")
            }
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
            folder: None,
            context_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_collections_keep_their_full_scope_without_current() {
        let targets = [
            PlaybackTarget::Album("album:uri".into()),
            PlaybackTarget::AlbumKey(library::AlbumKey::from_raw(2)),
            PlaybackTarget::Artist("artist:uri".into()),
            PlaybackTarget::AlbumArtist("artist:uri".into()),
            PlaybackTarget::ArtistKey(library::ArtistKey::from_raw(3), false),
            PlaybackTarget::ArtistKey(library::ArtistKey::from_raw(3), true),
            PlaybackTarget::Genre(GenreKey::from_raw(4)),
            PlaybackTarget::Mood(MoodKey::from_raw(5)),
            PlaybackTarget::Playlist(PlaylistKey::from_raw(-6)),
        ];
        for target in targets {
            let target = target.in_context("pinned");
            let expected = target.queue_input(None, None);
            for source in [None, Some(library::SourceKey::from_raw(99))] {
                let input = target.queue_input(source, Some(library::FolderKey::from_raw(100)));
                assert_eq!(input, expected);
                assert!(matches!(
                    input,
                    library::QueueInput::Collection { folder: None, .. }
                ));
            }
        }
    }

    #[test]
    fn smart_playlist_inputs_keep_definition_scope_inputs() {
        let key = SmartPlaylistKey::from_raw(1);
        let target = PlaybackTarget::SmartPlaylist(key).in_context("smart pin");
        for source in [None, Some(library::SourceKey::from_raw(99))] {
            let folder = Some(library::FolderKey::from_raw(100));
            let library::QueueInput::Smart {
                key: actual,
                source: actual_source,
                folder: actual_folder,
                context_id,
                ..
            } = target.queue_input(source, folder)
            else {
                panic!("expected smart input")
            };
            assert_eq!(actual, key);
            assert_eq!(actual_source, source);
            assert_eq!(actual_folder, folder);
            assert_eq!(context_id.as_ref(), "smart pin");
        }
    }
}
