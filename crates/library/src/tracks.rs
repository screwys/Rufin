//! The full view of one track: its details, paths, CUE window and MBIDs, for the queue,
//! playback surfaces and editors. List rows are `TrackRow`s.

use sqlx::{FromRow, Sqlite};

use crate::{
    AlbumKey, ArtistKey, Catalog, GenreKey, LibraryError, LibraryResult, ReadCancellation,
    SourceKey, TrackKey,
};

#[derive(Clone, Debug, FromRow, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AudioProperties {
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub bit_depth: Option<u32>,
    pub channels: Option<u32>,
}

impl sqlx::Type<Sqlite> for AudioProperties {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }
}

impl<'row> sqlx::Decode<'row, Sqlite> for AudioProperties {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'row>) -> Result<Self, sqlx::error::BoxDynError> {
        let text = <&str as sqlx::Decode<Sqlite>>::decode(value)?;
        Ok(serde_json::from_str(text)?)
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackState {
    pub favorite: bool,
    pub rating: Option<i64>,
    pub last_played: Option<i64>,
    pub play_count: i64,
    pub skip_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackAccess {
    pub source_path: Option<String>,
    pub source_name: String,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub is_downloaded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackLinks {
    pub album_key: Option<AlbumKey>,
    pub album_media_uri: Option<String>,
    pub artists: Vec<TrackArtistLink>,
    pub album_artists: Vec<TrackArtistLink>,
    pub genres: Vec<TrackGenreLink>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackView {
    pub track_key: TrackKey,
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub object_id: Option<String>,
    pub media_uri: String,
    pub metadata: crate::TrackMetadata,
    pub state: TrackState,
    pub access: TrackAccess,
    pub links: TrackLinks,
    pub artwork_binding: Option<Vec<u8>>,
}

impl crate::TrackReference {
    pub fn direct(
        uri: impl Into<String>,
        title: impl Into<String>,
        artist: impl Into<String>,
        album: impl Into<String>,
        duration_millis: i64,
    ) -> Self {
        let title = title.into();
        Self {
            media_uri: uri.into(),
            metadata: crate::TrackMetadata {
                sort_text: title.to_lowercase(),
                title,
                artist: artist.into(),
                album: album.into(),
                duration_millis: duration_millis.max(0),
                ..Default::default()
            },
            artwork_binding: None,
        }
    }
}

impl From<TrackView> for crate::TrackReference {
    fn from(song: TrackView) -> Self {
        Self {
            media_uri: song.media_uri,
            metadata: song.metadata,
            artwork_binding: song.artwork_binding,
        }
    }
}

impl Catalog {
    pub async fn track_source_by_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<crate::SourceId>> {
        let mut connection = tokio::select! {
            result = self.acquire_reader() => result?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled)
        };
        let source = sqlx::query_scalar::<_, String>(
            "SELECT source.object_id FROM tracks track JOIN sources source USING(source_key)
             WHERE track.media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?;
        Ok(source.map(crate::SourceId::new))
    }
}
