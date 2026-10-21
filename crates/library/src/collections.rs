//! Owns Album, Artist, Genre, Mood, and Folder orders, rows, and details.
//! Collection membership, filtering, and sorting are resolved in one Store operation.

use sqlx::{Arguments, Connection, FromRow, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, CatalogSort, Database, FolderKey, GenreKey, LibraryError, LibraryResult,
    MoodKey, ReadCancellation, SourceKey, TrackView,
};

pub(crate) const COLLECTION_ROW_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "rows are held only for a visible page"
)]
pub enum AlbumDetailRow {
    Album {
        album: AlbumView,
        last: bool,
    },
    Track {
        track: TrackView,
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

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AlbumReleaseClass {
    Album,
    Ep,
    Single,
    Collection,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollectionSummary {
    pub album_count: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumLinks {
    #[sqlx(json)]
    pub album_artists: Vec<AlbumArtistLink>,
    #[sqlx(json)]
    pub genres: Vec<AlbumGenreLink>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumView {
    pub album_key: AlbumKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    #[sqlx(json)]
    pub metadata: crate::AlbumMetadata,
    #[sqlx(flatten)]
    pub state: crate::TrackState,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    #[sqlx(flatten)]
    pub links: AlbumLinks,
    pub artwork_binding: Option<Vec<u8>>,
    pub release_lookup_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtistView {
    pub artist_key: ArtistKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    #[sqlx(json)]
    pub metadata: crate::ArtistMetadata,
    #[sqlx(flatten)]
    pub state: crate::TrackState,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GenreView {
    pub genre_key: GenreKey,
    pub source_key: SourceKey,
    pub source_id: String,
    pub object_id: String,
    #[sqlx(json)]
    pub metadata: crate::TagMetadata,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
    #[sqlx(skip)]
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MoodView {
    pub mood_key: MoodKey,
    pub source_key: SourceKey,
    #[sqlx(json)]
    pub metadata: crate::TagMetadata,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    #[sqlx(skip)]
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct FolderChoice {
    pub folder_key: FolderKey,
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderView {
    pub folder_key: FolderKey,
    pub source_key: SourceKey,
    pub object_id: String,
    #[sqlx(json)]
    pub metadata: crate::FolderMetadata,
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

impl Database {
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
            "SELECT item.value section_text,CAST(item.key AS INTEGER) section_position FROM json_each(?1) item",
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
        let sql = crate::scroll_sections::section_sql(&format!(
            "SELECT sort_text section_text,row_number() OVER(ORDER BY sort_text {direction},folder_key)-1 section_position FROM ({FILTERED_FOLDER_CHILDREN_SQL})"
        ));
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

    pub async fn genre_choices(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreChoice>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT genre.genre_key,genre.object_id,genre.name FROM {} WHERE {} ORDER BY genre.sort_text,genre.genre_key",
            collection_from("genre",false,folder), named_scope("genre",source,folder,"")
        ))).fetch_all(&mut *connection).await?)
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
               WHERE (album.indexed=1 OR album.track_count>0) AND album.source_key=?1
                 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM collection_album_index scope WHERE scope.kind='folder' AND scope.entity_key=?2 AND scope.role=0 AND scope.album_key=album.album_key AND coalesce(scope.folder_key,'')='' AND scope.track_count>0))
               ORDER BY ","album.added_sort"," DESC NULLS LAST,album.album_key LIMIT ?3
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
        let identity = "album.release_identity";
        Ok(sqlx::query_as::<_,AlbumReleaseCandidate>(sqlx::AssertSqlSafe(format!("SELECT album.album_key,album.media_uri,{identity} lookup_identity FROM albums album WHERE (album.indexed=1 OR album.track_count>0) AND source_key=?1 AND {identity} IS NOT NULL AND release_lookup_identity IS NOT {identity} ORDER BY album_key LIMIT ?2"))).bind(source).bind(limit).fetch_all(&mut *connection).await?)
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
        let identity=sqlx::query_as::<_,(Option<String>,Option<String>)>(sqlx::AssertSqlSafe(format!("SELECT {},release_lookup_identity FROM albums album WHERE source_key=?1 AND album_key=?2","album.release_identity"))).bind(source).bind(album).fetch_optional(&mut *transaction).await?;
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
            sqlx::query("INSERT INTO album_release_observations(album_key,lookup_identity,release_types) VALUES(?1,?2,jsonb(?3)) ON CONFLICT(album_key) DO UPDATE SET lookup_identity=excluded.lookup_identity,release_types=excluded.release_types").bind(album).bind(lookup_identity).bind(serde_json::to_string(&release_types)?).execute(&mut *transaction).await?;
        }
        sqlx::query(
            "UPDATE albums SET release_lookup_identity=?3 WHERE source_key=?1 AND album_key=?2",
        )
        .bind(source)
        .bind(album)
        .bind(lookup_identity)
        .execute(&mut *transaction)
        .await?;
        crate::catalog::index::albums(&mut transaction, &[album.raw()]).await?;
        transaction.commit().await?;
        Ok(Some(album))
    }

    pub async fn music_folders(&self, source: SourceKey) -> LibraryResult<Vec<FolderChoice>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT folder_key,object_id,name FROM folders WHERE source_key=?1 AND indexed=1 ORDER BY sort_text,folder_key LIMIT 256")
            .bind(source).fetch_all(&mut *connection).await?)
    }

    pub async fn folder_identity_key(
        &self,
        source: SourceKey,
        object_id: &str,
    ) -> LibraryResult<Option<FolderKey>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn folder_page(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderView>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        load_folder_requested(
            &mut connection,
            folder_request(source, parent, "", false, offset, limit)?,
        )
        .await
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
    ) -> LibraryResult<(usize, Vec<FolderView>)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let children = FILTERED_FOLDER_CHILDREN_SQL;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM ({children})"
        )))
        .bind(source)
        .bind(parent)
        .bind(filter.trim())
        .fetch_one(&mut *connection)
        .await?;
        let requested = folder_request(source, parent, filter, descending, offset, limit)?;
        Ok((
            count.max(0) as usize,
            load_folder_requested(&mut connection, requested).await?,
        ))
    }
}

pub(crate) async fn album_disc_sections(
    query: &crate::query::OrderedQuery,
    sort: CatalogSort,
    descending: bool,
    connection: &mut SqliteConnection,
) -> LibraryResult<Vec<(u32, i64)>> {
    if sort != CatalogSort::TrackNumber {
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

fn folder_request(
    source: SourceKey,
    parent: Option<FolderKey>,
    filter: &str,
    descending: bool,
    offset: usize,
    limit: usize,
) -> LibraryResult<crate::query::RequestedKeys> {
    let mut arguments = sqlx::sqlite::SqliteArguments::default();
    arguments.add(source).map_err(sqlx::Error::Encode)?;
    arguments.add(parent).map_err(sqlx::Error::Encode)?;
    arguments
        .add(filter.trim().to_owned())
        .map_err(sqlx::Error::Encode)?;
    let direction = if descending { "DESC" } else { "ASC" };
    Ok(crate::query::RequestedKeys {
        sql: format!(
            "SELECT folder_key value,row_number() OVER()-1 ordinal FROM (SELECT folder_key FROM ({FILTERED_FOLDER_CHILDREN_SQL}) ORDER BY sort_text {direction},folder_key LIMIT {} OFFSET {})",
            limit.min(COLLECTION_ROW_LIMIT),
            offset.min(i64::MAX as usize)
        ),
        arguments,
    })
}

const ARTIST_ALBUM_SCOPE: &str = "SELECT album.album_key FROM (
    SELECT album_key FROM collection_album_index WHERE kind='artist' AND entity_key=?2 AND role=?3 AND coalesce(folder_key,'')=coalesce(?4,'') AND (track_count>0 OR (?3=1 AND ?4 IS NULL AND album_count>0)) AND album_key IS NOT NULL
) candidate CROSS JOIN albums album USING(album_key)
WHERE (album.indexed=1 OR album.track_count>0) AND album.source_key=?1";

const FILTERED_FOLDER_CHILDREN_SQL: &str = "SELECT folder.folder_key,folder.sort_text FROM folder_children relation CROSS JOIN folders folder ON folder.folder_key=relation.child_folder_key WHERE relation.source_key=?1 AND coalesce(relation.parent_folder_key,'')=coalesce(?2,'') AND relation.admitted_track_count>0 AND folder.indexed=1 AND instr(lower(folder.name),lower(?3))>0";

pub(crate) fn album_artist_keys(artist: &str) -> String {
    format!("SELECT member.track_key FROM album_artists credit CROSS JOIN tracks member INDEXED BY tracks_album_order_idx ON member.album_key=credit.album_key WHERE credit.artist_key={artist}
        UNION SELECT credit.track_key FROM track_album_artists credit CROSS JOIN tracks member ON member.track_key=credit.track_key WHERE credit.artist_key={artist} AND member.album_key IS NULL")
}
pub(crate) fn genre_keys(genre: &str) -> String {
    format!("SELECT track_key FROM track_genres WHERE genre_key={genre}
        UNION SELECT member.track_key FROM album_genres credit CROSS JOIN tracks member INDEXED BY tracks_album_order_idx ON member.album_key=credit.album_key WHERE credit.genre_key={genre}")
}
pub(crate) fn album_artist_member(song: &str, artist: &str) -> String {
    format!("{song}.track_key IN ({})", album_artist_keys(artist))
}
fn album_summary_field(field: &str, folder: Option<FolderKey>) -> String {
    if let Some(folder) = folder {
        let selected = if field == "album_count" {
            "(track_count>0)"
        } else {
            field
        };
        let value = format!(
            "SELECT {selected} FROM collection_album_index WHERE kind='folder' AND entity_key={} AND role=0 AND album_key=album.album_key AND coalesce(folder_key,'')=''",
            folder.raw()
        );
        return if field == "last_played" {
            format!("({value})")
        } else {
            format!("COALESCE(({value}),0)")
        };
    }
    if field == "album_count" {
        "(album.track_count>0)".into()
    } else {
        format!("album.{field}")
    }
}

pub(crate) fn global_totals_join(kind: &str, role: bool) -> String {
    format!(
        "LEFT JOIN collection_totals totals ON totals.kind='{kind}' AND totals.entity_key={kind}.{kind}_key AND totals.role={} AND coalesce(totals.folder_key,'')=''",
        i32::from(role)
    )
}

fn collection_from(kind: &str, role: bool, folder: Option<FolderKey>) -> String {
    if let Some(folder) = folder {
        format!(
            "collection_totals totals CROSS JOIN {kind}s {kind} ON {kind}.{kind}_key=totals.entity_key AND totals.kind='{kind}' AND totals.role={} AND coalesce(totals.folder_key,'')={}",
            i32::from(role),
            folder.raw()
        )
    } else {
        format!("{kind}s {kind} {}", global_totals_join(kind, role))
    }
}

fn total_field(field: &str) -> String {
    if field == "last_played" {
        "totals.last_played".into()
    } else {
        format!("COALESCE(totals.{field},0)")
    }
}

fn favorite(alias: &str, field: &str) -> String {
    let fallback = if field == "favorite" { ",0" } else { "" };
    format!(
        "COALESCE((SELECT {field} FROM user_media_state WHERE media_uri={alias}.media_uri),(SELECT {field} FROM native_media_state WHERE media_uri={alias}.media_uri){fallback})"
    )
}

impl CatalogSort {
    pub(crate) fn album_order_sql(self, descending: bool, folder: Option<FolderKey>) -> String {
        let field = match self {
            Self::Title => "album.title_sort".into(),
            Self::Artist | Self::AlbumArtist => "album.album_artist_sort".into(),
            Self::Year => "album.year_sort".into(),
            Self::ReleaseDate => "album.release_sort".into(),
            Self::DateAdded => "album.added_sort".into(),
            Self::LastPlayed => album_summary_field("last_played", folder),
            Self::PlayCount => album_summary_field("play_count", folder),
            Self::UserRating => favorite("album", "rating"),
            Self::Favorite => favorite("album", "favorite"),
            Self::TrackCount => album_summary_field("track_count", folder),
            Self::Duration => album_summary_field("duration_millis", folder),
            _ => unreachable!("album sort field"),
        };
        let direction = if descending { "DESC" } else { "ASC" };
        match self {
            Self::Title => format!("{field} {direction},album.album_key"),
            Self::Artist | Self::AlbumArtist => format!(
                "({field} IS NULL),{field} {direction},(album.year_sort IS NULL),album.year_sort,album.title_sort,album.album_key"
            ),
            _ => format!("{field} {direction} NULLS LAST,album.title_sort,album.album_key"),
        }
    }
}

pub(crate) fn album_scope(
    source: SourceKey,
    folder: Option<FolderKey>,
    favorites_only: bool,
    filter: &str,
) -> String {
    let mut scope = format!(
        "(album.indexed=1 OR album.track_count>0) AND album.source_key={}",
        source.raw()
    );
    if folder.is_some() {
        scope.push_str(&format!(
            " AND {}>0",
            album_summary_field("track_count", folder)
        ));
    }
    if favorites_only {
        scope.push_str(&format!(
            " AND album.media_uri IN({})",
            crate::favorites::FAVORITE_URIS
        ));
    }
    let filter = filter.trim().to_lowercase();
    if !filter.is_empty() {
        let filter = crate::query::quote(&filter);
        scope.push_str(&format!(" AND (instr(album.title_search||' '||COALESCE(album.artist_search,''),{filter})>0 OR CAST(album.year_sort AS TEXT)={filter} OR EXISTS(SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,{filter})>0) OR EXISTS(SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,{filter})>0))"));
    }
    scope
}
pub(crate) fn named_scope(
    kind: &str,
    source: SourceKey,
    folder: Option<FolderKey>,
    filter: &str,
) -> String {
    let visible = if folder.is_some() {
        "totals.track_count>0".into()
    } else {
        format!("({kind}.indexed=1 OR totals.track_count>0)")
    };
    let mut scope = format!("{kind}.source_key={} AND {visible}", source.raw());
    if !filter.trim().is_empty() {
        scope.push_str(&format!(
            " AND instr({kind}.normalized_name,{})>0",
            crate::query::quote(&filter.trim().to_lowercase())
        ));
    }
    scope
}
pub(crate) fn artist_scope(
    source: SourceKey,
    folder: Option<FolderKey>,
    album_artist: bool,
    favorites_only: bool,
    filter: &str,
) -> String {
    let empty = if album_artist && folder.is_none() {
        " OR totals.album_count>0"
    } else {
        ""
    };
    let mut scope = format!(
        "artist.source_key={} AND (totals.track_count>0{empty})",
        source.raw()
    );
    if !filter.trim().is_empty() {
        scope.push_str(&format!(
            " AND instr(artist.normalized_name,{})>0",
            crate::query::quote(&filter.trim().to_lowercase())
        ));
    }
    if favorites_only {
        scope.push_str(&format!(
            " AND artist.media_uri IN({})",
            crate::favorites::FAVORITE_URIS
        ));
    }
    scope
}

fn release_text_filter(filter: &str) -> String {
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if filter.is_empty() {
        return "1".into();
    }
    let filter = crate::query::quote(&filter);
    format!(
        "instr(album.title_search,{filter})>0 OR instr(album.artist_search,{filter})>0 OR CAST(album.year_sort AS TEXT)={filter}"
    )
}

pub(crate) fn collection_query(input: &crate::ListRead) -> crate::query::OrderedQuery {
    use crate::ListKind as K;
    let source = input.source.expect("source collection list");
    let sort = input.sort;
    if let K::ArtistAlbums {
        artist,
        album_artist,
        release_class,
        ref filters,
    } = input.kind
    {
        let mut arguments = sqlx::sqlite::SqliteArguments::default();
        arguments.add(source).expect("source argument");
        arguments.add(artist).expect("artist argument");
        arguments.add(album_artist).expect("role argument");
        arguments.add(input.folder).expect("folder argument");
        let text = if let Some(filters) = filters {
            filters
                .iter()
                .enumerate()
                .map(|(class, filter)| {
                    format!(
                        "(album.release_class={class} AND ({}))",
                        release_text_filter(filter)
                    )
                })
                .collect::<Vec<_>>()
                .join(" OR ")
        } else {
            release_text_filter(&input.filter)
        };
        let mut predicate = format!("({text})");
        if let Some(class) = release_class {
            predicate.push_str(&format!(
                " AND album.release_class={}",
                release_class_code(class)
            ));
        }
        if input.favorites_only {
            predicate.push_str(&format!(
                " AND album.media_uri IN({})",
                crate::favorites::FAVORITE_URIS
            ));
        }
        if input.downloaded_only {
            predicate.push_str(&format!(
                " AND {}>0",
                album_summary_field("downloaded_count", input.folder)
            ));
        }
        return crate::query::OrderedQuery {
            key: "album.album_key".into(),
            text: match sort {
                CatalogSort::Title => Some("album.title_sort".into()),
                CatalogSort::Artist | CatalogSort::AlbumArtist => {
                    Some("album.album_artist_sort".into())
                }
                _ => None,
            },
            from: format!(
                "({ARTIST_ALBUM_SCOPE}) candidate CROSS JOIN albums album USING(album_key)"
            ),
            predicate,
            order: vec![format!(
                "album.release_class,{}",
                sort.album_order_sql(input.descending, input.folder)
            )],
            entry_key: "NULL".into(),
            arguments,
        };
    }
    if input.kind == K::Albums {
        let mut predicate = album_scope(source, None, input.favorites_only, &input.filter);
        if input.downloaded_only {
            predicate.push_str(&format!(
                " AND {}>0",
                album_summary_field("downloaded_count", input.folder)
            ));
        }
        return crate::query::OrderedQuery {
            key: "album.album_key".into(),
            text: match sort {
                CatalogSort::Title => Some("album.title_sort".into()),
                CatalogSort::Artist | CatalogSort::AlbumArtist => {
                    Some("album.album_artist_sort".into())
                }
                _ => None,
            },
            from: if let Some(folder) = input.folder {
                format!(
                    "(SELECT album_key FROM collection_album_index WHERE kind='folder' AND entity_key={} AND role=0 AND coalesce(folder_key,'')='' AND album_key IS NOT NULL AND track_count>0) candidate CROSS JOIN albums album USING(album_key)",
                    folder.raw()
                )
            } else {
                "albums album".into()
            },
            predicate,
            order: vec![sort.album_order_sql(input.descending, input.folder)],
            entry_key: "NULL".into(),
            arguments: Default::default(),
        };
    }
    let (kind, role) = match input.kind {
        K::Artists { album_artist } => ("artist", album_artist),
        K::Genres => ("genre", false),
        K::Moods => ("mood", false),
        _ => unreachable!(),
    };
    let field = match sort {
        CatalogSort::Title | CatalogSort::Artist => format!("{kind}.sort_text"),
        CatalogSort::AlbumCount => total_field("album_count"),
        CatalogSort::TrackCount => total_field("track_count"),
        CatalogSort::Duration => total_field("duration_millis"),
        CatalogSort::PlayCount => total_field("play_count"),
        CatalogSort::LastPlayed => total_field("last_played"),
        CatalogSort::UserRating => favorite(kind, "rating"),
        CatalogSort::Favorite => favorite(kind, "favorite"),
        _ => unreachable!("collection sort field"),
    };
    let mut predicate = if kind == "artist" {
        artist_scope(
            source,
            input.folder,
            role,
            input.favorites_only,
            &input.filter,
        )
    } else {
        named_scope(kind, source, input.folder, &input.filter)
    };
    if input.downloaded_only {
        predicate.push_str(&format!(" AND {}>0", total_field("downloaded_count")));
    }
    let direction = if input.descending { "DESC" } else { "ASC" };
    crate::query::OrderedQuery {
        key: format!("{kind}.{kind}_key"),
        text: matches!(sort, CatalogSort::Title | CatalogSort::Artist).then(|| field.clone()),
        from: collection_from(kind, role, input.folder),
        predicate,
        order: vec![
            if matches!(sort, CatalogSort::Title | CatalogSort::Artist) {
                format!("{field} {direction},{kind}.{kind}_key")
            } else {
                format!("{field} {direction} NULLS LAST,{kind}.sort_text,{kind}.{kind}_key")
            },
        ],
        entry_key: "NULL".into(),
        arguments: Default::default(),
    }
}

async fn load_collection<T>(
    connection: &mut SqliteConnection,
    kind: &str,
    requested: crate::query::RequestedKeys,
    role: bool,
    activity: bool,
    columns: &str,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let join = if kind == "album" {
        String::new()
    } else {
        global_totals_join(kind, role)
    };
    let metric = |field| {
        if kind == "album" {
            album_summary_field(field, None)
        } else {
            total_field(field)
        }
    };
    let play = if activity {
        metric("play_count")
    } else {
        "0".into()
    };
    let last = if activity {
        metric("last_played")
    } else {
        "NULL".into()
    };
    let sql = format!("WITH requested AS ({}) SELECT {columns},{} album_count,{} track_count,{} duration_millis,{} downloaded_count,
        COALESCE(state.favorite,native.favorite,0) favorite,COALESCE(state.rating,native.rating)/10 rating,COALESCE(native.play_count,{play},0) play_count,COALESCE(native.last_played,{last}) last_played,COALESCE(native.skip_count,0) skip_count
        FROM requested CROSS JOIN {kind}s {kind} ON {kind}.{kind}_key=requested.value {join}
        LEFT JOIN user_media_state state ON state.media_uri={kind}.media_uri
        LEFT JOIN native_media_state native ON native.media_uri={kind}.media_uri ORDER BY requested.ordinal",requested.sql,metric("album_count"),metric("track_count"),metric("duration_millis"),metric("downloaded_count"));
    Ok(
        sqlx::query_as_with(sqlx::AssertSqlSafe(sql), requested.arguments)
            .fetch_all(connection)
            .await?,
    )
}

pub(crate) async fn load_album_rows(
    connection: &mut SqliteConnection,
    keys: &[AlbumKey],
    activity: bool,
) -> LibraryResult<Vec<AlbumView>> {
    load_album_requested(
        connection,
        crate::query::RequestedKeys::batch(keys)?,
        activity,
    )
    .await
}
pub(crate) async fn load_album_requested(
    connection: &mut SqliteConnection,
    requested: crate::query::RequestedKeys,
    activity: bool,
) -> LibraryResult<Vec<AlbumView>> {
    let columns = format!("album.album_key,album.source_key,album.object_id,album.media_uri,json(album.metadata) metadata,album.release_lookup_identity,{} artwork_binding,
        (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position) FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key) album_artists,
        (SELECT json_group_array(json_object('genre_key',genre.genre_key,'name',genre.name) ORDER BY credit.position) FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key) genres",crate::artwork::entity_artwork_sql("album","album"));
    load_collection(connection, "album", requested, false, activity, &columns).await
}
pub(crate) async fn load_artist_requested(
    connection: &mut SqliteConnection,
    requested: crate::query::RequestedKeys,
    role: bool,
    activity: bool,
) -> LibraryResult<Vec<ArtistView>> {
    load_collection(connection,"artist",requested,role,activity,&format!("artist.artist_key,artist.source_key,artist.object_id,artist.media_uri,json(artist.metadata) metadata,{} artwork_binding",crate::artwork::entity_artwork_sql("artist","artist"))).await
}
#[derive(FromRow)]
struct WithGallery<T> {
    #[sqlx(flatten)]
    row: T,
    #[sqlx(json)]
    gallery: Vec<String>,
}

pub(crate) async fn load_genre_requested(
    connection: &mut SqliteConnection,
    requested: crate::query::RequestedKeys,
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<GenreView>> {
    let columns = format!(
        "genre.genre_key,genre.source_key,(SELECT object_id FROM sources WHERE source_key=genre.source_key) source_id,genre.object_id,json(genre.metadata) metadata,{} artwork_binding,CASE WHEN genre.native_artwork IS NULL THEN {} ELSE '[]' END gallery",
        crate::artwork::preview_sql("genre.native_artwork"),
        gallery_sql("genre", folder)
    );
    let rows: Vec<WithGallery<GenreView>> =
        load_collection(connection, "genre", requested, false, false, &columns).await?;
    Ok(rows
        .into_iter()
        .map(|mut row| {
            row.row.representative_artwork =
                row.gallery.into_iter().map(String::into_bytes).collect();
            row.row
        })
        .collect())
}
pub(crate) async fn load_mood_requested(
    connection: &mut SqliteConnection,
    requested: crate::query::RequestedKeys,
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<MoodView>> {
    let columns = format!(
        "mood.mood_key,mood.source_key,json(mood.metadata) metadata,{} gallery",
        gallery_sql("mood", folder)
    );
    let rows: Vec<WithGallery<MoodView>> =
        load_collection(connection, "mood", requested, false, false, &columns).await?;
    Ok(rows
        .into_iter()
        .map(|mut row| {
            row.row.representative_artwork =
                row.gallery.into_iter().map(String::into_bytes).collect();
            row.row
        })
        .collect())
}

fn gallery_sql(kind: &str, folder: Option<FolderKey>) -> String {
    let binding = crate::artwork::track_artwork_sql("gallery_track");
    let gallery=format!("(SELECT json_group_array(CAST(binding AS TEXT)) FROM (
        SELECT {binding} binding FROM (
            SELECT artwork_track_key,artwork_group_uri,artwork_member_uri FROM collection_album_index
            WHERE kind='{kind}' AND entity_key={kind}.{kind}_key AND role=0 AND folder_key IS NULL AND artwork_track_key IS NOT NULL
            ORDER BY artwork_group_uri,artwork_member_uri LIMIT 4
        ) chosen CROSS JOIN tracks gallery_track ON gallery_track.track_key=chosen.artwork_track_key
        ORDER BY chosen.artwork_group_uri,chosen.artwork_member_uri
    ))");
    if let Some(folder) = folder {
        format!(
            "CASE WHEN EXISTS(SELECT 1 FROM collection_totals scope WHERE scope.kind='{kind}' AND scope.entity_key={kind}.{kind}_key AND scope.role=0 AND coalesce(scope.folder_key,'')={} AND scope.track_count>0) THEN {gallery} ELSE '[]' END",
            folder.raw()
        )
    } else {
        gallery
    }
}

pub(crate) async fn load_folder_requested(
    connection: &mut SqliteConnection,
    requested: crate::query::RequestedKeys,
) -> LibraryResult<Vec<FolderView>> {
    Ok(sqlx::query_as_with(sqlx::AssertSqlSafe(format!("WITH requested AS ({}) SELECT folder.folder_key,folder.source_key,folder.object_id,json_patch(folder.metadata,json_object('name',folder.name,'sort_text',folder.sort_text)) metadata,{} artwork_binding,COALESCE(totals.track_count,0) track_count FROM requested CROSS JOIN folders folder ON folder.folder_key=requested.value {} ORDER BY requested.ordinal",requested.sql,crate::artwork::preview_sql("folder.native_artwork"),global_totals_join("folder", false))),requested.arguments).fetch_all(connection).await?)
}

async fn load_album_detail_spans(
    connection: &mut SqliteConnection,
    input: &crate::ListRead,
    offset: usize,
    limit: usize,
    spans: Vec<(AlbumKey, i64, i64)>,
) -> LibraryResult<Vec<AlbumDetailRow>> {
    let end = offset.saturating_add(limit);
    let headers = spans
        .iter()
        .filter(|(_, _, first)| *first as usize >= offset)
        .map(|(key, _, _)| *key)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    if !headers.is_empty() {
        for album in load_album_rows(connection, &headers, input.activity).await? {
            let (_, count, first) = spans
                .iter()
                .find(|(key, _, _)| *key == album.album_key)
                .expect("requested album span");
            rows.push((
                *first as usize,
                AlbumDetailRow::Album {
                    album,
                    last: *count == 0,
                },
            ));
        }
    }
    let mut selected = Vec::new();
    for (album, count, first) in &spans {
        let first_track = *first as usize + 1;
        let start = offset.saturating_sub(first_track);
        let length = end
            .saturating_sub(first_track)
            .min(*count as usize)
            .saturating_sub(start);
        if length == 0 {
            continue;
        }
        let mut scope = format!("item.indexed=1 AND item.album_key={}", album.raw());
        if let Some(folder) = input.folder {
            scope.push_str(&format!(
                " AND item.track_key IN(SELECT track_key FROM track_folders WHERE folder_key={})",
                folder.raw()
            ));
        }
        selected.push(format!("SELECT value,row_number() OVER()-1+{} ordinal FROM (SELECT item.track_key value FROM tracks item INDEXED BY tracks_album_order_idx WHERE {scope} ORDER BY item.disc_number,item.track_number,item.sort_text,item.track_key LIMIT {length} OFFSET {start})",first_track+start));
    }
    if !selected.is_empty() {
        #[derive(FromRow)]
        struct TrackRow {
            #[sqlx(rename = "__ordinal", try_from = "i64")]
            position: usize,
            #[sqlx(flatten)]
            track: TrackView,
        }
        let requested = crate::query::RequestedKeys {
            sql: selected.join(" UNION ALL "),
            arguments: Default::default(),
        };
        let songs = sqlx::query_as_with::<_, TrackRow, _>(
            sqlx::AssertSqlSafe(crate::tracks::track_requested_sql(&requested)),
            requested.arguments,
        )
        .fetch_all(&mut *connection)
        .await?;
        for row in songs {
            let span = spans
                .partition_point(|(_, _, first)| *first as usize <= row.position)
                .saturating_sub(1);
            let (_, count, first) = spans[span];
            let index = row.position - first as usize - 1;
            rows.push((
                row.position,
                AlbumDetailRow::Track {
                    track: row.track,
                    index,
                    last: index + 1 == count as usize,
                },
            ));
        }
    }
    rows.sort_unstable_by_key(|(position, _)| *position);
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

struct AlbumSpan {
    key: AlbumKey,
    count: usize,
    first: usize,
    genres: bool,
}
// Compact keys and counts describe layout; metadata is read only for the window.
pub struct AlbumDetailIndex {
    read: crate::ListRead,
    count: usize,
    albums: Vec<AlbumSpan>,
}

impl crate::ListRead {
    pub async fn album_detail_index(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<std::sync::Arc<AlbumDetailIndex>> {
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let relation = collection_query(self);
        let count = album_summary_field("track_count", self.folder);
        let facts=sqlx::query_as_with::<_,(AlbumKey,i64,bool),_>(sqlx::AssertSqlSafe(relation.select(&format!("album.album_key,{count},EXISTS(SELECT 1 FROM album_genres WHERE album_key=album.album_key)"))),relation.arguments).fetch_all(&mut *connection).await?;
        let mut albums = Vec::with_capacity(facts.len());
        let mut first = 0;
        for (key, count, genres) in facts {
            let count = count.max(0) as usize;
            albums.push(AlbumSpan {
                key,
                count,
                first,
                genres,
            });
            first += 1 + count;
        }
        Ok(std::sync::Arc::new(AlbumDetailIndex {
            read: self.clone(),
            count: first,
            albums,
        }))
    }
}

impl AlbumDetailIndex {
    pub fn spans(&self) -> impl Iterator<Item = (AlbumKey, usize, bool, usize)> + '_ {
        self.albums
            .iter()
            .map(|album| (album.key, album.count, album.genres, album.first))
    }
    pub fn matches(&self, read: &crate::ListRead) -> bool {
        &self.read == read
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub async fn page(
        &self,
        database: &Database,
        window: crate::RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<crate::Page<AlbumDetailRow>> {
        let range = window.range(self.count);
        Ok(crate::Page {
            count: self.count,
            first_row_position: range.start,
            rows: self
                .rows(database, range.start, range.len(), cancellation)
                .await?,
        })
    }
    pub async fn rows(
        &self,
        database: &Database,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumDetailRow>> {
        if limit == 0 || offset >= self.count {
            return Ok(Vec::new());
        }
        let start = self
            .albums
            .partition_point(|album| album.first <= offset)
            .saturating_sub(1);
        let end = offset.saturating_add(limit.min(256));
        let spans = self.albums[start..]
            .iter()
            .take_while(|album| album.first < end)
            .map(|album| (album.key, album.count as i64, album.first as i64))
            .collect();
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows =
            load_album_detail_spans(&mut transaction, &self.read, offset, limit.min(256), spans)
                .await?;
        transaction.commit().await?;
        Ok(rows)
    }
}
