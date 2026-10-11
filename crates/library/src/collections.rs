//! Owns Album, Artist, Genre, Mood, and Folder orders, rows, and details.
//! Collection membership, filtering, and sorting are resolved in one Store operation.

use std::collections::BTreeMap;

use sqlx::sqlite::SqliteRow;
use sqlx::{Connection, FromRow, QueryBuilder, Row, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult, MoodKey,
    ReadCancellation, RouteSeedWindow, SourceKey, TrackKey, TrackRow, TrackSort,
    tracks::load_track_rows,
};

const COLLECTION_ROW_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "rows are held only for a visible page"
)]
pub enum AlbumDetailRow {
    Album {
        album: AlbumRow,
        tracks: Vec<TrackRow>,
        last: bool,
    },
    Track {
        track: TrackRow,
        index: usize,
        last: bool,
    },
}

#[derive(Clone, Debug, FromRow)]
pub struct GenreChoice {
    pub genre_key: GenreKey,
    pub object_id: String,
    pub name: String,
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
    fn order_sql(self, descending: bool, _folder: Option<FolderKey>) -> String {
        let field = match self {
            Self::Title => "album.sort_text".to_string(),
            Self::AlbumArtist => "album.album_artist_sort".to_string(),
            Self::Year => "album.year".to_string(),
            Self::ReleaseDate => "album.release_date".to_string(),
            Self::DateAdded => crate::tracks::added_at!("album").to_string(),
            Self::LastPlayed => collection_stat("album", "last_played", ""),
            Self::PlayCount => collection_stat("album", "play_count", ""),
            Self::Rating => "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_rating)".to_string(),
            Self::TrackCount => "album.track_count".into(),
            Self::Duration => "album.duration_millis".into(),
            Self::Favorite => "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)".to_string(),
        };
        let direction = if descending { "DESC" } else { "ASC" };
        if self == Self::Title {
            format!("{field} {direction},album.album_key")
        } else if self == Self::AlbumArtist {
            format!(
                "(album.album_artist_sort IS NULL),{field} {direction},(album.year IS NULL),album.year,album.sort_text,album.album_key"
            )
        } else {
            format!("{field} {direction} NULLS LAST,album.sort_text,album.album_key")
        }
    }
}

fn collection_stat(entity: &str, field: &str, role: &str) -> String {
    format!(
        "CASE WHEN (SELECT kind FROM sources WHERE source_key={entity}.source_key) IN ('local','files','webdav','smb','documents') THEN {entity}.{role}activity_{field} ELSE COALESCE({entity}.source_{field},{entity}.{role}derived_source_{field}) END"
    )
}

fn album_track_scope(folder: Option<FolderKey>) -> String {
    let mut scope = "item.indexed=1 AND item.album_key=album.album_key".to_string();
    if let Some(folder) = folder {
        scope.push_str(&format!(" AND EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=item.track_key AND scope.folder_key={})", folder.raw()));
    }
    scope
}

fn album_scope(
    source: SourceKey,
    folder: Option<FolderKey>,
    favorites_only: bool,
    filter: &str,
) -> String {
    let mut scope = format!("album.indexed=1 AND album.source_key={}", source.raw());
    if let Some(folder) = folder {
        scope.push_str(&format!(
            " AND album.album_key IN (SELECT item.album_key FROM track_folders scope JOIN tracks item USING(track_key) WHERE item.indexed=1 AND scope.folder_key={})",
            folder.raw()
        ));
    }
    if favorites_only {
        scope.push_str(" AND COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=album.media_uri),album.source_favorite)=1");
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        let filter = crate::source_window::quote(&filter);
        scope.push_str(&format!(" AND (instr(album.normalized_title || ' ' || lower(album.display_artist),{filter})>0 OR CAST(album.year AS TEXT)={filter} OR EXISTS(SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,{filter})>0) OR EXISTS(SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,{filter})>0))"));
    }
    scope
}

fn named_scope(kind: &str, source: SourceKey, folder: Option<FolderKey>, filter: &str) -> String {
    let mut scope = format!("{kind}.indexed=1 AND {kind}.source_key={}", source.raw());
    if let Some(folder) = folder {
        scope.push_str(&format!(
            " AND (EXISTS(SELECT 1 FROM track_{kind}s credit JOIN tracks track USING(track_key) JOIN track_folders scope USING(track_key) WHERE credit.{kind}_key={kind}.{kind}_key AND track.indexed=1 AND scope.folder_key={})", folder.raw()
        ));
        if kind == "genre" {
            scope.push_str(&format!(" OR EXISTS(SELECT 1 FROM album_genres credit JOIN tracks track USING(album_key) JOIN track_folders scope USING(track_key) WHERE credit.genre_key=genre.genre_key AND track.indexed=1 AND scope.folder_key={})", folder.raw()));
        }
        scope.push(')');
    } else if kind == "genre" {
        scope.push_str(" AND genre.track_count>0");
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        scope.push_str(&format!(
            " AND instr({kind}.normalized_name,{})>0",
            crate::source_window::quote(&filter)
        ));
    }
    scope
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize)]
pub struct AlbumArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize)]
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

impl<'row> FromRow<'row, SqliteRow> for AlbumRow {
    fn from_row(row: &'row SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            album_key: row.try_get("album_key")?,
            source_key: row.try_get("source_key")?,
            object_id: row.try_get("object_id")?,
            media_uri: row.try_get("media_uri")?,
            title: row.try_get("title")?,
            display_artist: row.try_get("display_artist")?,
            year: row.try_get("year")?,
            release_date: row.try_get("release_date")?,
            date_added: row.try_get("date_added")?,
            musicbrainz_release_id: row.try_get("musicbrainz_release_id")?,
            musicbrainz_release_group_id: row.try_get("musicbrainz_release_group_id")?,
            is_compilation: row.try_get("is_compilation")?,
            release_lookup_identity: row.try_get("release_lookup_identity")?,
            artwork_binding: row.try_get("artwork_binding")?,
            favorite: row.try_get("favorite")?,
            rating: row.try_get("rating")?,
            play_count: row.try_get("play_count")?,
            last_played: row.try_get("last_played")?,
            track_count: row.try_get("track_count")?,
            duration_millis: row.try_get("duration_millis")?,
            downloaded_count: row.try_get("downloaded_count")?,
            album_artists: serde_json::from_str(row.try_get("album_artists")?)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            genres: serde_json::from_str(row.try_get("genres")?)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            release_types: serde_json::from_str(row.try_get("release_types")?)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
        })
    }
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
    pub source_id: String,
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
            source_id: row.try_get("source_id")?,
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

fn release_class_code(class: AlbumReleaseClass) -> i64 {
    match class {
        AlbumReleaseClass::Album => 0,
        AlbumReleaseClass::Ep => 1,
        AlbumReleaseClass::Single => 2,
        AlbumReleaseClass::Collection => 3,
        AlbumReleaseClass::Other => 4,
    }
}

fn release_class_from_code(code: i64) -> AlbumReleaseClass {
    match code {
        0 => AlbumReleaseClass::Album,
        1 => AlbumReleaseClass::Ep,
        2 => AlbumReleaseClass::Single,
        3 => AlbumReleaseClass::Collection,
        4 => AlbumReleaseClass::Other,
        _ => unreachable!("Release classification"),
    }
}

fn album_release_cte(scope: &str) -> String {
    // Release tags may contain legacy pipe separators and Unicode whitespace.
    let token = "trim(item.value,char(9,10,11,12,13,32,133,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288))";
    format!("candidates AS MATERIALIZED ({scope}), release_facts AS (
        SELECT candidates.album_key,count(NULLIF({token},'')) types_count,
          max({token}='album') has_album,
          max({token} IN ('compilation','compilations','collection','collections')) has_collection,
          max({token} IN ('ep','e.p.')) has_ep,max({token}='single') has_single
        FROM candidates LEFT JOIN album_release_types types USING(album_key)
        LEFT JOIN json_each('['||replace(json_quote(lower(types.release_type)),'|','\",\"')||']') item
        GROUP BY candidates.album_key
      ), classified AS (
        SELECT album.album_key,CASE WHEN coalesce(album.is_compilation,0)<>0 THEN 3
          WHEN release_facts.types_count=0 OR release_facts.has_album THEN 0
          WHEN release_facts.has_collection THEN 3 WHEN release_facts.has_ep THEN 1
          WHEN release_facts.has_single THEN 2 ELSE 4 END class_code
        FROM candidates JOIN albums album USING(album_key) JOIN release_facts USING(album_key)
      )")
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

fn artist_scope(
    source: SourceKey,
    folder: Option<FolderKey>,
    album_artist: bool,
    favorites_only: bool,
    filter: &str,
) -> String {
    let mut scope = format!("artist.indexed=1 AND artist.source_key={}", source.raw());
    scope.push_str(if album_artist {
        " AND EXISTS(SELECT 1 FROM album_artists role JOIN albums album USING(album_key) WHERE album.indexed=1 AND role.artist_key=artist.artist_key)"
    } else {
        " AND artist.track_count>0"
    });
    if let Some(folder) = folder {
        let members = if album_artist {
            "album_artists role JOIN tracks track USING(album_key)"
        } else {
            "track_artists role JOIN tracks track USING(track_key)"
        };
        scope.push_str(&format!(" AND EXISTS(SELECT 1 FROM {members} JOIN track_folders scope USING(track_key) WHERE track.indexed=1 AND role.artist_key=artist.artist_key AND scope.folder_key={})", folder.raw()));
    }
    if favorites_only {
        scope.push_str(" AND COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)=1");
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        scope.push_str(&format!(
            " AND instr(artist.normalized_name,{})>0",
            crate::source_window::quote(&filter)
        ));
    }
    scope
}

impl Database {
    pub async fn album_metadata_links(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(String, String, Vec<crate::TrackArtistLink>)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let row = sqlx::query_as::<_, (String, String, String)>(
            "SELECT album.title,album.display_artist,
                (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
                 FROM album_artists credit JOIN artists artist USING(artist_key)
                 WHERE credit.album_key=album.album_key)
             FROM albums album WHERE album.media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?;
        row.map(|(title, artist, credits)| Ok((title, artist, serde_json::from_str(&credits)?)))
            .transpose()
    }

    pub async fn artist_name_by_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<String>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_scalar("SELECT name FROM artists WHERE media_uri=?1")
                .bind(media_uri)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn ordered_name_sections<'a>(
        &self,
        names: impl Iterator<Item = &'a str>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        let mut encoded = String::from("[");
        for (index, name) in names.enumerate() {
            if index != 0 {
                encoded.push(',');
            }
            encoded.push_str(&serde_json::to_string(name)?);
        }
        encoded.push(']');
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let sql = crate::scroll_sections::section_sql(
            "SELECT value FROM json_each(?1) item ORDER BY item.key",
            "item.value",
        );
        crate::scroll_sections::decode_sections(
            sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(encoded)
                .fetch_all(&mut *connection)
                .await?,
        )
    }
    pub async fn folder_section_positions(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        filter: &str,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let direction = if descending { "DESC" } else { "ASC" };
        let sql = crate::scroll_sections::section_sql(
            &format!(
                "SELECT folder_key FROM ({FILTERED_FOLDER_CHILDREN_SQL}) ORDER BY sort_text {direction},folder_key"
            ),
            "sort_text",
        );
        crate::scroll_sections::decode_sections(
            sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(source)
                .bind(parent)
                .bind(filter.trim())
                .persistent(false)
                .fetch_all(&mut *connection)
                .await?,
        )
    }
    pub async fn mood_section_positions(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: MoodSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        if sort != MoodSort::Title {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::scroll_sections::decode_sections(
            load_mood_order::<String>(
                &mut connection,
                source,
                folder,
                filter,
                sort,
                descending,
                0,
                usize::MAX,
                true,
            )
            .await?,
        )
    }

    pub async fn album_section_positions(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        if !matches!(sort, AlbumSort::Title | AlbumSort::AlbumArtist) {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::scroll_sections::decode_sections(
            load_album_order::<String>(
                &mut connection,
                source,
                folder,
                favorites_only,
                filter,
                sort,
                descending,
                None,
                true,
            )
            .await?,
        )
    }

    pub async fn artist_section_positions(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists_only: bool,
        favorites_only: bool,
        filter: &str,
        sort: ArtistSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        if sort != ArtistSort::Title {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::scroll_sections::decode_sections(
            load_artist_order::<String>(
                &mut connection,
                source,
                folder,
                album_artists_only,
                favorites_only,
                filter,
                sort,
                descending,
                None,
                true,
            )
            .await?,
        )
    }

    pub async fn genre_section_positions(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: GenreSort,
        descending: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        if sort != GenreSort::Title {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        crate::scroll_sections::decode_sections(
            load_genre_order::<String>(
                &mut connection,
                source,
                folder,
                filter,
                sort,
                descending,
                None,
                true,
            )
            .await?,
        )
    }
    pub async fn genre_choices(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreChoice>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT genre.genre_key,genre.object_id,genre.name FROM genres genre WHERE {} ORDER BY genre.sort_text,genre.genre_key",
            named_scope("genre",source,folder,"")
        ))).fetch_all(&mut *connection).await?)
    }

    pub async fn album_count(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: AlbumSort,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let _ = sort;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM albums album WHERE {}",
            album_scope(source, folder, favorites_only, filter)
        )))
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn artist_count(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists_only: bool,
        favorites_only: bool,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM artists artist WHERE {}",
            artist_scope(source, folder, album_artists_only, favorites_only, filter)
        )))
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn genre_count(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM genres genre WHERE {}",
            named_scope("genre", source, folder, filter)
        )))
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn collection_source(
        &self,
        scope: &crate::SongScope,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SourceKey>> {
        use crate::SongScope as C;
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let query = match scope {
            C::Tracks(key) | C::Objects { source: key, .. } => return Ok(Some(*key)),
            C::Smart { source, .. } => return Ok(*source),
            C::History { source } => {
                sqlx::query_scalar("SELECT source_key FROM sources WHERE object_id=?1")
                    .bind(source.as_ref().map(crate::SourceId::as_str))
            }
            C::Album(key) => {
                sqlx::query_scalar("SELECT source_key FROM albums WHERE album_key=?1").bind(key)
            }
            C::Artist { key, .. } => {
                sqlx::query_scalar("SELECT source_key FROM artists WHERE artist_key=?1").bind(key)
            }
            C::Genre(key) => {
                sqlx::query_scalar("SELECT source_key FROM genres WHERE genre_key=?1").bind(key)
            }
            C::Mood(key) => {
                sqlx::query_scalar("SELECT source_key FROM moods WHERE mood_key=?1").bind(key)
            }
            C::Playlist(key) => sqlx::query_scalar(
                "SELECT source_key FROM playlists WHERE playlist_key=?1 AND source_key IS NOT NULL",
            )
            .bind(key),
        };
        Ok(query.fetch_optional(&mut *connection).await?)
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
        Ok(sqlx::query_scalar::<_, String>(
            concat!("WITH latest AS (
               SELECT album.album_key FROM albums album
               WHERE album.indexed=1 AND album.source_key=?1
                 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.indexed=1 AND track.album_key=album.album_key AND scope.folder_key=?2))
               ORDER BY ",crate::tracks::added_at!("album")," DESC NULLS LAST,album.album_key LIMIT ?3
             )
             SELECT track.media_uri FROM latest JOIN tracks track USING(album_key)
             WHERE track.indexed=1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
             ORDER BY latest.album_key,track.disc_number,track.track_number,track.track_key"),
        )
        .bind(source)
        .bind(folder)
        .bind(album_limit)
        .fetch_all(&mut *connection)
        .await?)
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
        Ok(sqlx::query_as::<_,AlbumReleaseCandidate>("SELECT album_key,media_uri,CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id ELSE 'release:'||musicbrainz_release_id END lookup_identity FROM albums WHERE indexed=1 AND source_key=?1 AND (musicbrainz_release_group_id IS NOT NULL OR musicbrainz_release_id IS NOT NULL) AND (release_lookup_identity IS NULL OR release_lookup_identity<>CASE WHEN musicbrainz_release_group_id IS NOT NULL THEN 'release-group:'||musicbrainz_release_group_id ELSE 'release:'||musicbrainz_release_id END) ORDER BY album_key LIMIT ?2")
            .bind(source).bind(limit).fetch_all(&mut *connection).await?)
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
    ) -> LibraryResult<(usize, usize, Vec<AlbumRow>)> {
        let count = self
            .album_count(source, folder, favorites_only, filter, sort, cancellation)
            .await?
            .max(0) as usize;
        let range = window.range(count);
        let rows = self
            .album_page(
                source,
                folder,
                favorites_only,
                filter,
                sort,
                descending,
                range.start,
                range.len(),
                cancellation,
            )
            .await?;
        Ok((count, range.start, rows))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn album_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order = load_album_order::<AlbumKey>(
            &mut transaction,
            source,
            folder,
            favorites_only,
            filter,
            sort,
            descending,
            Some((offset, limit)),
            false,
        )
        .await?;
        let rows = load_album_rows(&mut transaction, source, &order, folder).await?;
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn album_detail_count(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        inline_tracks: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COALESCE(sum(1+max({}-{},0)),0) FROM albums album WHERE {}",
            album_detail_track_count(folder),
            inline_tracks,
            album_scope(source, folder, favorites_only, filter)
        )))
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn album_detail_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        favorites_only: bool,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        inline_tracks: usize,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumDetailRow>> {
        let offset = offset.min(i64::MAX as usize);
        let end = offset
            .saturating_add(limit.min(COLLECTION_ROW_LIMIT))
            .min(i64::MAX as usize);
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let spans = sqlx::query_as::<_, (AlbumKey, i64, i64)>(sqlx::AssertSqlSafe(format!(
            "{} SELECT album_key,track_count,first_position FROM spans
             WHERE first_position<?1 AND first_position+max(track_count-{inline_tracks},0)>=?2
             ORDER BY first_position",
            album_detail_spans_sql(
                source,
                folder,
                favorites_only,
                filter,
                sort,
                descending,
                inline_tracks,
                end,
            )
        )))
        .bind(end.min(i64::MAX as usize) as i64)
        .bind(offset.min(i64::MAX as usize) as i64)
        .fetch_all(&mut *transaction)
        .await?;
        let mut items = Vec::new();
        let mut albums = Vec::new();
        let mut tracks = Vec::new();
        for (album, track_count, first_position) in spans {
            let first_position = first_position as usize;
            let track_count = track_count as usize;
            let header = first_position >= offset;
            if header {
                albums.push(album);
            }
            let first_track = first_position.saturating_add(1);
            let start = if header {
                0
            } else {
                inline_tracks.saturating_add(offset.saturating_sub(first_track))
            };
            let count = inline_tracks
                .saturating_add(end.saturating_sub(first_track))
                .min(track_count)
                .saturating_sub(start);
            let scope = album_track_scope(folder).replace("album.album_key", "?1");
            let keys = sqlx::query_scalar::<_, TrackKey>(sqlx::AssertSqlSafe(format!(
                "SELECT item.track_key FROM tracks item WHERE {scope}
                 ORDER BY item.disc_number,item.track_number,item.sort_text,item.track_key LIMIT ?2 OFFSET ?3"
            )))
            .bind(album)
            .bind(count as i64)
            .bind(start as i64)
            .fetch_all(&mut *transaction)
            .await?;
            tracks.extend(&keys);
            items.push((album, header, start, track_count, keys));
        }
        let mut albums = load_album_rows(&mut transaction, source, &albums, folder)
            .await?
            .into_iter()
            .map(|row| (row.album_key, row))
            .collect::<BTreeMap<_, _>>();
        let mut tracks = load_track_rows(&mut transaction, &tracks)
            .await?
            .into_iter()
            .map(|row| (row.track_key, row))
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::new();
        for (key, header, start, track_count, keys) in items {
            let mut loaded = keys.into_iter().filter_map(|key| tracks.remove(&key));
            if header && let Some(album) = albums.remove(&key) {
                rows.push(AlbumDetailRow::Album {
                    album,
                    tracks: loaded.by_ref().take(inline_tracks).collect(),
                    last: track_count <= inline_tracks,
                });
            }
            for (position, track) in loaded.enumerate() {
                let index = position + start.max(inline_tracks);
                rows.push(AlbumDetailRow::Track {
                    track,
                    index,
                    last: index + 1 == track_count,
                });
            }
        }
        transaction.commit().await?;
        Ok(rows)
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
    ) -> LibraryResult<(usize, usize, Vec<ArtistRow>)> {
        let count = self
            .artist_count(
                source,
                folder,
                album_artists_only,
                favorites_only,
                filter,
                cancellation,
            )
            .await?
            .max(0) as usize;
        let range = window.range(count);
        let rows = self
            .artist_page(
                source,
                folder,
                album_artists_only,
                favorites_only,
                filter,
                sort,
                descending,
                range.start,
                range.len(),
                cancellation,
            )
            .await?;
        Ok((count, range.start, rows))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn artist_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists_only: bool,
        favorites_only: bool,
        filter: &str,
        sort: ArtistSort,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ArtistRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let order = load_artist_order::<ArtistKey>(
            &mut transaction,
            source,
            folder,
            album_artists_only,
            favorites_only,
            filter,
            sort,
            descending,
            Some((offset, limit)),
            false,
        )
        .await?;
        let rows =
            load_artist_rows(&mut transaction, source, &order, album_artists_only, folder).await?;
        transaction.commit().await?;
        Ok(rows)
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
    ) -> LibraryResult<(usize, usize, Vec<GenreRow>)> {
        let count = self
            .genre_count(source, folder, filter, cancellation)
            .await?
            .max(0) as usize;
        let range = window.range(count);
        let rows = self
            .genre_page(
                source,
                folder,
                filter,
                sort,
                descending,
                range.start,
                range.len(),
                cancellation,
            )
            .await?;
        Ok((count, range.start, rows))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn genre_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: GenreSort,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let keys = load_genre_order::<GenreKey>(
            &mut transaction,
            source,
            folder,
            filter,
            sort,
            descending,
            Some((offset, limit)),
            false,
        )
        .await?;
        let rows = load_genre_rows(&mut transaction, source, &keys, folder).await?;
        transaction.commit().await?;
        Ok(rows)
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
    ) -> LibraryResult<(usize, usize, Vec<MoodRow>)> {
        let count = self
            .mood_count(source, folder, filter, cancellation)
            .await?
            .max(0) as usize;
        let range = window.range(count);
        let rows = self
            .mood_page(
                source,
                folder,
                filter,
                sort,
                descending,
                range.start,
                range.len(),
                cancellation,
            )
            .await?;
        Ok((count, range.start, rows))
    }

    pub async fn mood_count(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<i64> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM moods mood WHERE {}",
            named_scope("mood", source, folder, filter)
        )))
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn mood_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        filter: &str,
        sort: MoodSort,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<MoodRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let result = load_mood_order::<MoodKey>(
            &mut transaction,
            source,
            folder,
            filter,
            sort,
            descending,
            offset,
            limit,
            false,
        )
        .await?;
        let first_rows = load_mood_rows(&mut transaction, source, &result, folder).await?;
        transaction.commit().await?;
        Ok(first_rows)
    }
    pub async fn folder_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<FolderKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar::<_, FolderKey>(
            "SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn artist_release_counts(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        filter: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<[i64; 6]> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let scope = ARTIST_ALBUM_SCOPE;
        let sql = format!(
            "WITH {} SELECT class_code,count(*) FROM classified GROUP BY class_code",
            album_release_cte(scope)
        );
        let rows = sqlx::query_as::<_, (i64, i64)>(sqlx::AssertSqlSafe(sql))
            .bind(source)
            .bind(artist)
            .bind(album_artist)
            .bind(folder)
            .bind(filter.is_empty())
            .bind(filter)
            .fetch_all(&mut *connection)
            .await?;
        let mut counts = [0; 6];
        for (class, count) in rows {
            counts[class as usize] = count;
        }
        Ok(counts)
    }

    pub async fn artist_release_page(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        section: Option<AlbumReleaseClass>,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(AlbumReleaseClass, AlbumRow)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let scope = ARTIST_ALBUM_SCOPE;
        let order = sort.order_sql(descending, folder);
        let sql = format!(
            "WITH {} SELECT album.album_key,classified.class_code FROM classified JOIN albums album USING(album_key) WHERE (?7 IS NULL OR classified.class_code=?7) ORDER BY classified.class_code,{order} LIMIT ?8 OFFSET ?9",
            album_release_cte(scope)
        );
        let keys = sqlx::query_as::<_, (AlbumKey, i64)>(sqlx::AssertSqlSafe(sql))
            .bind(source)
            .bind(artist)
            .bind(album_artist)
            .bind(folder)
            .bind(filter.is_empty())
            .bind(filter)
            .bind(section.map(release_class_code))
            .bind(limit.min(COLLECTION_ROW_LIMIT) as i64)
            .bind(offset.min(i64::MAX as usize) as i64)
            .fetch_all(&mut *transaction)
            .await?;
        let albums = load_album_rows(
            &mut transaction,
            source,
            &keys.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            folder,
        )
        .await?;
        let rows = albums
            .into_iter()
            .zip(keys)
            .map(|(album, (_, class))| (release_class_from_code(class), album))
            .collect();
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn artist_release_scroll_sections(
        &self,
        source: SourceKey,
        artist: ArtistKey,
        album_artist: bool,
        folder: Option<FolderKey>,
        filter: &str,
        sort: AlbumSort,
        descending: bool,
        section: AlbumReleaseClass,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        if !matches!(sort, AlbumSort::Title | AlbumSort::AlbumArtist) {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let scope = ARTIST_ALBUM_SCOPE;
        let order = sort.order_sql(descending, folder);
        let ordered = format!(
            "WITH {} SELECT album.album_key FROM classified JOIN albums album USING(album_key) WHERE classified.class_code=?7 ORDER BY {order}",
            album_release_cte(scope)
        );
        let sql = crate::scroll_sections::section_sql(
            &ordered,
            if sort == AlbumSort::Title {
                "album.sort_text"
            } else {
                "album.album_artist_sort"
            },
        );
        crate::scroll_sections::decode_sections(
            sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(source)
                .bind(artist)
                .bind(album_artist)
                .bind(folder)
                .bind(filter.is_empty())
                .bind(filter)
                .bind(release_class_code(section))
                .fetch_all(&mut *connection)
                .await?,
        )
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
            return Ok(None);
        };
        let mut transaction = connection.begin().await?;
        let result = load_album_rows(&mut transaction, source, &[key], None)
            .await?
            .pop();
        transaction.commit().await?;
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
        result
    }

    pub async fn artist_identity_by_media_uri(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<(SourceKey, ArtistKey)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(
            sqlx::query_as("SELECT source_key,artist_key FROM artists WHERE media_uri=?1")
                .bind(media_uri)
                .fetch_optional(&mut *connection)
                .await?,
        )
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
            return Ok(None);
        };
        let mut transaction = connection.begin().await?;
        let result = load_artist_rows(&mut transaction, source, &[key], false, None)
            .await?
            .pop();
        transaction.commit().await?;
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
                      count(DISTINCT track.track_key) AS track_count
               FROM requested JOIN folders AS folder USING(folder_key)
               LEFT JOIN track_folders AS relation USING(folder_key)
               LEFT JOIN tracks track ON track.track_key=relation.track_key AND track.indexed=1
               WHERE folder.source_key=",
        );
        query
            .push_bind(source)
            .push(" GROUP BY folder.folder_key ORDER BY requested.position");
        Ok(query
            .build_query_as::<FolderRow>()
            .persistent(false)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn folder_child_order(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        load_folder_children(&mut connection, source, parent, None).await
    }

    pub async fn folder_page(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderRow>> {
        let keys = {
            let (_permit, mut connection) = self.acquire_general(cancellation).await?;
            load_folder_children(&mut connection, source, parent, Some((offset, limit))).await?
        };
        self.folder_rows(source, &keys, cancellation).await
    }

    pub async fn filtered_folder_page(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        filter: &str,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, Vec<FolderRow>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let direction = if descending { "DESC" } else { "ASC" };
        let children = FILTERED_FOLDER_CHILDREN_SQL;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM ({children})"
        )))
        .bind(source)
        .bind(parent)
        .bind(filter.trim())
        .fetch_one(&mut *connection)
        .await?;
        let keys = sqlx::query_scalar::<_, FolderKey>(sqlx::AssertSqlSafe(format!("SELECT folder_key FROM ({children}) ORDER BY sort_text {direction},folder_key LIMIT ?4 OFFSET ?5"))).bind(source).bind(parent).bind(filter.trim()).bind(limit.min(COLLECTION_ROW_LIMIT) as i64).bind(offset.min(i64::MAX as usize) as i64).fetch_all(&mut *connection).await?;
        drop(connection);
        drop(_permit);
        Ok((
            count.max(0) as usize,
            self.folder_rows(source, &keys, cancellation).await?,
        ))
    }

    pub async fn genre_detail(
        &self,
        source: SourceKey,
        key: GenreKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<GenreRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let genre = load_genre_rows(&mut transaction, source, &[key], folder)
            .await?
            .pop();
        transaction.commit().await?;
        Ok(genre)
    }

    pub async fn mood_detail(
        &self,
        source: SourceKey,
        key: MoodKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<MoodRow>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mood = load_mood_rows(&mut transaction, source, &[key], folder)
            .await?
            .pop();
        transaction.commit().await?;
        Ok(mood)
    }
}

pub(crate) async fn load_artist_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[ArtistKey],
    album_artist: bool,
    _folder: Option<FolderKey>,
) -> LibraryResult<Vec<ArtistRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(artist_key,position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    let role = if album_artist { "album_artist_" } else { "" };
    query.push(format!(")
        SELECT artist.artist_key,artist.source_key,artist.object_id,artist.media_uri,artist.name,
          artist.musicbrainz_artist_id,artist.artwork_binding,
          COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=artist.media_uri),artist.source_favorite) favorite,
          COALESCE((SELECT rating FROM user_media_state WHERE media_uri=artist.media_uri),artist.source_rating)/10 rating,
          {} play_count,{} last_played,artist.{role}album_count album_count,artist.{role}track_count track_count,artist.{role}duration_millis duration_millis,artist.{role}downloaded_count downloaded_count
        FROM requested JOIN artists artist USING(artist_key)
        WHERE artist.source_key=", collection_stat("artist","play_count",role), collection_stat("artist","last_played",role)));
    query.push_bind(source).push(" ORDER BY requested.position");
    Ok(query
        .build_query_as::<ArtistRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?)
}

fn row_limit(kind: &str) -> LibraryError {
    LibraryError::InvalidRequest(format!(
        "{kind} row reads are limited to {COLLECTION_ROW_LIMIT} keys"
    ))
}

pub(crate) async fn album_disc_sections(
    query: &crate::source_window::SourceQuery,
    sort: TrackSort,
    descending: bool,
    connection: &mut SqliteConnection,
) -> LibraryResult<Vec<(u32, i64)>> {
    if sort != TrackSort::TrackNumber {
        return Ok(Vec::new());
    }
    let direction = if descending { "DESC" } else { "ASC" };
    let counts = sqlx::query_as_with::<_, (i64, i64), _>(sqlx::AssertSqlSafe(format!(
        "SELECT track.disc_number,COUNT(*) FROM {} WHERE {} GROUP BY track.disc_number ORDER BY track.disc_number {direction}",
        query.from, query.predicate,
    )), query.arguments.clone()).fetch_all(connection).await?;
    if counts.iter().filter(|(disc, _)| *disc > 0).count() <= 1 {
        return Ok(Vec::new());
    }
    let mut position = 0;
    Ok(counts
        .into_iter()
        .map(|(disc, count)| {
            let start = position;
            position += count as u32;
            (start, disc)
        })
        .collect())
}

pub(crate) async fn load_album_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[AlbumKey],
    _folder: Option<FolderKey>,
) -> LibraryResult<Vec<AlbumRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(album_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push("      ) SELECT album.album_key,album.source_key,album.object_id,album.media_uri,album.title,
          album.display_artist,album.year,album.release_date,date(").push(crate::tracks::added_at!("album")).push(",'unixepoch') date_added,
          album.musicbrainz_release_id,album.musicbrainz_release_group_id,album.is_compilation,
          album.release_lookup_identity,album.artwork_binding,
          COALESCE((SELECT state.favorite FROM user_media_state state
                    WHERE state.media_uri=album.media_uri),album.source_favorite) favorite,
          COALESCE((SELECT state.rating FROM user_media_state state
                    WHERE state.media_uri=album.media_uri),album.source_rating)/10 rating,
          ").push(collection_stat("album","play_count","")).push(" play_count,
          ").push(collection_stat("album","last_played","")).push(" last_played,
          album.track_count,
          album.duration_millis,
          album.downloaded_count,
          (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY relation.position)
           FROM album_artists relation JOIN artists artist USING(artist_key)
           WHERE relation.album_key=album.album_key) album_artists,
          (SELECT json_group_array(json_object('genre_key',genre.genre_key,'name',genre.name) ORDER BY relation.position)
           FROM album_genres relation JOIN genres genre USING(genre_key)
           WHERE relation.album_key=album.album_key) genres,
          (SELECT json_group_array(relation.release_type ORDER BY relation.position)
           FROM album_release_types relation WHERE relation.album_key=album.album_key) release_types
        FROM requested JOIN albums album USING(album_key)
        WHERE album.source_key=").push_bind(source).push(" ORDER BY requested.position");
    Ok(query
        .build_query_as::<AlbumRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?)
}

async fn named_collection_artwork(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    keys: &[impl serde::Serialize],
    kind: &str,
) -> LibraryResult<BTreeMap<i64, Vec<Vec<u8>>>> {
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let (from, predicate) = if kind == "genre" {
        (
            "(SELECT track.track_key FROM track_genres credit JOIN tracks track USING(track_key)
              WHERE track.indexed=1 AND credit.genre_key=requested.value AND track.source_key=?2
                AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
              UNION
              SELECT track.track_key FROM album_genres credit
              JOIN albums album ON album.album_key=credit.album_key AND album.source_key=?2
              JOIN tracks track USING(album_key)
              WHERE track.indexed=1 AND credit.genre_key=requested.value
                AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))) member
             CROSS JOIN tracks track USING(track_key)".to_string(),
            "1".to_string(),
        )
    } else {
        (
            format!("track_{kind}s credit JOIN tracks track USING(track_key)"),
            format!("track.indexed=1 AND credit.{kind}_key=requested.value AND track.source_key=?2
              AND (?3 IS NULL OR EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))"),
        )
    };
    let selection = crate::artwork::collection_artwork_sql(&from, &predicate);
    let sql = format!(
        "WITH requested AS (SELECT DISTINCT value FROM json_each(?1))
         SELECT requested.value,COALESCE(album.artwork_binding,track.artwork_binding) FROM requested
         CROSS JOIN json_each((SELECT json_group_array(track_key) FROM ({selection}))) artwork
         JOIN tracks track ON track.track_key=artwork.value
         LEFT JOIN albums album USING(album_key)
         ORDER BY requested.value,artwork.key"
    );
    let mut artwork = BTreeMap::<i64, Vec<Vec<u8>>>::new();
    for (key, binding) in sqlx::query_as::<_, (i64, Vec<u8>)>(sqlx::AssertSqlSafe(sql))
        .bind(serde_json::to_string(keys)?)
        .bind(source)
        .bind(folder)
        .fetch_all(connection)
        .await?
    {
        artwork.entry(key).or_default().push(binding);
    }
    Ok(artwork)
}

async fn load_genre_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[GenreKey],
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<GenreRow>> {
    let mut rows = load_genre_facts(connection, source, keys, folder).await?;
    let keys = rows.iter().map(|row| row.genre_key).collect::<Vec<_>>();
    let artwork = named_collection_artwork(connection, source, None, &keys, "genre").await?;
    for row in &mut rows {
        row.representative_artwork = artwork
            .get(&row.genre_key.raw())
            .cloned()
            .unwrap_or_default();
    }
    Ok(rows)
}

async fn load_genre_facts(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[GenreKey],
    _folder: Option<FolderKey>,
) -> LibraryResult<Vec<GenreRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(genre_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(") SELECT genre.genre_key,genre.source_key,(SELECT object_id FROM sources WHERE source_key=genre.source_key) source_id,genre.object_id,genre.name,genre.artwork_binding,genre.album_count,genre.track_count,genre.duration_millis,genre.downloaded_count FROM requested JOIN genres genre USING(genre_key) WHERE genre.source_key=")
        .push_bind(source).push(" ORDER BY requested.position");
    let rows = query
        .build_query_as::<GenreRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    Ok(rows)
}

async fn load_mood_rows(
    connection: &mut SqliteConnection,
    source: SourceKey,
    keys: &[MoodKey],
    _folder: Option<FolderKey>,
) -> LibraryResult<Vec<MoodRow>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = QueryBuilder::<Sqlite>::new("WITH requested(mood_key, position) AS (");
    query.push_values(keys.iter().enumerate(), |mut row, (position, key)| {
        row.push_bind(*key).push_bind(position as i64);
    });
    query.push(") SELECT mood.mood_key,mood.source_key,mood.name,mood.track_count,mood.duration_millis,mood.downloaded_count FROM requested JOIN moods mood USING(mood_key) WHERE mood.source_key=")
        .push_bind(source).push(" ORDER BY requested.position");
    let mut rows = query
        .build_query_as::<MoodRow>()
        .persistent(false)
        .fetch_all(&mut *connection)
        .await?;
    let keys = rows.iter().map(|row| row.mood_key).collect::<Vec<_>>();
    let artwork = named_collection_artwork(connection, source, None, &keys, "mood").await?;
    for row in &mut rows {
        row.representative_artwork = artwork
            .get(&row.mood_key.raw())
            .cloned()
            .unwrap_or_default();
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
    Ok(sqlx::query_scalar(sql)
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
}

fn album_detail_track_count(folder: Option<FolderKey>) -> String {
    if folder.is_none() {
        "album.track_count".to_string()
    } else {
        format!(
            "(SELECT count(*) FROM tracks item WHERE {})",
            album_track_scope(folder)
        )
    }
}

fn album_detail_spans_sql(
    source: SourceKey,
    folder: Option<FolderKey>,
    favorites_only: bool,
    filter: &str,
    sort: AlbumSort,
    descending: bool,
    inline_tracks: usize,
    end: usize,
) -> String {
    let order = sort.order_sql(descending, folder);
    let scope = album_scope(source, folder, favorites_only, filter);
    let track_count = album_detail_track_count(folder);
    format!("WITH ordered AS (
        SELECT album.album_key,{track_count} track_count,
            row_number() OVER (ORDER BY {order}) album_rank
        FROM albums album WHERE {scope} ORDER BY {order} LIMIT {end}
      ), spans AS (
        SELECT album_key,track_count,
            COALESCE(sum(1+max(track_count-{inline_tracks},0)) OVER (ORDER BY album_rank ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),0) first_position
        FROM ordered
      )")
}

#[allow(clippy::too_many_arguments)]
async fn load_album_order<T>(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    favorites_only: bool,
    filter: &str,
    sort: AlbumSort,
    descending: bool,
    page: Option<(usize, usize)>,
    sections: bool,
) -> LibraryResult<Vec<T>>
where
    for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    let mut sql = format!(
        "SELECT album.album_key FROM albums album WHERE {} ORDER BY {}",
        album_scope(source, folder, favorites_only, filter),
        sort.order_sql(descending, folder)
    );
    if let Some((offset, limit)) = page {
        sql.push_str(&format!(
            " LIMIT {} OFFSET {}",
            limit.min(COLLECTION_ROW_LIMIT),
            offset.min(i64::MAX as usize)
        ));
    }
    if sections {
        sql = crate::scroll_sections::section_sql(
            &sql,
            if sort == AlbumSort::AlbumArtist {
                "album.album_artist_sort"
            } else {
                "album.sort_text"
            },
        );
    }
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

async fn load_folder_children(
    connection: &mut SqliteConnection,
    source: SourceKey,
    parent: Option<FolderKey>,
    page: Option<(usize, usize)>,
) -> LibraryResult<Vec<FolderKey>> {
    let suffix = page
        .map(|(offset, limit)| {
            format!(
                " LIMIT {} OFFSET {}",
                limit.min(COLLECTION_ROW_LIMIT),
                offset.min(i64::MAX as usize)
            )
        })
        .unwrap_or_default();
    let folder_keys = |sql: &str| {
        sqlx::query_scalar::<_, FolderKey>(sqlx::AssertSqlSafe(format!("{sql}{suffix}")))
    };
    let result = if let Some(parent) = parent {
        folder_keys(
            "SELECT DISTINCT child.folder_key
                 FROM track_folders parent_link
                 JOIN tracks track ON track.track_key=parent_link.track_key
                 JOIN track_folders child_link ON child_link.track_key=track.track_key
                    AND child_link.position=parent_link.position+1
                 JOIN folders child ON child.folder_key=child_link.folder_key
                 WHERE track.indexed=1 AND child.indexed=1 AND track.source_key=?1 AND parent_link.folder_key=?2
                 ORDER BY child.sort_text,child.folder_key",
        )
        .bind(source)
        .bind(parent)
        .fetch_all(&mut *connection)
        .await
    } else {
        folder_keys(
            "SELECT DISTINCT folder.folder_key
                 FROM track_folders relation JOIN tracks track USING(track_key)
                 JOIN folders folder USING(folder_key)
                 WHERE track.indexed=1 AND folder.indexed=1 AND track.source_key=?1 AND relation.position=0
                 ORDER BY folder.sort_text,folder.folder_key",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await
    };
    Ok(result?)
}

#[allow(clippy::too_many_arguments)]
async fn load_artist_order<T>(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    album_artists_only: bool,
    favorites_only: bool,
    filter: &str,
    sort: ArtistSort,
    descending: bool,
    page: Option<(usize, usize)>,
    sections: bool,
) -> LibraryResult<Vec<T>>
where
    for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    let descending = descending && filter.is_empty();
    let sort = if filter.is_empty() {
        sort
    } else {
        ArtistSort::Title
    };
    let mut query = QueryBuilder::<Sqlite>::new(format!(
        "SELECT artist.artist_key FROM artists artist WHERE {}",
        artist_scope(source, folder, album_artists_only, favorites_only, &filter)
    ));
    let role = if album_artists_only {
        "album_artist_"
    } else {
        ""
    };
    let (field, nulls_last) = match sort {
        ArtistSort::Title => ("artist.sort_text".into(), false),
        ArtistSort::AlbumCount => (format!("artist.{role}album_count"), false),
        ArtistSort::TrackCount => (format!("artist.{role}track_count"), false),
        ArtistSort::LastPlayed => (collection_stat("artist", "last_played", role), true),
        ArtistSort::PlayCount => (collection_stat("artist", "play_count", role), false),
        ArtistSort::Rating => (
            "COALESCE((SELECT state.rating FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_rating)".into(),
            true,
        ),
        ArtistSort::Favorite => (
            "COALESCE((SELECT state.favorite FROM user_media_state state WHERE state.media_uri=artist.media_uri),artist.source_favorite)".into(),
            false,
        ),
    };
    query
        .push(" ORDER BY ")
        .push(field)
        .push(if descending { " DESC" } else { " ASC" });
    if nulls_last {
        query.push(" NULLS LAST");
    }
    if sort != ArtistSort::Title {
        query.push(",artist.sort_text");
    }
    query.push(",artist.artist_key");
    if let Some((offset, limit)) = page {
        query
            .push(" LIMIT ")
            .push_bind(limit.min(COLLECTION_ROW_LIMIT) as i64)
            .push(" OFFSET ")
            .push_bind(offset.min(i64::MAX as usize) as i64);
    }
    let result = crate::scroll_sections::builder_rows(query, sections, connection).await?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn load_genre_order<T>(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    filter: &str,
    sort: GenreSort,
    descending: bool,
    page: Option<(usize, usize)>,
    sections: bool,
) -> LibraryResult<Vec<T>>
where
    for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    let field: &str = match sort {
        GenreSort::Title => "genre.sort_text",
        GenreSort::AlbumCount => "genre.album_count",
        GenreSort::TrackCount => "genre.track_count",
    };
    let tie = if sort == GenreSort::Title {
        ""
    } else {
        ",genre.sort_text"
    };
    let mut sql = format!(
        "SELECT genre.genre_key FROM genres genre WHERE {} ORDER BY {field} {}{tie},genre.genre_key",
        named_scope("genre", source, folder, filter),
        if descending && filter.trim().is_empty() {
            "DESC"
        } else {
            "ASC"
        }
    );
    if let Some((offset, limit)) = page {
        sql.push_str(&format!(
            " LIMIT {} OFFSET {}",
            limit.min(COLLECTION_ROW_LIMIT),
            offset.min(i64::MAX as usize)
        ));
    }
    if sections {
        sql = crate::scroll_sections::section_sql(&sql, "");
    }
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

const ARTIST_ALBUM_SCOPE: &str = "SELECT album.album_key FROM albums album
             WHERE album.indexed=1 AND album.source_key=?1 AND album.album_key IN (
               SELECT album_key FROM album_artists WHERE ?3=1 AND artist_key=?2
               UNION SELECT track.album_key FROM track_artists credit JOIN tracks track USING(track_key) WHERE track.indexed=1 AND ?3=0 AND credit.artist_key=?2)
             AND (?4 IS NULL OR EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.indexed=1 AND track.album_key=album.album_key AND scope.folder_key=?4))
             AND (?5 OR instr(lower(album.title),?6)>0 OR instr(lower(album.display_artist),?6)>0 OR CAST(album.year AS TEXT)=?6)";

async fn load_mood_order<T>(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    filter: &str,
    sort: MoodSort,
    descending: bool,
    offset: usize,
    limit: usize,
    sections: bool,
) -> LibraryResult<Vec<T>>
where
    for<'r> T: sqlx::Decode<'r, Sqlite> + sqlx::Type<Sqlite> + Send + Unpin,
{
    let field: &str = if !filter.trim().is_empty() {
        "mood.sort_text"
    } else {
        match sort {
            MoodSort::Title => "mood.sort_text",
            MoodSort::TrackCount => "mood.track_count",
            MoodSort::Duration => "mood.duration_millis",
        }
    };
    let tie = if field == "mood.sort_text" {
        ""
    } else {
        ",mood.sort_text"
    };
    let mut sql = format!(
        "SELECT mood.mood_key FROM moods mood WHERE {} ORDER BY {field} {}{tie},mood.mood_key",
        named_scope("mood", source, folder, filter),
        if descending && filter.trim().is_empty() {
            "DESC"
        } else {
            "ASC"
        }
    );
    if !sections {
        sql.push_str(&format!(
            " LIMIT {} OFFSET {}",
            limit.min(COLLECTION_ROW_LIMIT),
            offset.min(i64::MAX as usize)
        ));
    }
    if sections {
        sql = crate::scroll_sections::section_sql(&sql, "");
    }
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .fetch_all(connection)
        .await?)
}

const FILTERED_FOLDER_CHILDREN_SQL: &str = "SELECT DISTINCT folder.folder_key,folder.sort_text FROM folders folder JOIN track_folders child USING(folder_key) JOIN tracks track USING(track_key) WHERE track.indexed=1 AND folder.indexed=1 AND track.source_key=?1 AND instr(lower(folder.name),lower(?3))>0 AND ((?2 IS NULL AND child.position=0) OR EXISTS(SELECT 1 FROM track_folders parent WHERE parent.track_key=child.track_key AND parent.folder_key=?2 AND child.position=parent.position+1))";

pub(crate) async fn install_collection_facts_on(
    connection: &mut SqliteConnection,
) -> LibraryResult<()> {
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS collection_changes(kind TEXT NOT NULL,entity_key INTEGER NOT NULL,PRIMARY KEY(kind,entity_key)) STRICT, WITHOUT ROWID;")
        .execute(&mut *connection).await?;
    let changed_track = |row: &str| {
        format!("
        INSERT INTO collection_changes SELECT 'album',{row}.album_key WHERE {row}.album_key IS NOT NULL ON CONFLICT(kind,entity_key) DO NOTHING;
        INSERT INTO collection_changes SELECT 'artist',artist_key FROM track_artists WHERE track_key={row}.track_key UNION SELECT 'artist',artist_key FROM album_artists WHERE album_key={row}.album_key ON CONFLICT(kind,entity_key) DO NOTHING;
        INSERT INTO collection_changes SELECT 'genre',genre_key FROM track_genres WHERE track_key={row}.track_key UNION SELECT 'genre',genre_key FROM album_genres WHERE album_key={row}.album_key ON CONFLICT(kind,entity_key) DO NOTHING;
        INSERT INTO collection_changes SELECT 'mood',mood_key FROM track_moods WHERE track_key={row}.track_key ON CONFLICT(kind,entity_key) DO NOTHING;")
    };
    for (name, operation, body) in [
        ("track_insert", "AFTER INSERT", changed_track("NEW")),
        ("track_delete", "BEFORE DELETE", changed_track("OLD")),
        (
            "track_update",
            "AFTER UPDATE OF indexed,album_key,source_key,object_id,media_uri,source_play_count,source_last_played",
            format!("{}{}", changed_track("OLD"), changed_track("NEW")),
        ),
    ] {
        let changed = if name == "track_update" {
            " WHEN (OLD.indexed,OLD.album_key,OLD.source_key,OLD.object_id,OLD.media_uri,OLD.source_play_count,OLD.source_last_played) IS NOT (NEW.indexed,NEW.album_key,NEW.source_key,NEW.object_id,NEW.media_uri,NEW.source_play_count,NEW.source_last_played)"
        } else {
            ""
        };
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE TRIGGER IF NOT EXISTS facts_{name} {operation} ON tracks{changed} BEGIN {body} END;"
        )))
        .execute(&mut *connection)
        .await?;
    }
    for (table, kind, key) in [
        ("track_artists", "artist", "artist_key"),
        ("album_artists", "artist", "artist_key"),
        ("track_genres", "genre", "genre_key"),
        ("album_genres", "genre", "genre_key"),
        ("track_moods", "mood", "mood_key"),
    ] {
        for (event, row) in [("INSERT", "NEW"), ("DELETE", "OLD")] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER IF NOT EXISTS facts_{table}_{event} AFTER {event} ON {table} BEGIN INSERT INTO collection_changes VALUES('{kind}',{row}.{key}) ON CONFLICT(kind,entity_key) DO NOTHING; END;")))
                .execute(&mut *connection).await?;
        }
        let owner = if table.starts_with("track_") {
            "track_key"
        } else {
            "album_key"
        };
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER IF NOT EXISTS facts_{table}_update AFTER UPDATE OF {owner},{key} ON {table} WHEN (OLD.{owner},OLD.{key}) IS NOT (NEW.{owner},NEW.{key}) BEGIN INSERT INTO collection_changes VALUES('{kind}',OLD.{key}),('{kind}',NEW.{key}) ON CONFLICT(kind,entity_key) DO NOTHING; END;")))
            .execute(&mut *connection).await?;
    }
    let downloaded = |row: &str, delta: &str| {
        format!("
        UPDATE albums SET downloaded_count=downloaded_count{delta} WHERE NOT EXISTS(SELECT 1 FROM collection_changes WHERE kind='album' AND entity_key=albums.album_key) AND album_key IN (SELECT album_key FROM tracks WHERE media_uri={row}.media_uri AND indexed=1);
        UPDATE artists SET downloaded_count=downloaded_count{delta} WHERE NOT EXISTS(SELECT 1 FROM collection_changes WHERE kind='artist' AND entity_key=artists.artist_key) AND artist_key IN (SELECT artist_key FROM track_artists JOIN tracks USING(track_key) WHERE media_uri={row}.media_uri AND indexed=1);
        UPDATE artists SET album_artist_downloaded_count=album_artist_downloaded_count{delta} WHERE NOT EXISTS(SELECT 1 FROM collection_changes WHERE kind='artist' AND entity_key=artists.artist_key) AND artist_key IN (SELECT artist_key FROM album_artists JOIN tracks USING(album_key) WHERE media_uri={row}.media_uri AND indexed=1);
        UPDATE genres SET downloaded_count=downloaded_count{delta} WHERE NOT EXISTS(SELECT 1 FROM collection_changes WHERE kind='genre' AND entity_key=genres.genre_key) AND genre_key IN (SELECT genre_key FROM track_genres JOIN tracks USING(track_key) WHERE media_uri={row}.media_uri AND indexed=1 UNION SELECT genre_key FROM album_genres JOIN tracks USING(album_key) WHERE media_uri={row}.media_uri AND indexed=1);
        UPDATE moods SET downloaded_count=downloaded_count{delta} WHERE NOT EXISTS(SELECT 1 FROM collection_changes WHERE kind='mood' AND entity_key=moods.mood_key) AND mood_key IN (SELECT mood_key FROM track_moods JOIN tracks USING(track_key) WHERE media_uri={row}.media_uri AND indexed=1);")
    };
    let duration = downloaded(
        "NEW",
        "+(COALESCE(NEW.duration_millis,0)-COALESCE(OLD.duration_millis,0))",
    )
    .replace("downloaded_count", "duration_millis");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER IF NOT EXISTS facts_track_duration AFTER UPDATE OF duration_millis ON tracks
        WHEN NEW.duration_millis IS NOT OLD.duration_millis AND NEW.indexed=1
         AND (OLD.indexed,OLD.album_key,OLD.source_key,OLD.media_uri) IS (NEW.indexed,NEW.album_key,NEW.source_key,NEW.media_uri)
        BEGIN {duration} END;")))
        .execute(&mut *connection).await?;
    for (name, event, row, delta) in [
        ("insert", "AFTER INSERT", "NEW", "+1"),
        ("delete", "AFTER DELETE", "OLD", "-1"),
        (
            "update_old",
            "BEFORE UPDATE OF media_uri,origin",
            "OLD",
            "-1",
        ),
        (
            "update_new",
            "AFTER UPDATE OF media_uri,origin",
            "NEW",
            "+1",
        ),
    ] {
        let sql = format!(
            "CREATE TRIGGER IF NOT EXISTS facts_download_{name} {event} ON local_locators WHEN {row}.origin='download' BEGIN {} END;",
            downloaded(row, delta)
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *connection)
            .await?;
    }
    for (table, key, relation, prefix) in [
        (
            "albums",
            "album_key",
            "SELECT album_key FROM tracks WHERE media_uri=NEW.media_uri AND indexed=1",
            "",
        ),
        (
            "artists",
            "artist_key",
            "SELECT artist_key FROM track_artists JOIN tracks USING(track_key) WHERE media_uri=NEW.media_uri AND indexed=1",
            "",
        ),
        (
            "artists",
            "artist_key",
            "SELECT artist_key FROM album_artists JOIN tracks USING(album_key) WHERE media_uri=NEW.media_uri AND indexed=1",
            "album_artist_",
        ),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER IF NOT EXISTS facts_listen_{table}_{prefix} AFTER INSERT ON listens BEGIN
            UPDATE {table} SET {prefix}activity_play_count={prefix}activity_play_count+1,
              {prefix}activity_last_played=COALESCE(max({prefix}activity_last_played,NEW.started_at),NEW.started_at)
            WHERE {key} IN({relation}); END;")))
            .execute(&mut *connection).await?;
    }
    let changed_media = |predicate: &str| {
        format!("
        INSERT INTO collection_changes SELECT 'album',album_key FROM tracks track WHERE album_key IS NOT NULL AND ({predicate}) ON CONFLICT(kind,entity_key) DO NOTHING;
        INSERT INTO collection_changes SELECT 'artist',artist_key FROM track_artists JOIN tracks track USING(track_key) WHERE ({predicate}) UNION SELECT 'artist',artist_key FROM album_artists JOIN tracks track USING(album_key) WHERE ({predicate}) ON CONFLICT(kind,entity_key) DO NOTHING;")
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER IF NOT EXISTS facts_listen_delete AFTER DELETE ON listens BEGIN {} END;",
        changed_media("track.media_uri=OLD.media_uri")
    )))
    .execute(&mut *connection)
    .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER IF NOT EXISTS facts_listen_update AFTER UPDATE OF media_uri,started_at ON listens WHEN (OLD.media_uri,OLD.started_at) IS NOT (NEW.media_uri,NEW.started_at) BEGIN {} END;",changed_media("track.media_uri IN(OLD.media_uri,NEW.media_uri)"))))
        .execute(&mut *connection).await?;
    for event in ["INSERT", "DELETE", "UPDATE"] {
        let changed = if event == "INSERT" {
            vec!["NEW"]
        } else if event == "DELETE" {
            vec!["OLD"]
        } else {
            vec!["OLD", "NEW"]
        };
        let body = changed.into_iter().map(|row| changed_media(&format!("{row}.period='lifetime' AND {row}.item_kind='track' AND track.source_key=(SELECT source_key FROM sources WHERE object_id={row}.source_id) AND track.object_id={row}.track_object_id"))).collect::<Vec<_>>().join(" ");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TRIGGER IF NOT EXISTS facts_legacy_{event} AFTER {event} ON legacy_activity BEGIN {body} END;")))
            .execute(&mut *connection).await?;
    }
    Ok(())
}

pub(crate) async fn refresh_facts_on(connection: &mut SqliteConnection) -> LibraryResult<()> {
    for (kind, table, key, members, album_count, prefix) in [
        (
            "album",
            "albums",
            "album_key",
            "track.indexed=1 AND track.album_key=entity.album_key",
            false,
            "",
        ),
        (
            "artist",
            "artists",
            "artist_key",
            "track.indexed=1 AND track.track_key IN(SELECT track_key FROM track_artists WHERE artist_key=entity.artist_key)",
            true,
            "",
        ),
        (
            "artist",
            "artists",
            "artist_key",
            "track.indexed=1 AND track.album_key IN(SELECT album_key FROM album_artists WHERE artist_key=entity.artist_key)",
            true,
            "album_artist_",
        ),
        (
            "genre",
            "genres",
            "genre_key",
            "track.indexed=1 AND track.track_key IN(SELECT track_key FROM track_genres WHERE genre_key=entity.genre_key UNION SELECT track_key FROM album_genres JOIN tracks USING(album_key) WHERE genre_key=entity.genre_key)",
            true,
            "",
        ),
        (
            "mood",
            "moods",
            "mood_key",
            "track.indexed=1 AND track.track_key IN(SELECT track_key FROM track_moods WHERE mood_key=entity.mood_key)",
            false,
            "",
        ),
    ] {
        let columns = if album_count {
            format!("{prefix}album_count,")
        } else {
            String::new()
        };
        let albums = if album_count {
            "count(DISTINCT track.album_key),"
        } else {
            ""
        };
        let activity_plays = "track.local_play_count+COALESCE((SELECT play_count FROM legacy_activity old JOIN sources source ON source.object_id=old.source_id WHERE source.source_key=track.source_key AND old.track_object_id=track.object_id AND old.period='lifetime' AND old.item_kind='track'),0)";
        let activity_last = "(SELECT max(value) FROM (SELECT max(started_at) value FROM listens WHERE media_uri=track.media_uri UNION ALL SELECT last_played_at FROM legacy_activity old JOIN sources source ON source.object_id=old.source_id WHERE source.source_key=track.source_key AND old.track_object_id=track.object_id AND old.period='lifetime' AND old.item_kind='track'))";
        let (stat_columns, stats) = if matches!(kind, "album" | "artist") {
            (
                format!(
                    ",{prefix}derived_source_play_count,{prefix}derived_source_last_played,{prefix}activity_play_count,{prefix}activity_last_played"
                ),
                format!(
                    ",COALESCE(sum(track.source_play_count),0),max(track.source_last_played),COALESCE(sum({activity_plays}),0),max({activity_last})"
                ),
            )
        } else {
            (String::new(), String::new())
        };
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} AS entity SET ({columns}{prefix}track_count,{prefix}duration_millis,{prefix}downloaded_count{stat_columns})=(
            SELECT {albums}count(*),COALESCE(sum(track.duration_millis),0),count(CASE WHEN EXISTS(SELECT 1 FROM local_locators WHERE media_uri=track.media_uri AND origin='download') THEN 1 END){stats}
            FROM tracks track WHERE track.source_key=entity.source_key AND {members})
            WHERE {key} IN(SELECT entity_key FROM collection_changes WHERE kind='{kind}')")))
            .execute(&mut *connection).await?;
    }
    sqlx::query("DELETE FROM collection_changes")
        .execute(connection)
        .await?;
    Ok(())
}
