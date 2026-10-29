//! Track rows: the one full read of a track, by key. Orders, counts and the values computed
//! on read come from the resident Catalog; the rest of the row is read from the Store.

use sqlx::{FromRow, Sqlite, SqliteConnection};

use crate::catalog::snapshot::Packed;
use crate::catalog::views::{Art, Context, favorite, rating};
use crate::{
    AlbumKey, ArtistKey, Catalog, GenreKey, LibraryError, LibraryResult, ReadCancellation,
    SourceKey, TrackKey,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CatalogSort {
    Position,
    Title,
    TrackNumber,
    Artist,
    AlbumArtist,
    Album,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    UserRating,
    Genre,
    Bpm,
    Duration,
    Favorite,
    AlbumCount,
    TrackCount,
}

impl CatalogSort {
    pub fn activity_sensitive(self) -> bool {
        matches!(self, Self::PlayCount | Self::LastPlayed)
    }
}

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

pub type LoudnessAnalysisKey = [u8; 32];
#[cfg(feature = "bindings")]
uniffi::custom_type!(LoudnessAnalysisKey, Vec<u8>, {
    remote,
    lower: |value| value.to_vec(),
    try_lift: |value| value.try_into().map_err(|_| uniffi::deps::anyhow::anyhow!("Loudness key must contain 32 bytes"))
});

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackState {
    pub favorite: bool,
    pub rating: Option<i64>,
    pub last_played: Option<i64>,
    pub play_count: i64,
    pub skip_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackAccess {
    pub source_path: Option<String>,
    pub source_name: String,
    pub cue_path: Option<String>,
    pub cue_start_millis: Option<i64>,
    pub cue_end_millis: Option<i64>,
    pub is_downloaded: bool,
    #[sqlx(try_from = "Vec<u8>")]
    pub loudness_analysis_key: LoudnessAnalysisKey,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackLinks {
    pub album_key: Option<AlbumKey>,
    pub album_media_uri: Option<String>,
    #[sqlx(json)]
    pub artists: Vec<TrackArtistLink>,
    #[sqlx(json)]
    pub album_artists: Vec<TrackArtistLink>,
    #[sqlx(json)]
    pub genres: Vec<TrackGenreLink>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackView {
    pub track_key: TrackKey,
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub object_id: Option<String>,
    pub media_uri: String,
    #[sqlx(json)]
    pub metadata: crate::TrackMetadata,
    #[sqlx(flatten)]
    pub state: TrackState,
    #[sqlx(flatten)]
    pub access: TrackAccess,
    #[sqlx(flatten)]
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

/// The part of a track row the Catalog holds: state and the values computed on read.
struct Resident {
    state: TrackState,
    downloaded: bool,
    /// The row may name its album: the track is listed, or its album lists another track.
    album: bool,
    art: Art,
}

impl Catalog {
    fn resident_tracks(&self, keys: &[TrackKey]) -> Vec<Option<Resident>> {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let derived = self.covers(&context);
        keys.iter()
            .map(|key| {
                let slot = snapshot.tracks.slot(key.raw())?;
                let record = &snapshot.tracks.records[slot as usize];
                let played = context.played(slot, record);
                Some(Resident {
                    state: TrackState {
                        favorite: favorite(record.flags).unwrap_or(false),
                        rating: rating(record.rating, record.native_rating)
                            .map(|rating| i64::from(rating / 10)),
                        last_played: played.last.map(i64::from),
                        play_count: i64::from(played.plays),
                        skip_count: i64::from(played.skips),
                    },
                    downloaded: record.flags & crate::catalog::snapshot::DOWNLOADED != 0,
                    album: record.indexed()
                        || context
                            .album_of(record)
                            .is_some_and(|album| derived.albums[album as usize].tracks > 0),
                    art: context.track_art(&derived, record),
                })
            })
            .collect()
    }
}

/// Track rows by key, in the order asked. Keys the Store does not hold are skipped.
pub(crate) async fn load_track_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[TrackKey],
) -> LibraryResult<Vec<TrackView>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let resident = catalog.resident_tracks(keys);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        #[sqlx(flatten)]
        track: TrackView,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(track_rows_sql()))
        .bind(serde_json::to_string(keys)?)
        .fetch_all(&mut *connection)
        .await?;
    let mut tracks = Vec::with_capacity(rows.len());
    let mut arts = Vec::with_capacity(rows.len());
    for Row { ordinal, mut track } in rows {
        let Some(resident) = resident.get(ordinal as usize).and_then(Option::as_ref) else {
            arts.push(Art::None);
            tracks.push(track);
            continue;
        };
        track.state = resident.state.clone();
        track.access.is_downloaded = resident.downloaded;
        if !resident.album {
            track.links.album_key = None;
            track.links.album_media_uri = None;
        }
        arts.push(resident.art);
        tracks.push(track);
    }
    let bindings = crate::artwork::art_bindings(connection, &arts).await?;
    for (track, binding) in tracks.iter_mut().zip(bindings) {
        track.artwork_binding = binding;
    }
    Ok(tracks)
}

fn track_rows_sql() -> String {
    use crate::schema::{LINK_ARTIST, LINK_GENRE};
    let metadata = crate::metadata::track_metadata_sql("track", "details");
    format!(
        "SELECT CAST(requested.key AS INTEGER) ordinal,track.track_key,track.source_key,source.object_id source_id,track.object_id,track.media_uri,
        json_patch({metadata},json_object('title',coalesce(nullif(track.title,''),track.media_uri),
            'date_added',date(coalesce(unixepoch(track.date_added),track.first_seen_at),'unixepoch'),
            'album_display_artist',coalesce(nullif(album.display_artist,''),json_extract(details.metadata,'$.album_display_artist')),
            'musicbrainz_album_id',coalesce(json_extract(album.metadata,'$.musicbrainz_release_id'),json_extract(details.metadata,'$.musicbrainz_album_id')),
            'musicbrainz_release_group_id',coalesce(json_extract(album.metadata,'$.musicbrainz_release_group_id'),json_extract(details.metadata,'$.musicbrainz_release_group_id')),
            'primary_artist_musicbrainz_id',coalesce((SELECT artist.musicbrainz_artist_id FROM track_links credit JOIN artists artist ON artist.artist_key=credit.target WHERE credit.track_key=track.track_key AND credit.role={LINK_ARTIST} ORDER BY credit.position LIMIT 1),json_extract(details.metadata,'$.primary_artist_musicbrainz_id')))) metadata,
        0 favorite,NULL rating,NULL last_played,0 play_count,0 skip_count,
        CASE WHEN source.kind='local' THEN (SELECT access.source_path FROM local_track_access access WHERE access.track_key=track.track_key) ELSE details.source_path END source_path,
        coalesce(source.display_name,'') source_name,
        coalesce((SELECT access.cue_path FROM local_track_access access WHERE access.track_key=track.track_key),details.cue_path) cue_path,
        details.cue_start_millis,details.cue_end_millis,0 is_downloaded,coalesce(details.loudness_analysis_key,zeroblob(32)) loudness_analysis_key,
        track.album_key,album.media_uri album_media_uri,
        (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
            FROM track_links credit JOIN artists artist ON artist.artist_key=credit.target WHERE credit.track_key=track.track_key AND credit.role={LINK_ARTIST}) artists,
        (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
            FROM album_links credit JOIN artists artist ON artist.artist_key=credit.target WHERE credit.album_key=track.album_key AND credit.role={LINK_ARTIST}) album_artists,
        (SELECT json_group_array(json_object('genre_key',tag.tag_key,'name',tag.name) ORDER BY credit.position)
            FROM track_links credit JOIN tags tag ON tag.tag_key=credit.target WHERE credit.track_key=track.track_key AND credit.role={LINK_GENRE}) genres,
        NULL artwork_binding
        FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value
        LEFT JOIN track_details details ON details.track_key=track.track_key
        LEFT JOIN sources source ON source.source_key=track.source_key
        LEFT JOIN albums album ON album.album_key=track.album_key
        ORDER BY CAST(requested.key AS INTEGER)"
    )
}
