//! Owns Album, Artist, Genre, Mood, and Folder orders, rows, details, and metadata writes.
//! Collection membership, filtering, and sorting are resolved in one Store operation.

use std::collections::BTreeMap;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult, MoodKey,
    ReadCancellation, RouteSeedWindow, SourceKey, TrackKey, TrackRoutePage, TrackRow, TrackSort,
    tracks::load_track_rows,
};

const COLLECTION_ROW_LIMIT: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CollectionIdentity {
    Album(String),
    Artist {
        media_uri: String,
        album_artist: bool,
    },
    Genre {
        source_id: crate::SourceId,
        object_id: String,
    },
    Mood {
        source_id: crate::SourceId,
        object_id: String,
    },
    Playlist {
        source_id: Option<crate::SourceId>,
        object_id: String,
        native: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CollectionSourceReference {
    pub collection: CollectionIdentity,
    pub folder: Option<(crate::SourceId, String)>,
}

pub(crate) async fn canonical_collection_on(
    connection: &mut SqliteConnection,
    collection: &crate::QueueCollection,
    folder: Option<FolderKey>,
) -> LibraryResult<Option<CollectionSourceReference>> {
    use crate::QueueCollection as C;
    let collection = match collection {
        C::Album(uri) => CollectionIdentity::Album(uri.clone()),
        C::Artist {
            media_uri,
            album_artist,
        } => CollectionIdentity::Artist {
            media_uri: media_uri.clone(),
            album_artist: *album_artist,
        },
        C::AlbumKey(key) => {
            let Some(uri) =
                sqlx::query_scalar::<_, String>("SELECT media_uri FROM albums WHERE album_key=?1")
                    .bind(key)
                    .fetch_optional(&mut *connection)
                    .await?
            else {
                return Ok(None);
            };
            CollectionIdentity::Album(uri)
        }
        C::ArtistKey { key, album_artist } => {
            let Some(media_uri) = sqlx::query_scalar::<_, String>(
                "SELECT media_uri FROM artists WHERE artist_key=?1",
            )
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
            else {
                return Ok(None);
            };
            CollectionIdentity::Artist {
                media_uri,
                album_artist: *album_artist,
            }
        }
        C::Genre(key) => {
            let Some((source,id))=sqlx::query_as::<_,(String,String)>("SELECT source.object_id,genre.object_id FROM genres genre JOIN sources source USING(source_key) WHERE genre.genre_key=?1").bind(key).fetch_optional(&mut *connection).await? else{return Ok(None);};
            CollectionIdentity::Genre {
                source_id: crate::SourceId::new(source),
                object_id: id,
            }
        }
        C::Mood(key) => {
            let Some((source,id))=sqlx::query_as::<_,(String,String)>("SELECT source.object_id,mood.object_id FROM moods mood JOIN sources source USING(source_key) WHERE mood.mood_key=?1").bind(key).fetch_optional(&mut *connection).await? else{return Ok(None);};
            CollectionIdentity::Mood {
                source_id: crate::SourceId::new(source),
                object_id: id,
            }
        }
        C::Playlist(key) => {
            let native = key.raw() < 0;
            let query = if native {
                "SELECT source.object_id,playlist.object_id FROM catalog.native_playlists playlist JOIN sources source USING(source_key) WHERE playlist.playlist_key=-?1"
            } else {
                "SELECT source.object_id,playlist.object_id FROM main.playlists playlist LEFT JOIN main.source_ids source USING(source_key) WHERE playlist.playlist_key=?1"
            };
            let Some((source, id)) = sqlx::query_as::<_, (Option<String>, String)>(query)
                .bind(key)
                .fetch_optional(&mut *connection)
                .await?
            else {
                return Ok(None);
            };
            CollectionIdentity::Playlist {
                source_id: source.map(crate::SourceId::new),
                object_id: id,
                native,
            }
        }
    };
    let folder = if let Some(folder) = folder {
        let Some((source,id))=sqlx::query_as::<_,(String,String)>("SELECT source.object_id,folder.object_id FROM folders folder JOIN sources source USING(source_key) WHERE folder.folder_key=?1").bind(folder).fetch_optional(connection).await? else{return Ok(None);};
        Some((crate::SourceId::new(source), id))
    } else {
        None
    };
    Ok(Some(CollectionSourceReference { collection, folder }))
}

pub(crate) async fn resolve_collection_reference(
    connection: &mut SqliteConnection,
    reference: &CollectionSourceReference,
) -> LibraryResult<Option<(crate::QueueCollection, Option<FolderKey>)>> {
    use crate::QueueCollection as C;
    let collection = match &reference.collection {
        CollectionIdentity::Album(uri) => C::Album(uri.clone()),
        CollectionIdentity::Artist {
            media_uri,
            album_artist,
        } => C::Artist {
            media_uri: media_uri.clone(),
            album_artist: *album_artist,
        },
        CollectionIdentity::Genre {
            source_id,
            object_id,
        } => {
            let Some(key)=sqlx::query_scalar::<_,GenreKey>("SELECT genre_key FROM genres JOIN sources USING(source_key) WHERE sources.object_id=?1 AND genres.object_id=?2").bind(source_id.as_str()).bind(object_id).fetch_optional(&mut *connection).await? else{return Ok(None);};
            C::Genre(key)
        }
        CollectionIdentity::Mood {
            source_id,
            object_id,
        } => {
            let Some(key)=sqlx::query_scalar::<_,MoodKey>("SELECT mood_key FROM moods JOIN sources USING(source_key) WHERE sources.object_id=?1 AND moods.object_id=?2").bind(source_id.as_str()).bind(object_id).fetch_optional(&mut *connection).await? else{return Ok(None);};
            C::Mood(key)
        }
        CollectionIdentity::Playlist {
            source_id,
            object_id,
            native,
        } => {
            let query = if *native {
                "SELECT -playlist.playlist_key FROM catalog.native_playlists playlist JOIN sources source USING(source_key) WHERE source.object_id IS ?1 AND playlist.object_id=?2"
            } else {
                "SELECT playlist.playlist_key FROM main.playlists playlist LEFT JOIN main.source_ids source USING(source_key) WHERE source.object_id IS ?1 AND playlist.object_id=?2 AND playlist.name IS NOT NULL"
            };
            let Some(key) = sqlx::query_scalar::<_, crate::PlaylistKey>(query)
                .bind(source_id.as_ref().map(crate::SourceId::as_str))
                .bind(object_id)
                .fetch_optional(&mut *connection)
                .await?
            else {
                return Ok(None);
            };
            C::Playlist(key)
        }
    };
    let folder = if let Some((source, id)) = &reference.folder {
        let Some(key)=sqlx::query_scalar::<_,FolderKey>("SELECT folder_key FROM folders JOIN sources USING(source_key) WHERE sources.object_id=?1 AND folders.object_id=?2").bind(source.as_str()).bind(id).fetch_optional(connection).await? else{return Ok(None);};
        Some(key)
    } else {
        None
    };
    Ok(Some((collection, folder)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlbumSort {
    Title,
    AlbumArtist,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    Rating,
    TrackCount,
    Duration,
    Favorite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtistSort {
    Title,
    AlbumCount,
    TrackCount,
    LastPlayed,
    PlayCount,
    Rating,
    Favorite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenreSort {
    Title,
    AlbumCount,
    TrackCount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoodSort {
    Title,
    TrackCount,
    Duration,
}

impl AlbumSort {
    const fn code(self) -> i64 {
        match self {
            Self::Title => 0,
            Self::AlbumArtist => 1,
            Self::Year => 2,
            Self::ReleaseDate => 3,
            Self::DateAdded => 4,
            Self::LastPlayed => 5,
            Self::PlayCount => 6,
            Self::Rating => 7,
            Self::TrackCount => 8,
            Self::Duration => 9,
            Self::Favorite => 10,
        }
    }
}
impl ArtistSort {
    const fn code(self) -> i64 {
        match self {
            Self::Title => 0,
            Self::AlbumCount => 1,
            Self::TrackCount => 2,
            Self::LastPlayed => 3,
            Self::PlayCount => 4,
            Self::Rating => 5,
            Self::Favorite => 6,
        }
    }
}
impl GenreSort {
    const fn code(self) -> i64 {
        match self {
            Self::Title => 0,
            Self::AlbumCount => 1,
            Self::TrackCount => 2,
        }
    }
}
impl MoodSort {
    const fn code(self) -> i64 {
        match self {
            Self::Title => 0,
            Self::TrackCount => 1,
            Self::Duration => 2,
        }
    }
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct AlbumArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct AlbumGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlbumReleaseClass {
    Album,
    Ep,
    Single,
    Collection,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlbumReleaseClassification {
    pub album_key: AlbumKey,
    pub class: AlbumReleaseClass,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumRow {
    pub album_key: AlbumKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    pub title: String,
    pub display_artist: String,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub is_compilation: Option<bool>,
    pub release_lookup_identity: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub play_count: i64,
    pub last_played: Option<i64>,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub album_artists: Vec<AlbumArtistLink>,
    pub genres: Vec<AlbumGenreLink>,
    pub release_types: Vec<String>,
}

#[derive(FromRow)]
struct AlbumScalar {
    album_key: AlbumKey,
    source_key: SourceKey,
    object_id: String,
    media_uri: String,
    title: String,
    display_artist: String,
    year: Option<i64>,
    release_date: Option<String>,
    date_added: Option<String>,
    musicbrainz_release_id: Option<String>,
    musicbrainz_release_group_id: Option<String>,
    is_compilation: Option<bool>,
    release_lookup_identity: Option<String>,
    artwork_binding: Option<Vec<u8>>,
    favorite: bool,
    rating: Option<i64>,
    play_count: i64,
    last_played: Option<i64>,
    track_count: i64,
    duration_millis: i64,
    downloaded_count: i64,
}

#[derive(FromRow)]
struct AlbumArtistRelation {
    album_key: AlbumKey,
    artist_key: ArtistKey,
    media_uri: String,
    name: String,
}

#[derive(FromRow)]
struct AlbumGenreRelation {
    album_key: AlbumKey,
    genre_key: GenreKey,
    name: String,
}

#[derive(FromRow)]
struct AlbumReleaseTypeRelation {
    album_key: AlbumKey,
    release_type: String,
}

#[derive(FromRow)]
struct ArtistFactsRow {
    artist_key: ArtistKey,
    album_count: i64,
    track_count: i64,
    duration_millis: i64,
    play_count: i64,
    last_played: Option<i64>,
    downloaded_count: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ArtistRow {
    pub artist_key: ArtistKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    pub name: String,
    pub musicbrainz_artist_id: Option<String>,
    pub artwork_binding: Option<Vec<u8>>,
    pub favorite: bool,
    pub rating: Option<i64>,
    pub play_count: i64,
    pub last_played: Option<i64>,
    pub album_count: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenreRow {
    pub genre_key: GenreKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub album_count: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub representative_artwork: Vec<Vec<u8>>,
}

impl<'row> FromRow<'row, SqliteRow> for GenreRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            genre_key: row.try_get("genre_key")?,
            source_key: row.try_get("source_key")?,
            object_id: row.try_get("object_id")?,
            name: row.try_get("name")?,
            artwork_binding: row.try_get("artwork_binding")?,
            album_count: row.try_get("album_count")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
            downloaded_count: row.try_get("downloaded_count")?,
            representative_artwork: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MoodRow {
    pub mood_key: MoodKey,
    pub source_key: SourceKey,
    pub name: String,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub representative_artwork: Vec<Vec<u8>>,
}

impl<'row> FromRow<'row, SqliteRow> for MoodRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            mood_key: row.try_get("mood_key")?,
            source_key: row.try_get("source_key")?,
            name: row.try_get("name")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
            downloaded_count: row.try_get("downloaded_count")?,
            representative_artwork: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct FolderRow {
    pub folder_key: FolderKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumDetail {
    pub album: AlbumRow,
    pub track_order: Vec<String>,
    pub artists: Vec<ArtistKey>,
    pub genres: Vec<GenreKey>,
    pub release_types: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtistDetail {
    pub artist: ArtistRow,
    pub representative_albums: Vec<AlbumKey>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenreDetail {
    pub genre: GenreRow,
    pub representative_albums: Vec<AlbumKey>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MoodDetail {
    pub mood: MoodRow,
    pub representative_albums: Vec<AlbumKey>,
}

fn album_release_class(compilation: Option<bool>, release_types: &str) -> AlbumReleaseClass {
    if compilation == Some(true) {
        return AlbumReleaseClass::Collection;
    }
    let types = release_types
        .split('|')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if types.is_empty() || types.contains(&"album") {
        AlbumReleaseClass::Album
    } else if types.iter().any(|value| {
        matches!(
            *value,
            "compilation" | "compilations" | "collection" | "collections"
        )
    }) {
        AlbumReleaseClass::Collection
    } else if types.iter().any(|value| matches!(*value, "ep" | "e.p.")) {
        AlbumReleaseClass::Ep
    } else if types.contains(&"single") {
        AlbumReleaseClass::Single
    } else {
        AlbumReleaseClass::Other
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumMetadataWrite {
    pub title: String,
    pub normalized_title: String,
    pub display_artist: String,
    pub sort_text: String,
    pub year: Option<i64>,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub is_compilation: Option<bool>,
}

#[derive(Clone, Debug, Eq, FromRow, PartialEq)]
pub struct AlbumReleaseCandidate {
    pub album_key: AlbumKey,
    pub media_uri: String,
    pub lookup_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlbumReleaseResult {
    Found { release_types: Vec<String> },
    Missing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtistMetadataWrite {
    pub name: String,
    pub normalized_name: String,
    pub sort_text: String,
    pub musicbrainz_artist_id: Option<String>,
}

fn push_artist_role_scope(
    query: &mut QueryBuilder<Sqlite>,
    album_artist: bool,
    folder: Option<FolderKey>,
) {
    query.push(" AND ((").push_bind(!album_artist).push(" AND EXISTS (SELECT 1 FROM track_artists role WHERE role.artist_key=artist.artist_key)) OR (").push_bind(album_artist).push(" AND EXISTS (SELECT 1 FROM album_artists role WHERE role.artist_key=artist.artist_key))) AND (").push_bind(folder).push(" IS NULL OR ((").push_bind(!album_artist).push(" AND EXISTS (SELECT 1 FROM track_artists role JOIN track_folders scope USING(track_key) WHERE role.artist_key=artist.artist_key AND scope.folder_key=").push_bind(folder).push(")) OR (").push_bind(album_artist).push(" AND EXISTS (SELECT 1 FROM album_artists role JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE role.artist_key=artist.artist_key AND scope.folder_key=").push_bind(folder).push("))))");
}

impl Database {
    pub async fn album_release_classifications(
        &self,
        source: SourceKey,
        keys: &[AlbumKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumReleaseClassification>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut classifications = Vec::with_capacity(keys.len());
        for batch in keys.chunks(COLLECTION_ROW_LIMIT) {
            let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key,position) AS (");
            query.push_values(batch.iter().enumerate(), |mut row, (position, key)| {
                row.push_bind(*key).push_bind(position as i64);
            });
            query.push(") SELECT album.album_key,album.is_compilation,COALESCE(group_concat(lower(release.release_type),'|'),'') release_types FROM requested JOIN albums album USING(album_key) LEFT JOIN album_release_types release USING(album_key) WHERE album.source_key=").push_bind(source).push(" GROUP BY album.album_key ORDER BY requested.position");
            let rows = query
                .build_query_as::<(AlbumKey, Option<bool>, String)>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?;
            classifications.extend(rows.into_iter().map(|(album_key, compilation, types)| {
                AlbumReleaseClassification {
                    album_key,
                    class: album_release_class(compilation, &types),
                }
            }));
        }
        Database::clear_progress(&mut connection).await?;
        Ok(classifications)
    }

    pub async fn album_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumKey>> {
        collection_key_by_object(self, source, "albums", "album_key", object_id, cancellation)
            .await
            .map(|key| key.map(AlbumKey::from_raw))
    }
    pub async fn artist_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<ArtistKey>> {
        collection_key_by_object(
            self,
            source,
            "artists",
            "artist_key",
            object_id,
            cancellation,
        )
        .await
        .map(|key| key.map(ArtistKey::from_raw))
    }
    pub async fn genre_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<GenreKey>> {
        collection_key_by_object(self, source, "genres", "genre_key", object_id, cancellation)
            .await
            .map(|key| key.map(GenreKey::from_raw))
    }
    pub async fn latest_album_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let album_limit = album_limit.clamp(1, 100) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, String>(
            "WITH latest AS (
               SELECT album.album_key FROM albums album
               WHERE album.source_key=?1
                 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2))
               ORDER BY album.date_added DESC NULLS LAST,album.album_key LIMIT ?3
             )
             SELECT track.media_uri FROM latest JOIN tracks track USING(album_key)
             WHERE (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
             ORDER BY latest.album_key,track.disc_number,track.track_number,track.track_key",
        )
        .bind(source)
        .bind(folder)
        .bind(album_limit)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn album_release_candidates(
        &self,
        source: SourceKey,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumReleaseCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(100) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result=sqlx::query_as::<_,AlbumReleaseCandidate>("SELECT album_key,media_uri,CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id ELSE 'release:'||musicbrainz_release_id END lookup_identity FROM albums WHERE source_key=?1 AND (musicbrainz_release_group_id IS NOT NULL OR musicbrainz_release_id IS NOT NULL) AND (release_lookup_identity IS NULL OR release_lookup_identity<>CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id ELSE 'release:'||musicbrainz_release_id END) ORDER BY album_key LIMIT ?2")
            .bind(source).bind(limit).fetch_all(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn accept_album_release_result(
        &self,
        source: SourceKey,
        album: AlbumKey,
        lookup_identity: &str,
        result: AlbumReleaseResult,
    ) -> LibraryResult<Option<AlbumKey>> {
        if lookup_identity.is_empty()
            || matches!(&result,AlbumReleaseResult::Found { release_types } if release_types.is_empty() || release_types.iter().any(|value|value.trim().is_empty()))
        {
            return Err(LibraryError::InvalidRequest(
                "invalid Album release lookup result".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let identity=sqlx::query_as::<_,(Option<String>,Option<String>)>("SELECT CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id WHEN musicbrainz_release_id IS NOT NULL THEN 'release:'||musicbrainz_release_id END,release_lookup_identity FROM albums WHERE source_key=?1 AND album_key=?2")
            .bind(source).bind(album).fetch_optional(&mut *transaction).await?;
        let Some((current, attempted)) = identity else {
            transaction.rollback().await?;
            return Ok(None);
        };
        if current.as_deref() != Some(lookup_identity)
            || attempted.as_deref() == Some(lookup_identity)
        {
            transaction.rollback().await?;
            return Ok(None);
        }
        if let AlbumReleaseResult::Found { release_types } = result {
            sqlx::query("DELETE FROM album_release_types WHERE album_key=?1")
                .bind(album)
                .execute(&mut *transaction)
                .await?;
            for (position, release_type) in release_types.iter().enumerate() {
                sqlx::query("INSERT INTO album_release_types(album_key,release_type,position) VALUES (?1,?2,?3)")
                    .bind(album).bind(release_type.trim()).bind(position as i64).execute(&mut *transaction).await?;
            }
        }
        sqlx::query(
            "UPDATE albums SET release_lookup_identity=?3 WHERE source_key=?1 AND album_key=?2",
        )
        .bind(source)
        .bind(album)
        .bind(lookup_identity)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(album))
    }

    pub async fn album_route_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<AlbumKey>, usize, Vec<AlbumRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        if !filter.is_empty() {
            let order = sqlx::query_scalar::<_, AlbumKey>(
                "SELECT album.album_key FROM albums album
                 WHERE album.source_key=?1 AND (instr(album.normalized_title || ' ' || lower(album.display_artist), ?2)>0 OR CAST(album.year AS TEXT)=?2 OR EXISTS (SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,?2)>0) OR EXISTS (SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,?2)>0))
                   AND (?3 IS NULL OR EXISTS (SELECT 1 FROM tracks item JOIN track_folders scope USING(track_key) WHERE item.album_key=album.album_key AND scope.folder_key=?3))
                   AND (?4=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1)
                 ORDER BY sort_text, album_key",
            )
            .bind(source)
            .bind(filter)
            .bind(folder)
            .bind(favorites_only)
            .fetch_all(&mut *transaction)
            .await?;
            let seed = window.range(order.len());
            let first_row_position = seed.start;
            let first_rows =
                load_album_rows(&mut transaction, source, &order[seed], folder).await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((order, first_row_position, first_rows));
        }
        if sort == AlbumSort::Title {
            let result = sqlx::query_scalar::<_, AlbumKey>(if descending {
                "SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2)) AND (?3=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) ORDER BY album.sort_text DESC,album.album_key"
            } else {
                "SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2)) AND (?3=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) ORDER BY album.sort_text,album.album_key"
            }).bind(source).bind(folder).bind(favorites_only).fetch_all(&mut *transaction).await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows =
                load_album_rows(&mut transaction, source, &result[seed], folder).await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        if matches!(
            sort,
            AlbumSort::AlbumArtist
                | AlbumSort::Year
                | AlbumSort::ReleaseDate
                | AlbumSort::DateAdded
                | AlbumSort::Rating
                | AlbumSort::Favorite
        ) {
            let order = match (sort, descending) {
                (AlbumSort::AlbumArtist, false) => {
                    "(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key ORDER BY credit.position LIMIT 1) ASC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::AlbumArtist, true) => {
                    "(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key ORDER BY credit.position LIMIT 1) DESC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Year, false) => {
                    "album.year ASC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Year, true) => {
                    "album.year DESC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::ReleaseDate, false) => {
                    "album.release_date ASC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::ReleaseDate, true) => {
                    "album.release_date DESC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::DateAdded, false) => {
                    "album.date_added ASC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::DateAdded, true) => {
                    "album.date_added DESC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Rating, false) => {
                    "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating) ASC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Rating, true) => {
                    "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating) DESC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Favorite, false) => {
                    "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) ASC,album.sort_text,album.album_key"
                }
                (AlbumSort::Favorite, true) => {
                    "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) DESC,album.sort_text,album.album_key"
                }
                _ => unreachable!(),
            };
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT album.album_key FROM albums album WHERE album.source_key=",
            );
            query.push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=").push_bind(folder).push(")) AND (").push_bind(!favorites_only).push(" OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) ORDER BY ").push(order);
            let result = query
                .build_query_scalar::<AlbumKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows =
                load_album_rows(&mut transaction, source, &result[seed], folder).await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        if matches!(sort, AlbumSort::TrackCount | AlbumSort::Duration) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT album.album_key FROM albums album LEFT JOIN tracks track ON track.album_key=album.album_key AND (",
            );
            query.push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) WHERE album.source_key=").push_bind(source).push(" AND (").push_bind(!favorites_only).push(" OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1) GROUP BY album.album_key ORDER BY ");
            query
                .push(match (sort, descending) {
                    (AlbumSort::TrackCount, false) => "count(track.track_key) ASC",
                    (AlbumSort::TrackCount, true) => "count(track.track_key) DESC",
                    (AlbumSort::Duration, false) => "COALESCE(sum(track.duration_millis),0) ASC",
                    (AlbumSort::Duration, true) => "COALESCE(sum(track.duration_millis),0) DESC",
                    _ => unreachable!(),
                })
                .push(",album.sort_text,album.album_key");
            let result = query
                .build_query_scalar::<AlbumKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows =
                load_album_rows(&mut transaction, source, &result[seed], folder).await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        let result = sqlx::query_scalar::<_, AlbumKey>(
            "WITH listens_by_track AS (
               SELECT media_uri,count(*) plays,max(started_at) last_played
               FROM listens WHERE source_id=(SELECT object_id FROM sources WHERE source_key=?1) GROUP BY media_uri
             ), rows AS (
               SELECT album.album_key,album.sort_text,album.display_artist,album.year,
                      album.release_date,album.date_added,
                      COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating) rating,
                      COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) favorite,
                      count(track.track_key) track_count,
                      COALESCE(sum(track.duration_millis),0) duration,
                      COALESCE(sum(COALESCE(base.play_count,0)+COALESCE(listen.plays,0)),0) plays,
                      max(CASE WHEN base.last_played_at IS NULL THEN listen.last_played
                               WHEN listen.last_played IS NULL THEN base.last_played_at
                               ELSE max(base.last_played_at,listen.last_played) END) last_played
               FROM albums album LEFT JOIN tracks track USING(album_key)
               LEFT JOIN activity_baseline base ON base.source_key=album.source_key
                    AND base.track_object_id=track.object_id
                    AND base.period='lifetime' AND base.item_kind='track'
               LEFT JOIN listens_by_track listen ON listen.media_uri=track.media_uri
               WHERE album.source_key=?1
                 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
                 AND (?5=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1)
               GROUP BY album.album_key
             ) SELECT album_key FROM rows ORDER BY
               CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,
               CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,
               CASE WHEN ?2=1 AND ?3=0 THEN display_artist END ASC,
               CASE WHEN ?2=1 AND ?3=1 THEN display_artist END DESC,
               CASE WHEN ?2=2 AND ?3=0 THEN year END ASC NULLS LAST,
               CASE WHEN ?2=2 AND ?3=1 THEN year END DESC NULLS LAST,
               CASE WHEN ?2=3 AND ?3=0 THEN release_date END ASC NULLS LAST,
               CASE WHEN ?2=3 AND ?3=1 THEN release_date END DESC NULLS LAST,
               CASE WHEN ?2=4 AND ?3=0 THEN date_added END ASC NULLS LAST,
               CASE WHEN ?2=4 AND ?3=1 THEN date_added END DESC NULLS LAST,
               CASE WHEN ?2=5 AND ?3=0 THEN last_played END ASC NULLS LAST,
               CASE WHEN ?2=5 AND ?3=1 THEN last_played END DESC NULLS LAST,
               CASE WHEN ?2=6 AND ?3=0 THEN plays END ASC,
               CASE WHEN ?2=6 AND ?3=1 THEN plays END DESC,
               CASE WHEN ?2=7 AND ?3=0 THEN rating END ASC NULLS LAST,
               CASE WHEN ?2=7 AND ?3=1 THEN rating END DESC NULLS LAST,
               CASE WHEN ?2=8 AND ?3=0 THEN track_count END ASC,
               CASE WHEN ?2=8 AND ?3=1 THEN track_count END DESC,
               CASE WHEN ?2=9 AND ?3=0 THEN duration END ASC,
               CASE WHEN ?2=9 AND ?3=1 THEN duration END DESC,
               CASE WHEN ?2=10 AND ?3=0 THEN favorite END ASC,
               CASE WHEN ?2=10 AND ?3=1 THEN favorite END DESC,
               sort_text,album_key",
        )
        .bind(source)
        .bind(sort.code())
        .bind(descending)
        .bind(folder)
        .bind(favorites_only)
        .fetch_all(&mut *transaction)
        .await?;
        let seed = window.range(result.len());
        let first_row_position = seed.start;
        let first_rows = load_album_rows(&mut transaction, source, &result[seed], folder).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((result, first_row_position, first_rows))
    }

    pub async fn album_detail_route_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<String>, Vec<String>, Vec<String>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let rows = sqlx::query_as::<_, (String, Option<bool>)>(
            "WITH listens_by_track AS (
               SELECT media_uri,count(*) plays,max(started_at) last_played
               FROM listens WHERE source_id=(SELECT object_id FROM sources WHERE source_key=?1) GROUP BY media_uri
             ), rows AS (
               SELECT album.album_key,album.media_uri,album.sort_text,album.display_artist,album.year,
                      EXISTS(SELECT 1 FROM album_genres relation WHERE relation.album_key=album.album_key) has_genres,
                      album.release_date,album.date_added,
                      COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating) rating,
                      COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) favorite,
                      count(track.track_key) track_count,
                      COALESCE(sum(track.duration_millis),0) duration,
                      COALESCE(sum(COALESCE(base.play_count,0)+COALESCE(listen.plays,0)),0) plays,
                      max(CASE WHEN base.last_played_at IS NULL THEN listen.last_played
                               WHEN listen.last_played IS NULL THEN base.last_played_at
                               ELSE max(base.last_played_at,listen.last_played) END) last_played
               FROM albums album LEFT JOIN tracks track USING(album_key)
               LEFT JOIN activity_baseline base ON base.source_key=album.source_key
                    AND base.track_object_id=track.object_id
                    AND base.period='lifetime' AND base.item_kind='track'
               LEFT JOIN listens_by_track listen ON listen.media_uri=track.media_uri
               WHERE album.source_key=?1
                 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
                 AND (?5=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1)
                 AND (?6 OR instr(album.normalized_title || ' ' || lower(album.display_artist),?7)>0 OR CAST(album.year AS TEXT)=?7 OR EXISTS (SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,?7)>0) OR EXISTS (SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,?7)>0))
               GROUP BY album.album_key
             ), ordered AS (
               SELECT album_key,media_uri,has_genres,row_number() OVER (ORDER BY
                 CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,
                 CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,
                 CASE WHEN ?2=1 AND ?3=0 THEN display_artist END ASC,
                 CASE WHEN ?2=1 AND ?3=1 THEN display_artist END DESC,
                 CASE WHEN ?2=2 AND ?3=0 THEN year END ASC NULLS LAST,
                 CASE WHEN ?2=2 AND ?3=1 THEN year END DESC NULLS LAST,
                 CASE WHEN ?2=3 AND ?3=0 THEN release_date END ASC NULLS LAST,
                 CASE WHEN ?2=3 AND ?3=1 THEN release_date END DESC NULLS LAST,
                 CASE WHEN ?2=4 AND ?3=0 THEN date_added END ASC NULLS LAST,
                 CASE WHEN ?2=4 AND ?3=1 THEN date_added END DESC NULLS LAST,
                 CASE WHEN ?2=5 AND ?3=0 THEN last_played END ASC NULLS LAST,
                 CASE WHEN ?2=5 AND ?3=1 THEN last_played END DESC NULLS LAST,
                 CASE WHEN ?2=6 AND ?3=0 THEN plays END ASC,
                 CASE WHEN ?2=6 AND ?3=1 THEN plays END DESC,
                 CASE WHEN ?2=7 AND ?3=0 THEN rating END ASC NULLS LAST,
                 CASE WHEN ?2=7 AND ?3=1 THEN rating END DESC NULLS LAST,
                 CASE WHEN ?2=8 AND ?3=0 THEN track_count END ASC,
                 CASE WHEN ?2=8 AND ?3=1 THEN track_count END DESC,
                 CASE WHEN ?2=9 AND ?3=0 THEN duration END ASC,
                 CASE WHEN ?2=9 AND ?3=1 THEN duration END DESC,
                 CASE WHEN ?2=10 AND ?3=0 THEN favorite END ASC,
                 CASE WHEN ?2=10 AND ?3=1 THEN favorite END DESC,
                 sort_text,album_key) album_rank
               FROM rows
             ), flattened(media_uri,album_has_genres,album_rank,item_rank) AS (
               SELECT media_uri,has_genres,album_rank,0 FROM ordered
               UNION ALL
               SELECT track.media_uri,NULL,ordered.album_rank,
                      row_number() OVER (PARTITION BY track.album_key ORDER BY track.disc_number,track.track_number,track.sort_text,track.track_key)
               FROM ordered JOIN tracks track USING(album_key)
               WHERE (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
             ) SELECT media_uri,album_has_genres FROM flattened ORDER BY album_rank,item_rank",
        )
        .bind(source)
        .bind(sort.code())
        .bind(descending)
        .bind(folder)
        .bind(favorites_only)
        .bind(filter.is_empty())
        .bind(filter)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        let rows = rows?;
        let albums_with_genres = rows
            .iter()
            .filter_map(|(uri, has_genres)| has_genres.unwrap_or(false).then(|| uri.clone()))
            .collect();
        let albums = rows
            .iter()
            .filter_map(|(uri, has_genres)| has_genres.map(|_| uri.clone()))
            .collect();
        Ok((
            rows.into_iter().map(|(uri, _)| uri).collect(),
            albums,
            albums_with_genres,
        ))
    }

    pub async fn artist_route_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists_only: bool,
        favorites_only: bool,
        filter: &str,
        sort: ArtistSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<ArtistKey>, usize, Vec<ArtistRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        if !filter.is_empty() {
            let result = sqlx::query_scalar::<_, ArtistKey>(
                "SELECT artist.artist_key FROM artists artist
                 WHERE artist.source_key=?1 AND instr(artist.normalized_name, ?2)>0
                   AND ((?4=0 AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.artist_key=artist.artist_key)) OR (?4=1 AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.artist_key=artist.artist_key)))
                   AND (?3 IS NULL OR (?4=0 AND EXISTS (SELECT 1 FROM track_artists credit JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?3)) OR (?4=1 AND EXISTS (SELECT 1 FROM album_artists credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=?3)))
                   AND (?5=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1)
                 ORDER BY sort_text, artist_key",
            )
            .bind(source)
            .bind(filter)
            .bind(folder)
            .bind(album_artists_only)
            .bind(favorites_only)
            .fetch_all(&mut *transaction)
            .await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows = load_artist_rows(
                &mut transaction,
                source,
                &result[seed],
                album_artists_only,
                folder,
            )
            .await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        if sort == ArtistSort::Title {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT artist.artist_key FROM artists artist WHERE artist.source_key=",
            );
            query.push_bind(source);
            push_artist_role_scope(&mut query, album_artists_only, folder);
            query.push(" AND (").push_bind(!favorites_only).push(" OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1) ORDER BY artist.sort_text ").push(if descending { "DESC" } else { "ASC" }).push(",artist.artist_key");
            let result = query
                .build_query_scalar::<ArtistKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows = load_artist_rows(
                &mut transaction,
                source,
                &result[seed],
                album_artists_only,
                folder,
            )
            .await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        if matches!(sort, ArtistSort::Rating | ArtistSort::Favorite) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT artist.artist_key FROM artists artist WHERE artist.source_key=",
            );
            query.push_bind(source);
            push_artist_role_scope(&mut query, album_artists_only, folder);
            query
                .push(" AND (")
                .push_bind(!favorites_only)
                .push(" OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1) ORDER BY ");
            query
                .push(match (sort, descending) {
                    (ArtistSort::Rating, false) => {
                        "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_rating) ASC NULLS LAST"
                    }
                    (ArtistSort::Rating, true) => {
                        "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_rating) DESC NULLS LAST"
                    }
                    (ArtistSort::Favorite, false) => {
                        "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite) ASC"
                    }
                    (ArtistSort::Favorite, true) => {
                        "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite) DESC"
                    }
                    _ => unreachable!(),
                })
                .push(",artist.sort_text,artist.artist_key");
            let result = query
                .build_query_scalar::<ArtistKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows = load_artist_rows(
                &mut transaction,
                source,
                &result[seed],
                album_artists_only,
                folder,
            )
            .await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        if matches!(sort, ArtistSort::AlbumCount | ArtistSort::TrackCount) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT artist.artist_key FROM artists artist LEFT JOIN tracks track ON track.source_key=artist.source_key AND ((",
            );
            query.push_bind(!album_artists_only).push(" AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.track_key=track.track_key AND credit.artist_key=artist.artist_key)) OR (").push_bind(album_artists_only).push(" AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.album_key=track.album_key AND credit.artist_key=artist.artist_key))) AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) WHERE artist.source_key=").push_bind(source);
            push_artist_role_scope(&mut query, album_artists_only, folder);
            query.push(" AND (").push_bind(!favorites_only).push(" OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1) GROUP BY artist.artist_key ORDER BY ");
            query
                .push(match (sort, descending) {
                    (ArtistSort::AlbumCount, false) => "count(DISTINCT track.album_key) ASC",
                    (ArtistSort::AlbumCount, true) => "count(DISTINCT track.album_key) DESC",
                    (ArtistSort::TrackCount, false) => "count(DISTINCT track.track_key) ASC",
                    (ArtistSort::TrackCount, true) => "count(DISTINCT track.track_key) DESC",
                    _ => unreachable!(),
                })
                .push(",artist.sort_text,artist.artist_key");
            let result = query
                .build_query_scalar::<ArtistKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?;
            let seed = window.range(result.len());
            let first_row_position = seed.start;
            let first_rows = load_artist_rows(
                &mut transaction,
                source,
                &result[seed],
                album_artists_only,
                folder,
            )
            .await?;
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok((result, first_row_position, first_rows));
        }
        let result=sqlx::query_scalar::<_,ArtistKey>(
            "WITH listen AS (SELECT media_uri,count(*) plays,max(started_at) last_played
              FROM listens WHERE source_id=(SELECT object_id FROM sources WHERE source_key=?1) GROUP BY media_uri),
             rows AS (SELECT artist.artist_key,artist.sort_text,
               COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_rating) rating,
               COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite) favorite,
               count(DISTINCT track.album_key) album_count,count(DISTINCT track.track_key) track_count,
               COALESCE(sum(COALESCE(base.play_count,0)+COALESCE(listen.plays,0)),0) plays,
               max(CASE WHEN base.last_played_at IS NULL THEN listen.last_played
                        WHEN listen.last_played IS NULL THEN base.last_played_at
                        ELSE max(base.last_played_at,listen.last_played) END) last_played
              FROM artists artist LEFT JOIN tracks track ON
                ((?6=0 AND EXISTS (SELECT 1 FROM track_artists credit
                    WHERE credit.artist_key=artist.artist_key AND credit.track_key=track.track_key))
                 OR (?6=1 AND EXISTS (SELECT 1 FROM album_artists credit
                    WHERE credit.artist_key=artist.artist_key AND credit.album_key=track.album_key)))
              LEFT JOIN activity_baseline base ON base.source_key=artist.source_key
                   AND base.track_object_id=track.object_id
                   AND base.period='lifetime' AND base.item_kind='track' LEFT JOIN listen ON listen.media_uri=track.media_uri
              WHERE artist.source_key=?1
                AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
                AND (?5=0 OR COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1)
                AND ((?6=0 AND EXISTS (SELECT 1 FROM track_artists role WHERE role.artist_key=artist.artist_key))
                     OR (?6=1 AND EXISTS (SELECT 1 FROM album_artists role WHERE role.artist_key=artist.artist_key)))
              GROUP BY artist.artist_key)
             SELECT artist_key FROM rows ORDER BY
              CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,
              CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,
              CASE WHEN ?2=1 AND ?3=0 THEN album_count END ASC,
              CASE WHEN ?2=1 AND ?3=1 THEN album_count END DESC,
              CASE WHEN ?2=2 AND ?3=0 THEN track_count END ASC,
              CASE WHEN ?2=2 AND ?3=1 THEN track_count END DESC,
              CASE WHEN ?2=3 AND ?3=0 THEN last_played END ASC NULLS LAST,
              CASE WHEN ?2=3 AND ?3=1 THEN last_played END DESC NULLS LAST,
              CASE WHEN ?2=4 AND ?3=0 THEN plays END ASC,
              CASE WHEN ?2=4 AND ?3=1 THEN plays END DESC,
              CASE WHEN ?2=5 AND ?3=0 THEN rating END ASC NULLS LAST,
              CASE WHEN ?2=5 AND ?3=1 THEN rating END DESC NULLS LAST,
              CASE WHEN ?2=6 AND ?3=0 THEN favorite END ASC,
              CASE WHEN ?2=6 AND ?3=1 THEN favorite END DESC,sort_text,artist_key")
            .bind(source).bind(sort.code()).bind(descending).bind(folder).bind(favorites_only).bind(album_artists_only).fetch_all(&mut *transaction).await?;
        let seed = window.range(result.len());
        let first_row_position = seed.start;
        let first_rows = load_artist_rows(
            &mut transaction,
            source,
            &result[seed],
            album_artists_only,
            folder,
        )
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((result, first_row_position, first_rows))
    }

    pub async fn genre_route_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: GenreSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<GenreKey>, usize, Vec<GenreRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let result = if !filter.is_empty() {
            sqlx::query_scalar::<_, GenreKey>(
                "SELECT genre.genre_key FROM genres genre WHERE genre.source_key=?1 AND instr(genre.normalized_name,?2)>0 AND EXISTS(SELECT 1 FROM track_genres credit WHERE credit.genre_key=genre.genre_key AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=credit.track_key AND scope.folder_key=?3))) ORDER BY genre.sort_text,genre.genre_key",
            )
            .bind(source)
            .bind(filter)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?
        } else if sort == GenreSort::Title {
            sqlx::query_scalar::<_, GenreKey>(if descending {"SELECT genre.genre_key FROM genres genre WHERE genre.source_key=?1 AND EXISTS(SELECT 1 FROM track_genres credit WHERE credit.genre_key=genre.genre_key AND (?2 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=credit.track_key AND scope.folder_key=?2))) ORDER BY genre.sort_text DESC,genre.genre_key"} else {"SELECT genre.genre_key FROM genres genre WHERE genre.source_key=?1 AND EXISTS(SELECT 1 FROM track_genres credit WHERE credit.genre_key=genre.genre_key AND (?2 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=credit.track_key AND scope.folder_key=?2))) ORDER BY genre.sort_text,genre.genre_key"}).bind(source).bind(folder).fetch_all(&mut *transaction).await?
        } else {
            sqlx::query_scalar::<_,GenreKey>(
                "WITH rows AS (SELECT genre.genre_key,genre.sort_text,count(DISTINCT track.album_key) album_count,count(DISTINCT track.track_key) track_count FROM genres genre JOIN track_genres credit USING(genre_key) JOIN tracks track USING(track_key) WHERE genre.source_key=?1 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4)) GROUP BY genre.genre_key) SELECT genre_key FROM rows ORDER BY CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,CASE WHEN ?2=1 AND ?3=0 THEN album_count END ASC,CASE WHEN ?2=1 AND ?3=1 THEN album_count END DESC,CASE WHEN ?2=2 AND ?3=0 THEN track_count END ASC,CASE WHEN ?2=2 AND ?3=1 THEN track_count END DESC,sort_text,genre_key")
                .bind(source).bind(sort.code()).bind(descending).bind(folder).fetch_all(&mut *transaction).await?
        };
        let seed = window.range(result.len());
        let first_row_position = seed.start;
        let first_rows = load_genre_rows(&mut transaction, source, &result[seed], folder).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((result, first_row_position, first_rows))
    }

    pub async fn mood_route_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: MoodSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<MoodKey>, usize, Vec<MoodRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let result = if !filter.is_empty() {
            sqlx::query_scalar::<_, MoodKey>("SELECT mood.mood_key FROM moods mood WHERE mood.source_key=?1 AND instr(mood.normalized_name,?2)>0 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?3)) ORDER BY mood.sort_text,mood.mood_key").bind(source).bind(filter).bind(folder).fetch_all(&mut *transaction).await?
        } else if sort == MoodSort::Title {
            sqlx::query_scalar::<_,MoodKey>(if descending {"SELECT mood.mood_key FROM moods mood WHERE mood.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?2)) ORDER BY mood.sort_text DESC,mood.mood_key"} else {"SELECT mood.mood_key FROM moods mood WHERE mood.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?2)) ORDER BY mood.sort_text,mood.mood_key"}).bind(source).bind(folder).fetch_all(&mut *transaction).await?
        } else {
            sqlx::query_scalar::<_,MoodKey>("WITH rows AS (SELECT mood.mood_key,mood.sort_text,count(DISTINCT track.track_key) track_count,COALESCE(sum(track.duration_millis),0) duration FROM moods mood LEFT JOIN track_moods credit USING(mood_key) LEFT JOIN tracks track USING(track_key) WHERE mood.source_key=?1 AND (?4 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4)) GROUP BY mood.mood_key) SELECT mood_key FROM rows ORDER BY CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,CASE WHEN ?2=1 AND ?3=0 THEN track_count END ASC,CASE WHEN ?2=1 AND ?3=1 THEN track_count END DESC,CASE WHEN ?2=2 AND ?3=0 THEN duration END ASC,CASE WHEN ?2=2 AND ?3=1 THEN duration END DESC,sort_text,mood_key").bind(source).bind(sort.code()).bind(descending).bind(folder).fetch_all(&mut *transaction).await?
        };
        let seed = window.range(result.len());
        let first_row_position = seed.start;
        let first_rows = load_mood_rows(&mut transaction, source, &result[seed], folder).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((result, first_row_position, first_rows))
    }
    pub async fn folder_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<FolderKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, FolderKey>(
            "SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn album_track_route_page(
        &self,
        source: SourceKey,
        album: AlbumKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = collection_track_query(
            source,
            sort,
            descending,
            &crate::QueueCollection::AlbumKey(album),
        );
        let order = finish_collection_track_order(
            query,
            folder,
            filter,
            sort,
            descending,
            &mut transaction,
        )
        .await?;
        let page = finish_track_route_page(&mut transaction, order, window).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(page)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn artist_track_route_page(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        favorites_only: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut query = collection_track_query(
            source,
            sort,
            descending,
            &crate::QueueCollection::ArtistKey {
                key: artist,
                album_artist,
            },
        );
        crate::tracks::track_filter(&mut query, None, "", favorites_only);
        let order = finish_collection_track_order(
            query,
            folder,
            filter,
            sort,
            descending,
            &mut transaction,
        )
        .await?;
        let page = finish_track_route_page(&mut transaction, order, window).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(page)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn genre_track_route_page(
        &self,
        source: SourceKey,
        genre: GenreKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = collection_track_query(
            source,
            sort,
            descending,
            &crate::QueueCollection::Genre(genre),
        );
        let order = finish_collection_track_order(
            query,
            folder,
            filter,
            sort,
            descending,
            &mut transaction,
        )
        .await?;
        let page = finish_track_route_page(&mut transaction, order, window).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(page)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn mood_track_route_page(
        &self,
        source: SourceKey,
        mood: MoodKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<TrackRoutePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = collection_track_query(
            source,
            sort,
            descending,
            &crate::QueueCollection::Mood(mood),
        );
        let order = finish_collection_track_order(
            query,
            folder,
            filter,
            sort,
            descending,
            &mut transaction,
        )
        .await?;
        let page = finish_track_route_page(&mut transaction, order, window).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(page)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn artist_album_projection_order(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumKey>> {
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT album.album_key FROM albums album
             WHERE album.source_key=?1 AND (
               (?3=1 AND EXISTS(SELECT 1 FROM album_artists credit WHERE credit.album_key=album.album_key AND credit.artist_key=?2))
               OR (?3=0 AND EXISTS(SELECT 1 FROM tracks track JOIN track_artists credit USING(track_key) WHERE track.album_key=album.album_key AND credit.artist_key=?2)))
             AND (?4 IS NULL OR EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?4))
             AND (?5 OR instr(lower(album.title),?6)>0 OR instr(lower(album.display_artist),?6)>0 OR CAST(album.year AS TEXT)=?6)
             ORDER BY
               CASE WHEN ?7=0 AND ?8=0 THEN album.sort_text END ASC,
               CASE WHEN ?7=0 AND ?8=1 THEN album.sort_text END DESC,
               CASE WHEN ?7=1 AND ?8=0 THEN album.display_artist END ASC,
               CASE WHEN ?7=1 AND ?8=1 THEN album.display_artist END DESC,
               CASE WHEN ?7=2 AND ?8=0 THEN album.year END ASC NULLS LAST,
               CASE WHEN ?7=2 AND ?8=1 THEN album.year END DESC NULLS LAST,
               CASE WHEN ?7=3 AND ?8=0 THEN album.release_date END ASC NULLS LAST,
               CASE WHEN ?7=3 AND ?8=1 THEN album.release_date END DESC NULLS LAST,
               CASE WHEN ?7=4 AND ?8=0 THEN album.date_added END ASC NULLS LAST,
               CASE WHEN ?7=4 AND ?8=1 THEN album.date_added END DESC NULLS LAST,
               CASE WHEN ?7=5 AND ?8=0 THEN (SELECT max(value) FROM (SELECT base.last_played_at value FROM activity_baseline base JOIN tracks item ON item.source_key=base.source_key AND item.object_id=base.track_object_id AND base.period='lifetime' AND base.item_kind='track' WHERE item.album_key=album.album_key UNION ALL SELECT listen.started_at FROM listens listen JOIN tracks item USING(media_uri) WHERE item.album_key=album.album_key)) END ASC NULLS LAST,
               CASE WHEN ?7=5 AND ?8=1 THEN (SELECT max(value) FROM (SELECT base.last_played_at value FROM activity_baseline base JOIN tracks item ON item.source_key=base.source_key AND item.object_id=base.track_object_id AND base.period='lifetime' AND base.item_kind='track' WHERE item.album_key=album.album_key UNION ALL SELECT listen.started_at FROM listens listen JOIN tracks item USING(media_uri) WHERE item.album_key=album.album_key)) END DESC NULLS LAST,
               CASE WHEN ?7=6 AND ?8=0 THEN COALESCE((SELECT sum(base.play_count) FROM activity_baseline base JOIN tracks item ON item.source_key=base.source_key AND item.object_id=base.track_object_id AND base.period='lifetime' AND base.item_kind='track' WHERE item.album_key=album.album_key),0)+(SELECT count(*) FROM listens listen JOIN tracks item USING(media_uri) WHERE item.album_key=album.album_key) END ASC,
               CASE WHEN ?7=6 AND ?8=1 THEN COALESCE((SELECT sum(base.play_count) FROM activity_baseline base JOIN tracks item ON item.source_key=base.source_key AND item.object_id=base.track_object_id AND base.period='lifetime' AND base.item_kind='track' WHERE item.album_key=album.album_key),0)+(SELECT count(*) FROM listens listen JOIN tracks item USING(media_uri) WHERE item.album_key=album.album_key) END DESC,
               CASE WHEN ?7=7 AND ?8=0 THEN COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating) END ASC NULLS LAST,
               CASE WHEN ?7=7 AND ?8=1 THEN COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating) END DESC NULLS LAST,
               CASE WHEN ?7=8 AND ?8=0 THEN (SELECT count(*) FROM tracks item WHERE item.album_key=album.album_key) END ASC,
               CASE WHEN ?7=8 AND ?8=1 THEN (SELECT count(*) FROM tracks item WHERE item.album_key=album.album_key) END DESC,
               CASE WHEN ?7=9 AND ?8=0 THEN (SELECT COALESCE(sum(duration_millis),0) FROM tracks item WHERE item.album_key=album.album_key) END ASC,
               CASE WHEN ?7=9 AND ?8=1 THEN (SELECT COALESCE(sum(duration_millis),0) FROM tracks item WHERE item.album_key=album.album_key) END DESC,
               CASE WHEN ?7=10 AND ?8=0 THEN COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) END ASC,
               CASE WHEN ?7=10 AND ?8=1 THEN COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) END DESC,
               album.sort_text,album.album_key",
        )
        .bind(source)
        .bind(artist)
        .bind(album_artist)
        .bind(folder)
        .bind(filter.is_empty())
        .bind(filter)
        .bind(sort.code())
        .bind(descending)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn album_rows(
        &self,
        source: SourceKey,
        keys: &[AlbumKey],
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumRow>> {
        if keys.len() > COLLECTION_ROW_LIMIT {
            return Err(row_limit("Album"));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_album_rows(&mut transaction, source, keys, folder).await;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn album_row_by_media_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let Some((source, key)) = sqlx::query_as::<_, (SourceKey, AlbumKey)>(
            "SELECT source_key,album_key FROM albums WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?
        else {
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let mut transaction = connection.begin().await?;
        let result = load_album_rows(&mut transaction, source, &[key], None)
            .await?
            .pop();
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn artist_rows(
        &self,
        source: SourceKey,
        keys: &[ArtistKey],
        album_artist: bool,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ArtistRow>> {
        if keys.len() > COLLECTION_ROW_LIMIT {
            return Err(row_limit("Artist"));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_artist_rows(&mut transaction, source, keys, album_artist, folder).await;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn artist_row_by_media_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<ArtistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let Some((source, key)) = sqlx::query_as::<_, (SourceKey, ArtistKey)>(
            "SELECT source_key,artist_key FROM artists WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?
        else {
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let mut transaction = connection.begin().await?;
        let result = load_artist_rows(&mut transaction, source, &[key], false, None)
            .await?
            .pop();
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(result)
    }

    pub async fn genre_rows(
        &self,
        source: SourceKey,
        keys: &[GenreKey],
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreRow>> {
        if keys.len() > COLLECTION_ROW_LIMIT {
            return Err(row_limit("Genre"));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_genre_rows(&mut transaction, source, keys, folder).await;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn mood_rows(
        &self,
        source: SourceKey,
        keys: &[MoodKey],
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MoodRow>> {
        if keys.len() > COLLECTION_ROW_LIMIT {
            return Err(row_limit("Mood"));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_mood_rows(&mut transaction, source, keys, folder).await;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        result
    }

    pub async fn folder_rows(
        &self,
        source: SourceKey,
        keys: &[FolderKey],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderRow>> {
        if keys.len() > COLLECTION_ROW_LIMIT {
            return Err(row_limit("Folder"));
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(folder_key, position) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
            row.push_bind(*key).push_bind(position as i64);
        });
        query.push(
            ") SELECT folder.folder_key, folder.source_key, folder.object_id, folder.name,
                      folder.artwork_binding,
                      count(DISTINCT relation.track_key) AS track_count
               FROM requested JOIN folders AS folder USING(folder_key)
               LEFT JOIN track_folders AS relation USING(folder_key)
               WHERE folder.source_key=",
        );
        query
            .push_bind(source)
            .push(" GROUP BY folder.folder_key ORDER BY requested.position");
        let result = query
            .build_query_as::<FolderRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn folder_child_order(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = if let Some(parent) = parent {
            sqlx::query_scalar::<_, FolderKey>(
                "SELECT DISTINCT child.folder_key
                 FROM track_folders parent_link
                 JOIN tracks track ON track.track_key=parent_link.track_key
                 JOIN track_folders child_link ON child_link.track_key=track.track_key
                    AND child_link.position=parent_link.position+1
                 JOIN folders child ON child.folder_key=child_link.folder_key
                 WHERE track.source_key=?1 AND parent_link.folder_key=?2
                 ORDER BY child.sort_text,child.folder_key",
            )
            .bind(source)
            .bind(parent)
            .fetch_all(&mut *connection)
            .await
        } else {
            sqlx::query_scalar::<_, FolderKey>(
                "SELECT DISTINCT folder.folder_key
                 FROM track_folders relation JOIN tracks track USING(track_key)
                 JOIN folders folder USING(folder_key)
                 WHERE track.source_key=?1 AND relation.position=0
                 ORDER BY folder.sort_text,folder.folder_key",
            )
            .bind(source)
            .fetch_all(&mut *connection)
            .await
        };
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn album_detail(
        &self,
        media_uri: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumDetail>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let Some((source, key)) = sqlx::query_as::<_, (SourceKey, AlbumKey)>(
            "SELECT source_key,album_key FROM albums WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *transaction)
        .await?
        else {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let album = load_album_rows(&mut transaction, source, &[key], None)
            .await?
            .pop();
        let Some(album) = album else {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let query = collection_track_query(
            source,
            sort,
            descending,
            &crate::QueueCollection::AlbumKey(key),
        );
        let track_order =
            finish_collection_track_order(query, None, "", sort, descending, &mut transaction)
                .await?;
        let artists = album
            .album_artists
            .iter()
            .map(|artist| artist.artist_key)
            .collect();
        let genres = album.genres.iter().map(|genre| genre.genre_key).collect();
        let release_types = album.release_types.clone();
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(Some(AlbumDetail {
            album,
            track_order: track_order.into_iter().map(|(_, uri)| uri).collect(),
            artists,
            genres,
            release_types,
        }))
    }

    pub async fn album_detail_route_rows(
        &self,
        source: SourceKey,
        items: &[String],
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Vec<AlbumRow>, Vec<TrackRow>)> {
        if items.len() > COLLECTION_ROW_LIMIT {
            return Err(row_limit("Album detail route"));
        }
        let album_uris = items
            .iter()
            .filter(|uri| {
                crate::source_entity_parts(uri).is_some_and(|(_, kind, _)| kind == "album")
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        let track_uris = items
            .iter()
            .filter(|uri| {
                !crate::source_entity_parts(uri).is_some_and(|(_, kind, _)| kind == "album")
            })
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let albums = if album_uris.is_empty() {
            Vec::new()
        } else {
            let mut query = QueryBuilder::<Sqlite>::new("WITH requested(media_uri) AS (");
            query.push_values(album_uris, |mut row, uri| {
                row.push_bind(uri);
            });
            query.push(
                ") SELECT album.album_key FROM requested JOIN albums album USING(media_uri)
                 WHERE album.source_key=",
            );
            query.push_bind(source);
            query
                .build_query_scalar::<AlbumKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?
        };
        let tracks = if track_uris.is_empty() {
            Vec::new()
        } else {
            let mut query = QueryBuilder::<Sqlite>::new("WITH requested(media_uri) AS (");
            query.push_values(track_uris, |mut row, uri| {
                row.push_bind(uri);
            });
            query.push(
                ") SELECT track.track_key FROM requested JOIN tracks track USING(media_uri)
                 WHERE track.source_key=",
            );
            query.push_bind(source);
            query
                .build_query_scalar::<TrackKey>()
                .persistent(false)
                .fetch_all(&mut *transaction)
                .await?
        };
        let albums = load_album_rows(&mut transaction, source, &albums, folder).await?;
        let tracks = load_track_rows(&mut transaction, &tracks).await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok((albums, tracks))
    }

    pub async fn artist_detail(
        &self,
        source: SourceKey,
        key: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<ArtistDetail>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let artist = load_artist_rows(&mut transaction, source, &[key], album_artist, folder)
            .await?
            .pop();
        let Some(artist) = artist else {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let representative_albums = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT DISTINCT track.album_key FROM tracks track
             WHERE track.source_key=?2 AND track.album_key IS NOT NULL
               AND ((?3=0 AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.track_key=track.track_key AND credit.artist_key=?1)) OR (?3=1 AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.album_key=track.album_key AND credit.artist_key=?1)))
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
             ORDER BY track.date_added DESC NULLS LAST,track.album_key LIMIT 16",
        )
        .bind(key)
        .bind(source)
        .bind(album_artist)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(Some(ArtistDetail {
            artist,
            representative_albums,
        }))
    }

    pub async fn genre_detail(
        &self,
        source: SourceKey,
        key: GenreKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<GenreDetail>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let genre = sqlx::query_as::<_, GenreRow>(
            "SELECT genre.genre_key,genre.source_key,genre.object_id,genre.name,genre.artwork_binding,
              count(DISTINCT track.album_key) album_count,
              count(DISTINCT track.track_key) track_count,
              COALESCE(sum(track.duration_millis),0) duration_millis,
              count(DISTINCT CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') THEN track.track_key END) downloaded_count
             FROM genres genre LEFT JOIN track_genres credit USING(genre_key)
             LEFT JOIN tracks track USING(track_key)
             WHERE genre.source_key=?1 AND genre.genre_key=?2
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
             GROUP BY genre.genre_key",
        )
        .bind(source)
        .bind(key)
        .bind(folder)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(genre) = genre else {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let representative_albums = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT DISTINCT track.album_key FROM track_genres credit
             JOIN tracks track USING(track_key)
             WHERE credit.genre_key=?1 AND track.source_key=?2 AND track.album_key IS NOT NULL
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
             ORDER BY track.date_added DESC NULLS LAST,track.album_key LIMIT 16",
        )
        .bind(key)
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(Some(GenreDetail {
            genre,
            representative_albums,
        }))
    }

    pub async fn mood_detail(
        &self,
        source: SourceKey,
        key: MoodKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<MoodDetail>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mood = sqlx::query_as::<_, MoodRow>(
            "SELECT mood.mood_key,mood.source_key,mood.name,
              count(DISTINCT track.track_key) track_count,
              COALESCE(sum(track.duration_millis),0) duration_millis,
              count(DISTINCT CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') THEN track.track_key END) downloaded_count
             FROM moods mood LEFT JOIN track_moods credit USING(mood_key)
             LEFT JOIN tracks track USING(track_key)
             WHERE mood.source_key=?1 AND mood.mood_key=?2
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
             GROUP BY mood.mood_key",
        )
        .bind(source)
        .bind(key)
        .bind(folder)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(mood) = mood else {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let representative_albums = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT DISTINCT track.album_key FROM track_moods credit
             JOIN tracks track USING(track_key)
             WHERE credit.mood_key=?1 AND track.source_key=?2 AND track.album_key IS NOT NULL
               AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
             ORDER BY track.date_added DESC NULLS LAST,track.album_key LIMIT 16",
        )
        .bind(key)
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        Ok(Some(MoodDetail {
            mood,
            representative_albums,
        }))
    }

    pub async fn update_album_metadata(
        &self,
        source: SourceKey,
        key: AlbumKey,
        write: AlbumMetadataWrite,
    ) -> LibraryResult<crate::ScanOutcome> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let changed = sqlx::query(
            "UPDATE albums SET title=?3, normalized_title=?4,
                    display_artist=?5, sort_text=?6, year=?7,
                    release_date=?8, date_added=?9,
                    musicbrainz_release_id=?10,
                    musicbrainz_release_group_id=?11,
                    is_compilation=?12
             WHERE source_key=?1 AND album_key=?2
               AND (title,normalized_title,display_artist,sort_text,year,release_date,date_added,
                    musicbrainz_release_id,musicbrainz_release_group_id,is_compilation)
                   IS NOT (?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )
        .bind(source)
        .bind(key)
        .bind(write.title)
        .bind(write.normalized_title)
        .bind(write.display_artist)
        .bind(write.sort_text)
        .bind(write.year)
        .bind(write.release_date)
        .bind(write.date_added)
        .bind(write.musicbrainz_release_id)
        .bind(write.musicbrainz_release_group_id)
        .bind(write.is_compilation)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        let outcome = crate::scan::metadata_publication(&mut transaction, source, changed).await?;
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn update_artist_metadata(
        &self,
        source: SourceKey,
        key: ArtistKey,
        write: ArtistMetadataWrite,
    ) -> LibraryResult<crate::ScanOutcome> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let changed = sqlx::query(
            "UPDATE artists SET name=?3, normalized_name=?4,
                    sort_text=?5, musicbrainz_artist_id=?6
             WHERE source_key=?1 AND artist_key=?2
               AND (name,normalized_name,sort_text,musicbrainz_artist_id) IS NOT (?3,?4,?5,?6)",
        )
        .bind(source)
        .bind(key)
        .bind(write.name)
        .bind(write.normalized_name)
        .bind(write.sort_text)
        .bind(write.musicbrainz_artist_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            == 1;
        let outcome = crate::scan::metadata_publication(&mut transaction, source, changed).await?;
        transaction.commit().await?;
        Ok(outcome)
    }
}

pub(crate) async fn load_artist_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[ArtistKey],
    album_artist: bool,
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<ArtistRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(artist_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query
        .push(
            ") SELECT artist.artist_key, artist.source_key, artist.object_id, artist.media_uri, artist.name,
                  artist.musicbrainz_artist_id,
                  artist.artwork_binding,
                  COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite) AS favorite,
                  COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_rating) / 10 AS rating,
                  0 AS play_count,
                  NULL AS last_played,
                  0 AS album_count,
                  0 AS track_count,
                  0 AS duration_millis,
                  0 AS downloaded_count
           FROM requested JOIN artists AS artist USING(artist_key)
           WHERE artist.source_key=",
        );
    query.push_bind(source);
    push_artist_role_scope(&mut query, album_artist, folder);
    query.push(" ORDER BY requested.position");
    let mut result = query
        .build_query_as::<ArtistRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    let facts = artist_facts_rows(connection, source, keys, album_artist, folder).await?;
    let facts = facts
        .into_iter()
        .map(|row| (row.artist_key, row))
        .collect::<BTreeMap<_, _>>();
    for row in &mut result {
        if let Some(facts) = facts.get(&row.artist_key) {
            row.album_count = facts.album_count;
            row.track_count = facts.track_count;
            row.duration_millis = facts.duration_millis;
            row.play_count = facts.play_count;
            row.last_played = facts.last_played;
            row.downloaded_count = facts.downloaded_count;
        }
    }
    Ok(result)
}

fn row_limit(kind: &str) -> LibraryError {
    LibraryError::InvalidRequest(format!(
        "{kind} row reads are limited to {COLLECTION_ROW_LIMIT} keys"
    ))
}

fn collection_track_query(
    source: SourceKey,
    sort: TrackSort,
    descending: bool,
    collection: &crate::QueueCollection,
) -> crate::source_window::SourceQuery {
    let mut query = crate::tracks::track_query(source, sort, descending, false, None, "", false);
    use crate::QueueCollection as C;
    let predicate = match collection {
        C::AlbumKey(key) => format!("track.album_key={}", key.raw()),
        C::Album(uri) => format!(
            "track.album_key=(SELECT album_key FROM albums WHERE media_uri={})",
            crate::source_window::quote(uri)
        ),
        C::ArtistKey { key, album_artist } => format!(
            "EXISTS(SELECT 1 FROM {} credit WHERE credit.artist_key={} AND credit.{}=track.{})",
            if *album_artist {
                "album_artists"
            } else {
                "track_artists"
            },
            key.raw(),
            if *album_artist {
                "album_key"
            } else {
                "track_key"
            },
            if *album_artist {
                "album_key"
            } else {
                "track_key"
            }
        ),
        C::Artist {
            media_uri,
            album_artist,
        } => format!(
            "EXISTS(SELECT 1 FROM {} credit JOIN artists USING(artist_key) WHERE artists.media_uri={} AND credit.{}=track.{})",
            if *album_artist {
                "album_artists"
            } else {
                "track_artists"
            },
            crate::source_window::quote(media_uri),
            if *album_artist {
                "album_key"
            } else {
                "track_key"
            },
            if *album_artist {
                "album_key"
            } else {
                "track_key"
            }
        ),
        C::Genre(key) => format!(
            "EXISTS(SELECT 1 FROM track_genres credit WHERE credit.genre_key={} AND credit.track_key=track.track_key)",
            key.raw()
        ),
        C::Mood(key) => format!(
            "EXISTS(SELECT 1 FROM track_moods credit WHERE credit.mood_key={} AND credit.track_key=track.track_key)",
            key.raw()
        ),
        C::Playlist(_) => unreachable!("playlist queries belong to the playlist owner"),
    };
    query.predicate.push_str(&format!(" AND ({predicate})"));
    query
}

pub(crate) async fn playback_query(
    connection: &mut SqliteConnection,
    collection: &crate::QueueCollection,
    folder: Option<FolderKey>,
    filter: &str,
    sort: TrackSort,
    descending: bool,
    favorites_only: bool,
) -> LibraryResult<crate::source_window::SourceQuery> {
    use crate::QueueCollection as C;
    let source = match collection {
        C::Album(uri) => {
            sqlx::query_scalar("SELECT source_key FROM albums WHERE media_uri=?1")
                .bind(uri)
                .fetch_one(&mut *connection)
                .await?
        }
        C::Artist { media_uri, .. } => {
            sqlx::query_scalar("SELECT source_key FROM artists WHERE media_uri=?1")
                .bind(media_uri)
                .fetch_one(&mut *connection)
                .await?
        }
        C::AlbumKey(key) => {
            sqlx::query_scalar("SELECT source_key FROM albums WHERE album_key=?1")
                .bind(key)
                .fetch_one(&mut *connection)
                .await?
        }
        C::ArtistKey { key, .. } => {
            sqlx::query_scalar("SELECT source_key FROM artists WHERE artist_key=?1")
                .bind(key)
                .fetch_one(&mut *connection)
                .await?
        }
        C::Genre(key) => {
            sqlx::query_scalar("SELECT source_key FROM genres WHERE genre_key=?1")
                .bind(key)
                .fetch_one(&mut *connection)
                .await?
        }
        C::Mood(key) => {
            sqlx::query_scalar("SELECT source_key FROM moods WHERE mood_key=?1")
                .bind(key)
                .fetch_one(&mut *connection)
                .await?
        }
        C::Playlist(key) => {
            return Ok(crate::playlists::playlist_query(
                *key,
                folder,
                crate::PlaylistEntrySort::Position,
                descending,
                filter,
            ));
        }
    };
    let mut query = collection_track_query(source, sort, descending, collection);
    crate::tracks::track_filter(&mut query, folder, filter, favorites_only);
    Ok(query)
}

async fn finish_collection_track_order(
    mut query: crate::source_window::SourceQuery,
    folder: Option<FolderKey>,
    filter: &str,
    sort: TrackSort,
    descending: bool,
    connection: &mut SqliteConnection,
) -> LibraryResult<Vec<(crate::TrackKey, String)>> {
    crate::tracks::track_filter(&mut query, folder, filter, false);
    let _ = (sort, descending);
    Ok(
        sqlx::query_as::<_, (crate::TrackKey, String)>(sqlx::AssertSqlSafe(
            query.select("track.track_key,track.media_uri"),
        ))
        .fetch_all(connection)
        .await?,
    )
}

async fn finish_track_route_page(
    transaction: &mut sqlx::Transaction<'_, Sqlite>,
    order: Vec<(TrackKey, String)>,
    window: RouteSeedWindow,
) -> LibraryResult<TrackRoutePage> {
    let seed = window.range(order.len());
    let first_row_position = seed.start;
    let first_keys = order[seed].iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let first_rows = load_track_rows(transaction, &first_keys).await?;
    let order = order.into_iter().map(|(_, media_uri)| media_uri).collect();
    Ok(TrackRoutePage {
        order,
        first_row_position,
        first_rows,
    })
}

async fn artist_facts_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[ArtistKey],
    album_artist: bool,
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<ArtistFactsRow>> {
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(artist_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),members AS (SELECT requested.artist_key,item.* FROM requested CROSS JOIN ");
    query.push(if album_artist {
        "album_artists credit USING(artist_key) CROSS JOIN tracks item ON item.album_key=credit.album_key"
    } else {
        "track_artists credit USING(artist_key) CROSS JOIN tracks item USING(track_key)"
    });
    query.push(" WHERE item.source_key=").push_bind(source)
        .push(" AND (").push_bind(folder)
        .push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=item.track_key AND scope.folder_key=")
        .push_bind(folder).push("))),listen_activity AS (
          SELECT listen.media_uri,count(*) play_count,max(listen.started_at) last_played
          FROM (SELECT DISTINCT media_uri FROM members) item
          CROSS JOIN listens listen ON listen.media_uri=item.media_uri GROUP BY listen.media_uri)
        SELECT requested.artist_key,count(DISTINCT item.album_key) album_count,
          count(DISTINCT item.track_key) track_count,COALESCE(sum(item.duration_millis),0) duration_millis,
          COALESCE(sum(COALESCE(baseline.play_count,0)+COALESCE(listen.play_count,0)),0) play_count,
          max(CASE WHEN baseline.last_played_at IS NULL THEN listen.last_played WHEN listen.last_played IS NULL THEN baseline.last_played_at ELSE max(baseline.last_played_at,listen.last_played) END) last_played,
          count(DISTINCT CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=item.media_uri AND access.origin='download') THEN item.track_key END) downloaded_count
        FROM requested LEFT JOIN members item USING(artist_key)
        LEFT JOIN activity_baseline baseline ON baseline.source_key=item.source_key AND baseline.track_object_id=item.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'
        LEFT JOIN listen_activity listen USING(media_uri)
        GROUP BY requested.artist_key ORDER BY requested.position");
    Ok(query
        .build_query_as::<ArtistFactsRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?)
}

pub(crate) async fn load_album_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[AlbumKey],
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<AlbumRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),scoped_tracks AS (SELECT track.* FROM requested JOIN tracks track USING(album_key) WHERE track.source_key=").push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push("))),track_facts AS (SELECT track.album_key,count(track.track_key) track_count,COALESCE(sum(track.duration_millis),0) duration_millis,count(CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') THEN 1 END) downloaded_count,COALESCE(sum(COALESCE(baseline.play_count,0)),0) baseline_plays,max(baseline.last_played_at) baseline_last_played FROM scoped_tracks track LEFT JOIN activity_baseline baseline ON baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track' GROUP BY track.album_key),listen_facts AS (SELECT track.album_key,count(*) listen_plays,max(listen.started_at) listen_last_played FROM listens listen JOIN scoped_tracks track USING(media_uri) GROUP BY track.album_key) SELECT album.album_key,album.source_key,album.object_id,album.media_uri,album.title,album.display_artist,album.year,album.release_date,album.date_added,album.musicbrainz_release_id,album.musicbrainz_release_group_id,album.is_compilation,album.release_lookup_identity,album.artwork_binding,COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite) favorite,COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating)/10 rating,COALESCE(track_facts.baseline_plays,0)+COALESCE(listen_facts.listen_plays,0) play_count,CASE WHEN track_facts.baseline_last_played IS NULL THEN listen_facts.listen_last_played WHEN listen_facts.listen_last_played IS NULL THEN track_facts.baseline_last_played ELSE max(track_facts.baseline_last_played,listen_facts.listen_last_played) END last_played,COALESCE(track_facts.track_count,0) track_count,COALESCE(track_facts.duration_millis,0) duration_millis,COALESCE(track_facts.downloaded_count,0) downloaded_count FROM requested JOIN albums album USING(album_key) LEFT JOIN track_facts USING(album_key) LEFT JOIN listen_facts USING(album_key) WHERE album.source_key=").push_bind(source).push(" ORDER BY requested.position");
    let scalars = query
        .build_query_as::<AlbumScalar>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    let mut album_artists = BTreeMap::<AlbumKey, Vec<AlbumArtistLink>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT album_key,min(position) position FROM requested GROUP BY album_key) SELECT relation.album_key,artist.artist_key,artist.media_uri,artist.name FROM unique_requested requested JOIN album_artists relation USING(album_key) JOIN artists artist USING(artist_key) ORDER BY requested.position,relation.position");
    for relation in query
        .build_query_as::<AlbumArtistRelation>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?
    {
        album_artists
            .entry(relation.album_key)
            .or_default()
            .push(AlbumArtistLink {
                artist_key: relation.artist_key,
                media_uri: relation.media_uri,
                name: relation.name,
            });
    }
    let mut genres = BTreeMap::<AlbumKey, Vec<AlbumGenreLink>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT album_key,min(position) position FROM requested GROUP BY album_key) SELECT relation.album_key,genre.genre_key,genre.name FROM unique_requested requested JOIN album_genres relation USING(album_key) JOIN genres genre USING(genre_key) ORDER BY requested.position,relation.position");
    for relation in query
        .build_query_as::<AlbumGenreRelation>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?
    {
        genres
            .entry(relation.album_key)
            .or_default()
            .push(AlbumGenreLink {
                genre_key: relation.genre_key,
                name: relation.name,
            });
    }
    let mut release_types = BTreeMap::<AlbumKey, Vec<String>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT album_key,min(position) position FROM requested GROUP BY album_key) SELECT relation.album_key,relation.release_type FROM unique_requested requested JOIN album_release_types relation USING(album_key) ORDER BY requested.position,relation.position");
    for relation in query
        .build_query_as::<AlbumReleaseTypeRelation>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?
    {
        release_types
            .entry(relation.album_key)
            .or_default()
            .push(relation.release_type);
    }
    let mut rows = Vec::with_capacity(scalars.len());
    for scalar in scalars {
        let key = scalar.album_key;
        rows.push(AlbumRow {
            album_key: scalar.album_key,
            source_key: scalar.source_key,
            object_id: scalar.object_id,
            media_uri: scalar.media_uri,
            title: scalar.title,
            display_artist: scalar.display_artist,
            year: scalar.year,
            release_date: scalar.release_date,
            date_added: scalar.date_added,
            musicbrainz_release_id: scalar.musicbrainz_release_id,
            musicbrainz_release_group_id: scalar.musicbrainz_release_group_id,
            is_compilation: scalar.is_compilation,
            release_lookup_identity: scalar.release_lookup_identity,
            artwork_binding: scalar.artwork_binding,
            favorite: scalar.favorite,
            rating: scalar.rating,
            play_count: scalar.play_count,
            last_played: scalar.last_played,
            track_count: scalar.track_count,
            duration_millis: scalar.duration_millis,
            downloaded_count: scalar.downloaded_count,
            album_artists: album_artists.get(&key).cloned().unwrap_or_default(),
            genres: genres.get(&key).cloned().unwrap_or_default(),
            release_types: release_types.get(&key).cloned().unwrap_or_default(),
        });
    }
    Ok(rows)
}

async fn load_genre_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[GenreKey],
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<GenreRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(genre_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(") SELECT genre.genre_key,genre.source_key,genre.object_id,genre.name,genre.artwork_binding,count(DISTINCT track.album_key) album_count,count(DISTINCT track.track_key) track_count,COALESCE(sum(track.duration_millis),0) duration_millis,count(DISTINCT CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') THEN track.track_key END) downloaded_count FROM requested JOIN genres genre USING(genre_key) LEFT JOIN track_genres relation USING(genre_key) LEFT JOIN tracks track USING(track_key) WHERE genre.source_key=").push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) GROUP BY genre.genre_key ORDER BY requested.position");
    let mut rows = query
        .build_query_as::<GenreRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    for row in &mut rows {
        row.representative_artwork = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT album.artwork_binding FROM track_genres relation JOIN tracks track USING(track_key) JOIN albums album USING(album_key) WHERE relation.genre_key=?1 AND track.source_key=?2 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3)) AND album.artwork_binding IS NOT NULL GROUP BY album.album_key ORDER BY max(track.date_added) DESC NULLS LAST,album.album_key LIMIT 1",
        )
        .bind(row.genre_key)
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await?;
    }
    Ok(rows)
}

async fn load_mood_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[MoodKey],
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<MoodRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(mood_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(") SELECT mood.mood_key,mood.source_key,mood.name,count(DISTINCT track.track_key) track_count,COALESCE(sum(track.duration_millis),0) duration_millis,count(DISTINCT CASE WHEN EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download') THEN track.track_key END) downloaded_count FROM requested JOIN moods mood USING(mood_key) LEFT JOIN track_moods relation USING(mood_key) LEFT JOIN tracks track USING(track_key) WHERE mood.source_key=").push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) GROUP BY mood.mood_key ORDER BY requested.position");
    let mut rows = query
        .build_query_as::<MoodRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    for row in &mut rows {
        row.representative_artwork = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT album.artwork_binding FROM track_moods relation JOIN tracks track USING(track_key) JOIN albums album USING(album_key) WHERE relation.mood_key=?1 AND track.source_key=?2 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3)) AND album.artwork_binding IS NOT NULL GROUP BY album.album_key ORDER BY max(track.date_added) DESC NULLS LAST,album.album_key LIMIT 4",
        )
        .bind(row.mood_key)
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await?;
    }
    Ok(rows)
}

async fn collection_key_by_object(
    database: &Database,
    source: SourceKey,
    table: &'static str,
    key: &'static str,
    object_id: &str,
    cancellation: &ReadCancellation,
) -> LibraryResult<Option<i64>> {
    let sql = match (table, key) {
        ("albums", "album_key") => {
            "SELECT album_key FROM albums WHERE source_key=?1 AND object_id=?2"
        }
        ("artists", "artist_key") => {
            "SELECT artist_key FROM artists WHERE source_key=?1 AND object_id=?2"
        }
        ("genres", "genre_key") => {
            "SELECT genre_key FROM genres WHERE source_key=?1 AND object_id=?2"
        }
        ("moods", "mood_key") => "SELECT mood_key FROM moods WHERE source_key=?1 AND object_id=?2",
        _ => unreachable!("fixed collection key lookup"),
    };
    let (_permit, mut connection) = database.acquire_general(cancellation).await?;
    let result = sqlx::query_scalar(sql)
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await;
    Database::clear_progress(&mut connection).await?;
    Ok(result?)
}
