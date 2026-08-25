//! Owns Album, Artist, Genre, Mood, and Folder orders, rows, details, and metadata writes.
//! Collection membership, filtering, and sorting are resolved in one Store operation.

use std::collections::BTreeMap;

use sqlx::{Connection, FromRow, QueryBuilder, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult, MoodKey,
    ReadCancellation, SourceKey, TrackKey, TrackSort,
};

const COLLECTION_ROW_LIMIT: usize = 128;

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
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct AlbumGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumRow {
    pub album_key: AlbumKey,
    pub source_key: SourceKey,
    pub object_id: String,
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
    pub album_artists: Vec<AlbumArtistLink>,
    pub genres: Vec<AlbumGenreLink>,
    pub release_types: Vec<String>,
}

#[derive(FromRow)]
struct AlbumScalar {
    album_key: AlbumKey,
    source_key: SourceKey,
    object_id: String,
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
}

#[derive(FromRow)]
struct AlbumArtistRelation {
    album_key: AlbumKey,
    artist_key: ArtistKey,
    name: String,
}

#[derive(FromRow)]
struct AlbumGenreRelation {
    album_key: AlbumKey,
    genre_key: GenreKey,
    name: String,
}

#[derive(FromRow)]
struct AlbumActivityRow {
    album_key: AlbumKey,
    play_count: i64,
    last_played: Option<i64>,
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
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct ArtistRow {
    pub artist_key: ArtistKey,
    pub source_key: SourceKey,
    pub object_id: String,
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
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct GenreRow {
    pub genre_key: GenreKey,
    pub source_key: SourceKey,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub album_count: i64,
    pub track_count: i64,
    pub duration_millis: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MoodRow {
    pub mood_key: MoodKey,
    pub source_key: SourceKey,
    pub name: String,
    pub track_count: i64,
    pub duration_millis: i64,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct FolderRow {
    pub folder_key: FolderKey,
    pub source_key: SourceKey,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlbumDetail {
    pub album: AlbumRow,
    pub track_order: Vec<TrackKey>,
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

impl Database {
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
    pub async fn mood_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<MoodKey>> {
        collection_key_by_object(self, source, "moods", "mood_key", object_id, cancellation)
            .await
            .map(|key| key.map(MoodKey::from_raw))
    }
    pub async fn latest_album_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let album_limit = album_limit.clamp(1, 100) as i64;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, TrackKey>(
            "WITH latest AS (
               SELECT album.album_key FROM albums album
               WHERE album.source_key=?1
                 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2))
               ORDER BY album.date_added DESC NULLS LAST,album.album_key LIMIT ?3
             )
             SELECT track.track_key FROM latest JOIN tracks track USING(album_key)
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
        let result=sqlx::query_as::<_,AlbumReleaseCandidate>("SELECT album_key,CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id ELSE 'release:'||musicbrainz_release_id END lookup_identity FROM albums WHERE source_key=?1 AND (musicbrainz_release_group_id IS NOT NULL OR musicbrainz_release_id IS NOT NULL) AND (release_lookup_identity IS NULL OR release_lookup_identity<>CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id ELSE 'release:'||musicbrainz_release_id END) ORDER BY album_key LIMIT ?2")
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

    pub async fn album_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        sort: AlbumSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        if sort == AlbumSort::Title {
            let result = sqlx::query_scalar::<_, AlbumKey>(if descending {
                "SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2)) AND (?3=0 OR COALESCE(album.user_favorite,album.source_favorite)=1) ORDER BY album.sort_text DESC,album.album_key"
            } else {
                "SELECT album.album_key FROM albums album WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=?2)) AND (?3=0 OR COALESCE(album.user_favorite,album.source_favorite)=1) ORDER BY album.sort_text,album.album_key"
            }).bind(source).bind(folder).bind(favorites_only).fetch_all(&mut *connection).await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
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
                    "COALESCE(album.user_rating,album.source_rating) ASC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Rating, true) => {
                    "COALESCE(album.user_rating,album.source_rating) DESC NULLS LAST,album.sort_text,album.album_key"
                }
                (AlbumSort::Favorite, false) => {
                    "COALESCE(album.user_favorite,album.source_favorite) ASC,album.sort_text,album.album_key"
                }
                (AlbumSort::Favorite, true) => {
                    "COALESCE(album.user_favorite,album.source_favorite) DESC,album.sort_text,album.album_key"
                }
                _ => unreachable!(),
            };
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT album.album_key FROM albums album WHERE album.source_key=",
            );
            query.push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.album_key=album.album_key AND scope.folder_key=").push_bind(folder).push(")) AND (").push_bind(!favorites_only).push(" OR COALESCE(album.user_favorite,album.source_favorite)=1) ORDER BY ").push(order);
            let result = query
                .build_query_scalar::<AlbumKey>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        if matches!(sort, AlbumSort::TrackCount | AlbumSort::Duration) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT album.album_key FROM albums album LEFT JOIN tracks track ON track.album_key=album.album_key AND (",
            );
            query.push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) WHERE album.source_key=").push_bind(source).push(" AND (").push_bind(!favorites_only).push(" OR COALESCE(album.user_favorite,album.source_favorite)=1) GROUP BY album.album_key ORDER BY ");
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
                .fetch_all(&mut *connection)
                .await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        let result = sqlx::query_scalar::<_, AlbumKey>(
            "WITH listens_by_track AS (
               SELECT track_key,count(*) plays,max(started_at) last_played
               FROM listens WHERE source_key=?1 AND track_key IS NOT NULL GROUP BY track_key
             ), rows AS (
               SELECT album.album_key,album.sort_text,album.display_artist,album.year,
                      album.release_date,album.date_added,
                      COALESCE(album.user_rating,album.source_rating) rating,
                      COALESCE(album.user_favorite,album.source_favorite) favorite,
                      count(track.track_key) track_count,
                      COALESCE(sum(track.duration_millis),0) duration,
                      COALESCE(sum(COALESCE(base.play_count,0)+COALESCE(listen.plays,0)),0) plays,
                      max(CASE WHEN base.last_played_at IS NULL THEN listen.last_played
                               WHEN listen.last_played IS NULL THEN base.last_played_at
                               ELSE max(base.last_played_at,listen.last_played) END) last_played
               FROM albums album LEFT JOIN tracks track USING(album_key)
               LEFT JOIN activity_baseline base ON base.source_key=album.source_key
                    AND base.track_object_id=track.object_id
               LEFT JOIN listens_by_track listen USING(track_key)
               WHERE album.source_key=?1
                 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
                 AND (?5=0 OR COALESCE(album.user_favorite,album.source_favorite)=1)
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
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn artist_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists_only: bool,
        favorites_only: bool,
        sort: ArtistSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ArtistKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        if sort == ArtistSort::Title {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT artist.artist_key FROM artists artist WHERE artist.source_key=",
            );
            query.push_bind(source).push(" AND (").push_bind(!album_artists_only).push(" OR EXISTS (SELECT 1 FROM album_artists relation WHERE relation.artist_key=artist.artist_key)) AND (").push_bind(!favorites_only).push(" OR COALESCE(artist.user_favorite,artist.source_favorite)=1) AND (").push_bind(folder).push(" IS NULL OR ((").push_bind(!album_artists_only).push(" AND EXISTS (SELECT 1 FROM track_artists credit JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=").push_bind(folder).push(")) OR (").push_bind(album_artists_only).push(" AND EXISTS (SELECT 1 FROM album_artists credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.artist_key=artist.artist_key AND scope.folder_key=").push_bind(folder).push(")))) ORDER BY artist.sort_text ").push(if descending { "DESC" } else { "ASC" }).push(",artist.artist_key");
            let result = query
                .build_query_scalar::<ArtistKey>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        if matches!(sort, ArtistSort::Rating | ArtistSort::Favorite) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT artist.artist_key FROM artists artist WHERE artist.source_key=",
            );
            query.push_bind(source).push(" AND (").push_bind(!album_artists_only).push(" OR EXISTS (SELECT 1 FROM album_artists credit WHERE credit.artist_key=artist.artist_key)) AND (").push_bind(!favorites_only).push(" OR COALESCE(artist.user_favorite,artist.source_favorite)=1) AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE scope.folder_key=").push_bind(folder).push(" AND ((").push_bind(!album_artists_only).push(" AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.track_key=track.track_key AND credit.artist_key=artist.artist_key)) OR (").push_bind(album_artists_only).push(" AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.album_key=track.album_key AND credit.artist_key=artist.artist_key)))) ORDER BY ");
            query
                .push(match (sort, descending) {
                    (ArtistSort::Rating, false) => {
                        "COALESCE(artist.user_rating,artist.source_rating) ASC NULLS LAST"
                    }
                    (ArtistSort::Rating, true) => {
                        "COALESCE(artist.user_rating,artist.source_rating) DESC NULLS LAST"
                    }
                    (ArtistSort::Favorite, false) => {
                        "COALESCE(artist.user_favorite,artist.source_favorite) ASC"
                    }
                    (ArtistSort::Favorite, true) => {
                        "COALESCE(artist.user_favorite,artist.source_favorite) DESC"
                    }
                    _ => unreachable!(),
                })
                .push(",artist.sort_text,artist.artist_key");
            let result = query
                .build_query_scalar::<ArtistKey>()
                .persistent(false)
                .fetch_all(&mut *connection)
                .await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        if matches!(sort, ArtistSort::AlbumCount | ArtistSort::TrackCount) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "SELECT artist.artist_key FROM artists artist LEFT JOIN tracks track ON track.source_key=artist.source_key AND ((",
            );
            query.push_bind(!album_artists_only).push(" AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.track_key=track.track_key AND credit.artist_key=artist.artist_key)) OR (").push_bind(album_artists_only).push(" AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.album_key=track.album_key AND credit.artist_key=artist.artist_key))) AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) WHERE artist.source_key=").push_bind(source).push(" AND (").push_bind(!favorites_only).push(" OR COALESCE(artist.user_favorite,artist.source_favorite)=1) GROUP BY artist.artist_key HAVING ").push_bind(!album_artists_only).push(" OR EXISTS (SELECT 1 FROM album_artists credit WHERE credit.artist_key=artist.artist_key) ORDER BY ");
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
                .fetch_all(&mut *connection)
                .await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        let result=sqlx::query_scalar::<_,ArtistKey>(
            "WITH listen AS (SELECT track_key,count(*) plays,max(started_at) last_played
              FROM listens WHERE source_key=?1 AND track_key IS NOT NULL GROUP BY track_key),
             rows AS (SELECT artist.artist_key,artist.sort_text,
               COALESCE(artist.user_rating,artist.source_rating) rating,
               COALESCE(artist.user_favorite,artist.source_favorite) favorite,
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
                   AND base.track_object_id=track.object_id LEFT JOIN listen USING(track_key)
              WHERE artist.source_key=?1
                AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4))
                AND (?5=0 OR COALESCE(artist.user_favorite,artist.source_favorite)=1)
                AND (?6=0 OR EXISTS (SELECT 1 FROM album_artists album_credit WHERE album_credit.artist_key=artist.artist_key))
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
            .bind(source).bind(sort.code()).bind(descending).bind(folder).bind(favorites_only).bind(album_artists_only).fetch_all(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn genre_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        sort: GenreSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        if sort == GenreSort::Title {
            let result=sqlx::query_scalar::<_,GenreKey>(if descending {"SELECT genre.genre_key FROM genres genre WHERE genre.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_genres credit JOIN track_folders scope USING(track_key) WHERE credit.genre_key=genre.genre_key AND scope.folder_key=?2)) ORDER BY genre.sort_text DESC,genre.genre_key"} else {"SELECT genre.genre_key FROM genres genre WHERE genre.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_genres credit JOIN track_folders scope USING(track_key) WHERE credit.genre_key=genre.genre_key AND scope.folder_key=?2)) ORDER BY genre.sort_text,genre.genre_key"}).bind(source).bind(folder).fetch_all(&mut *connection).await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        let result=sqlx::query_scalar::<_,GenreKey>(
            "WITH rows AS (SELECT genre.genre_key,genre.sort_text,
               count(DISTINCT track.album_key) album_count,count(DISTINCT track.track_key) track_count
              FROM genres genre LEFT JOIN track_genres credit USING(genre_key)
              LEFT JOIN tracks track USING(track_key) WHERE genre.source_key=?1 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4)) GROUP BY genre.genre_key)
             SELECT genre_key FROM rows ORDER BY
              CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,
              CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,
              CASE WHEN ?2=1 AND ?3=0 THEN album_count END ASC,
              CASE WHEN ?2=1 AND ?3=1 THEN album_count END DESC,
              CASE WHEN ?2=2 AND ?3=0 THEN track_count END ASC,
              CASE WHEN ?2=2 AND ?3=1 THEN track_count END DESC,sort_text,genre_key")
            .bind(source).bind(sort.code()).bind(descending).bind(folder).fetch_all(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn mood_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        sort: MoodSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MoodKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        if sort == MoodSort::Title {
            let result=sqlx::query_scalar::<_,MoodKey>(if descending {"SELECT mood.mood_key FROM moods mood WHERE mood.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?2)) ORDER BY mood.sort_text DESC,mood.mood_key"} else {"SELECT mood.mood_key FROM moods mood WHERE mood.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_moods credit JOIN track_folders scope USING(track_key) WHERE credit.mood_key=mood.mood_key AND scope.folder_key=?2)) ORDER BY mood.sort_text,mood.mood_key"}).bind(source).bind(folder).fetch_all(&mut *connection).await;
            Database::clear_progress(&mut connection).await?;
            return Ok(result?);
        }
        let result=sqlx::query_scalar::<_,MoodKey>(
            "WITH rows AS (SELECT mood.mood_key,mood.sort_text,
               count(DISTINCT track.track_key) track_count,COALESCE(sum(track.duration_millis),0) duration
              FROM moods mood LEFT JOIN track_moods credit USING(mood_key)
              LEFT JOIN tracks track USING(track_key) WHERE mood.source_key=?1 AND (?4 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?4)) GROUP BY mood.mood_key)
             SELECT mood_key FROM rows ORDER BY
              CASE WHEN ?2=0 AND ?3=0 THEN sort_text END ASC,
              CASE WHEN ?2=0 AND ?3=1 THEN sort_text END DESC,
              CASE WHEN ?2=1 AND ?3=0 THEN track_count END ASC,
              CASE WHEN ?2=1 AND ?3=1 THEN track_count END DESC,
              CASE WHEN ?2=2 AND ?3=0 THEN duration END ASC,
              CASE WHEN ?2=2 AND ?3=1 THEN duration END DESC,sort_text,mood_key")
            .bind(source).bind(sort.code()).bind(descending).bind(folder).fetch_all(&mut *connection).await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }
    pub async fn folder_order(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, FolderKey>(
            "SELECT folder_key FROM folders WHERE source_key=?1 ORDER BY sort_text, folder_key",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn album_track_order(
        &self,
        source: SourceKey,
        album: AlbumKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = collection_track_query(source, sort);
        query.push(" AND track.album_key=").push_bind(album);
        let result =
            finish_collection_track_order(query, folder, filter, sort, descending, &mut connection)
                .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn artist_track_order(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = collection_track_query(source, sort);
        query.push(" AND ((").push_bind(!album_artist).push(" AND EXISTS (SELECT 1 FROM track_artists relation WHERE relation.track_key=track.track_key AND relation.artist_key=").push_bind(artist).push(")) OR (").push_bind(album_artist).push(" AND EXISTS (SELECT 1 FROM album_artists relation WHERE relation.album_key=track.album_key AND relation.artist_key=").push_bind(artist).push(")))");
        let result =
            finish_collection_track_order(query, folder, filter, sort, descending, &mut connection)
                .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn genre_track_order(
        &self,
        source: SourceKey,
        genre: GenreKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = collection_track_query(source, sort);
        query.push(" AND EXISTS (SELECT 1 FROM track_genres relation WHERE relation.track_key=track.track_key AND relation.genre_key=").push_bind(genre).push(")");
        let result =
            finish_collection_track_order(query, folder, filter, sort, descending, &mut connection)
                .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn mood_track_order(
        &self,
        source: SourceKey,
        mood: MoodKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: TrackSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut query = collection_track_query(source, sort);
        query.push(" AND EXISTS (SELECT 1 FROM track_moods relation WHERE relation.track_key=track.track_key AND relation.mood_key=").push_bind(mood).push(")");
        let result =
            finish_collection_track_order(query, folder, filter, sort, descending, &mut connection)
                .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
    }

    pub async fn artist_album_order(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        appears_on: bool,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let result = sqlx::query_scalar::<_, AlbumKey>(
            "SELECT DISTINCT album.album_key
             FROM albums AS album
             WHERE album.source_key=?1 AND (
                 (?3=0 AND EXISTS (
                     SELECT 1 FROM album_artists AS direct
                     WHERE direct.album_key=album.album_key AND direct.artist_key=?2
                 )) OR (?3=1 AND NOT EXISTS (
                     SELECT 1 FROM album_artists AS direct
                     WHERE direct.album_key=album.album_key AND direct.artist_key=?2
                 ) AND EXISTS (
                     SELECT 1 FROM tracks AS track
                     JOIN track_artists AS credit USING(track_key)
                     WHERE track.album_key=album.album_key AND credit.artist_key=?2
                 ))
             ) AND (?4 IS NULL OR EXISTS (SELECT 1 FROM tracks scope_track JOIN track_folders scope USING(track_key) WHERE scope_track.album_key=album.album_key AND scope.folder_key=?4)) ORDER BY album.sort_text, album.album_key",
        )
        .bind(source)
        .bind(artist)
        .bind(appears_on)
        .bind(folder)
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
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(genre_key, position) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
            row.push_bind(*key).push_bind(position as i64);
        });
        query.push(
            ") SELECT genre.genre_key, genre.source_key, genre.name,
                      genre.artwork_binding,
                      count(DISTINCT track.album_key) AS album_count,
                      count(DISTINCT track.track_key) AS track_count,
                      COALESCE(sum(track.duration_millis), 0) AS duration_millis
               FROM requested JOIN genres AS genre USING(genre_key)
               LEFT JOIN track_genres AS relation USING(genre_key)
               LEFT JOIN tracks AS track USING(track_key)
               WHERE genre.source_key=",
        );
        query
            .push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push("))")
            .push(" GROUP BY genre.genre_key ORDER BY requested.position");
        let result = query
            .build_query_as::<GenreRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
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
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(mood_key, position) AS (");
        query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
            row.push_bind(*key).push_bind(position as i64);
        });
        query.push(
            ") SELECT mood.mood_key, mood.source_key, mood.name,
                      count(DISTINCT track.track_key) AS track_count,
                      COALESCE(sum(track.duration_millis), 0) AS duration_millis
               FROM requested JOIN moods AS mood USING(mood_key)
               LEFT JOIN track_moods AS relation USING(mood_key)
               LEFT JOIN tracks AS track USING(track_key)
               WHERE mood.source_key=",
        );
        query
            .push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push("))")
            .push(" GROUP BY mood.mood_key ORDER BY requested.position");
        let result = query
            .build_query_as::<MoodRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await;
        Database::clear_progress(&mut connection).await?;
        Ok(result?)
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
            ") SELECT folder.folder_key, folder.source_key, folder.name,
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

    pub async fn album_detail(
        &self,
        source: SourceKey,
        key: AlbumKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<AlbumDetail>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let album = load_album_rows(&mut transaction, source, &[key], folder)
            .await?
            .pop();
        let Some(album) = album else {
            transaction.commit().await?;
            Database::clear_progress(&mut connection).await?;
            return Ok(None);
        };
        let track_order = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks WHERE source_key=?1 AND album_key=?2
             AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3))
             ORDER BY disc_number, track_number, sort_text, track_key",
        )
        .bind(source)
        .bind(key)
        .bind(folder)
        .fetch_all(&mut *transaction)
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
            track_order,
            artists,
            genres,
            release_types,
        }))
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
            "SELECT genre.genre_key,genre.source_key,genre.name,genre.artwork_binding,
              count(DISTINCT track.album_key) album_count,
              count(DISTINCT track.track_key) track_count,
              COALESCE(sum(track.duration_millis),0) duration_millis
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
              COALESCE(sum(track.duration_millis),0) duration_millis
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
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE albums SET title=?3, normalized_title=?4,
                    display_artist=?5, sort_text=?6, year=?7,
                    release_date=?8, date_added=?9,
                    musicbrainz_release_id=?10,
                    musicbrainz_release_group_id=?11,
                    is_compilation=?12
             WHERE source_key=?1 AND album_key=?2",
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
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn update_artist_metadata(
        &self,
        source: SourceKey,
        key: ArtistKey,
        write: ArtistMetadataWrite,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query(
            "UPDATE artists SET name=?3, normalized_name=?4,
                    sort_text=?5, musicbrainz_artist_id=?6
             WHERE source_key=?1 AND artist_key=?2",
        )
        .bind(source)
        .bind(key)
        .bind(write.name)
        .bind(write.normalized_name)
        .bind(write.sort_text)
        .bind(write.musicbrainz_artist_id)
        .execute(connection)
        .await?
        .rows_affected()
            == 1)
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
    query.push(
        ") SELECT artist.artist_key, artist.source_key, artist.object_id, artist.name,
                  artist.musicbrainz_artist_id, artist.artwork_binding,
                  COALESCE(artist.user_favorite, artist.source_favorite) AS favorite,
                  COALESCE(artist.user_rating, artist.source_rating) / 10 AS rating,
                  0 AS play_count,
                  NULL AS last_played,
                  0 AS album_count,
                  0 AS track_count,
                  0 AS duration_millis
           FROM requested JOIN artists AS artist USING(artist_key)
           WHERE artist.source_key=",
    );
    query.push_bind(source).push(" ORDER BY requested.position");
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
        }
    }
    Ok(result)
}

fn row_limit(kind: &str) -> LibraryError {
    LibraryError::InvalidRequest(format!(
        "{kind} row reads are limited to {COLLECTION_ROW_LIMIT} keys"
    ))
}

fn collection_track_query(source: SourceKey, sort: TrackSort) -> QueryBuilder<Sqlite> {
    if sort.uses_activity() {
        let mut query = QueryBuilder::<Sqlite>::new(
            "WITH listen_activity AS (SELECT track_key,count(*) play_count,max(started_at) last_played FROM listens WHERE source_key=",
        );
        query.push_bind(source).push(" AND track_key IS NOT NULL GROUP BY track_key),activity AS (SELECT track.track_key,COALESCE(baseline.play_count,0)+COALESCE(listen.play_count,0) play_count,CASE WHEN baseline.last_played_at IS NULL THEN listen.last_played WHEN listen.last_played IS NULL THEN baseline.last_played_at ELSE max(baseline.last_played_at,listen.last_played) END last_played FROM tracks track LEFT JOIN activity_baseline baseline ON baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id LEFT JOIN listen_activity listen USING(track_key) WHERE track.source_key=").push_bind(source).push(") SELECT track.track_key FROM tracks track JOIN activity USING(track_key) WHERE track.source_key=").push_bind(source);
        query
    } else {
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT track.track_key FROM tracks track WHERE track.source_key=",
        );
        query.push_bind(source);
        query
    }
}

async fn finish_collection_track_order(
    mut query: QueryBuilder<Sqlite>,
    folder: Option<FolderKey>,
    filter: &str,
    sort: TrackSort,
    descending: bool,
    connection: &mut SqliteConnection,
) -> LibraryResult<Vec<TrackKey>> {
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    query.push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) AND (").push_bind(filter.is_empty()).push(" OR instr(track.normalized_search,").push_bind(&filter).push(")>0 OR CAST(track.year AS TEXT)=").push_bind(&filter).push(") ORDER BY ").push(sort.order_sql(descending)).push(",track.track_key");
    Ok(query
        .build_query_scalar::<TrackKey>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?)
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
    query.push("),listen_activity AS (SELECT track_key,count(*) play_count,max(started_at) last_played FROM listens WHERE source_key=").push_bind(source).push(" AND track_key IS NOT NULL GROUP BY track_key) SELECT requested.artist_key,count(DISTINCT item.album_key) album_count,count(DISTINCT item.track_key) track_count,COALESCE(sum(item.duration_millis),0) duration_millis,COALESCE(sum(COALESCE(baseline.play_count,0)+COALESCE(listen.play_count,0)),0) play_count,max(CASE WHEN baseline.last_played_at IS NULL THEN listen.last_played WHEN listen.last_played IS NULL THEN baseline.last_played_at ELSE max(baseline.last_played_at,listen.last_played) END) last_played FROM requested LEFT JOIN tracks item ON item.source_key=").push_bind(source).push(" AND ((").push_bind(!album_artist).push(" AND EXISTS (SELECT 1 FROM track_artists credit WHERE credit.track_key=item.track_key AND credit.artist_key=requested.artist_key)) OR (").push_bind(album_artist).push(" AND EXISTS (SELECT 1 FROM album_artists credit WHERE credit.album_key=item.album_key AND credit.artist_key=requested.artist_key))) AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=item.track_key AND scope.folder_key=").push_bind(folder).push(")) LEFT JOIN activity_baseline baseline ON baseline.source_key=item.source_key AND baseline.track_object_id=item.object_id LEFT JOIN listen_activity listen USING(track_key) GROUP BY requested.artist_key ORDER BY requested.position");
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
    query.push(") SELECT album.album_key,album.source_key,album.object_id,album.title,album.display_artist,album.year,album.release_date,album.date_added,album.musicbrainz_release_id,album.musicbrainz_release_group_id,album.is_compilation,album.release_lookup_identity,album.artwork_binding,COALESCE(album.user_favorite,album.source_favorite) favorite,COALESCE(album.user_rating,album.source_rating)/10 rating,COALESCE((SELECT sum(baseline.play_count) FROM activity_baseline baseline JOIN tracks item ON item.source_key=baseline.source_key AND item.object_id=baseline.track_object_id WHERE item.album_key=album.album_key),0)+(SELECT count(*) FROM listens listen JOIN tracks item USING(track_key) WHERE item.album_key=album.album_key) play_count,(SELECT max(played_at) FROM (SELECT baseline.last_played_at played_at FROM activity_baseline baseline JOIN tracks item ON item.source_key=baseline.source_key AND item.object_id=baseline.track_object_id WHERE item.album_key=album.album_key UNION ALL SELECT listen.started_at FROM listens listen JOIN tracks item USING(track_key) WHERE item.album_key=album.album_key)) last_played,count(track.track_key) track_count,COALESCE(sum(track.duration_millis),0) duration_millis FROM requested JOIN albums album USING(album_key) LEFT JOIN tracks track USING(album_key) WHERE album.source_key=");
    query.push_bind(source).push(" AND (").push_bind(folder).push(" IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=").push_bind(folder).push(")) GROUP BY album.album_key ORDER BY requested.position");
    let mut scalars = query
        .build_query_as::<AlbumScalar>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    if let Some(folder) = folder {
        let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key) AS (");
        query.push_values(keys, |mut row, key| {
            row.push_bind(*key);
        });
        query.push(") SELECT requested.album_key,COALESCE((SELECT sum(baseline.play_count) FROM activity_baseline baseline JOIN tracks track ON track.source_key=baseline.source_key AND track.object_id=baseline.track_object_id JOIN track_folders scope USING(track_key) WHERE track.album_key=requested.album_key AND scope.folder_key=").push_bind(folder).push("),0)+(SELECT count(*) FROM listens listen JOIN tracks track USING(track_key) JOIN track_folders scope USING(track_key) WHERE track.album_key=requested.album_key AND scope.folder_key=").push_bind(folder).push(") play_count,(SELECT max(played_at) FROM (SELECT baseline.last_played_at played_at FROM activity_baseline baseline JOIN tracks track ON track.source_key=baseline.source_key AND track.object_id=baseline.track_object_id JOIN track_folders scope USING(track_key) WHERE track.album_key=requested.album_key AND scope.folder_key=").push_bind(folder).push(" UNION ALL SELECT listen.started_at FROM listens listen JOIN tracks track USING(track_key) JOIN track_folders scope USING(track_key) WHERE track.album_key=requested.album_key AND scope.folder_key=").push_bind(folder).push(")) last_played FROM requested");
        let activity = query
            .build_query_as::<AlbumActivityRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?;
        let activity = activity
            .into_iter()
            .map(|row| (row.album_key, (row.play_count, row.last_played)))
            .collect::<BTreeMap<_, _>>();
        for scalar in &mut scalars {
            if let Some((play_count, last_played)) = activity.get(&scalar.album_key) {
                scalar.play_count = *play_count;
                scalar.last_played = *last_played;
            }
        }
    }
    let mut album_artists = BTreeMap::<AlbumKey, Vec<AlbumArtistLink>>::new();
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("),unique_requested AS (SELECT album_key,min(position) position FROM requested GROUP BY album_key) SELECT relation.album_key,artist.artist_key,artist.name FROM unique_requested requested JOIN album_artists relation USING(album_key) JOIN artists artist USING(artist_key) ORDER BY requested.position,relation.position");
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
            album_artists: album_artists.get(&key).cloned().unwrap_or_default(),
            genres: genres.get(&key).cloned().unwrap_or_default(),
            release_types: release_types.get(&key).cloned().unwrap_or_default(),
        });
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
