//! Owns Album, Artist, Genre, Mood, and Folder orders, rows, and details.
//! Collection membership, filtering, and sorting are resolved in one Store operation.

use sqlx::{Connection, FromRow, SqliteConnection};

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
        tracks: Vec<TrackView>,
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
        FROM candidates LEFT JOIN effective_album_release_types types USING(album_key)
        LEFT JOIN json_each('['||replace(json_quote(lower(types.release_type)),'|','\",\"')||']') item
        GROUP BY candidates.album_key
      ), classified AS (
        SELECT album.album_key,CASE WHEN coalesce(CASE WHEN album.metadata IS NOT NULL THEN album.is_compilation ELSE(SELECT json_extract(member.metadata,'$.album_tags.is_compilation') FROM tracks member INDEXED BY tracks_album_order_idx WHERE member.album_key=album.album_key AND member.indexed=1 AND json_extract(member.metadata,'$.album_tags.is_compilation') IS NOT NULL ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1) END,0)<>0 THEN 3
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

    pub async fn collection_source(
        &self,
        scope: &crate::TrackScope,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<SourceKey>> {
        use crate::TrackScope as C;
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
               SELECT album.album_key FROM albums album JOIN album_order ordering USING(album_key)
               WHERE (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1)) AND album.source_key=?1
                 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.indexed=1 AND track.album_key=album.album_key AND scope.folder_key=?2))
               ORDER BY ","ordering.added_sort"," DESC NULLS LAST,album.album_key LIMIT ?3
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
        let identity = album_release_identity("album");
        Ok(sqlx::query_as::<_,AlbumReleaseCandidate>(sqlx::AssertSqlSafe(format!("SELECT album.album_key,album.media_uri,{identity} lookup_identity FROM albums album WHERE (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1)) AND source_key=?1 AND {identity} IS NOT NULL AND release_lookup_identity IS NOT {identity} ORDER BY album_key LIMIT ?2"))).bind(source).bind(limit).fetch_all(&mut *connection).await?)
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
        let identity=sqlx::query_as::<_,(Option<String>,Option<String>)>(sqlx::AssertSqlSafe(format!("SELECT {},release_lookup_identity FROM albums album WHERE source_key=?1 AND album_key=?2",album_release_identity("album")))).bind(source).bind(album).fetch_optional(&mut *transaction).await?;
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
        transaction.commit().await?;
        Ok(Some(album))
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
        sort: CatalogSort,
        descending: bool,
        section: Option<AlbumReleaseClass>,
        offset: usize,
        limit: usize,
        activity: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(AlbumReleaseClass, AlbumView)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let scope = ARTIST_ALBUM_SCOPE;
        let order = sort.album_order_sql(descending, folder);
        let sql = format!(
            "WITH {} SELECT album.album_key,classified.class_code FROM classified JOIN albums album USING(album_key) JOIN album_order ordering USING(album_key) WHERE (?7 IS NULL OR classified.class_code=?7) ORDER BY classified.class_code,{order} LIMIT ?8 OFFSET ?9",
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
            &keys.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            activity,
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
        sort: CatalogSort,
        descending: bool,
        section: AlbumReleaseClass,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        if !matches!(sort, CatalogSort::Title | CatalogSort::AlbumArtist) {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
        let scope = ARTIST_ALBUM_SCOPE;
        let order = sort.album_order_sql(descending, folder);
        let ordered = format!(
            "WITH {} SELECT album.album_key FROM classified JOIN albums album USING(album_key) JOIN album_order ordering USING(album_key) WHERE classified.class_code=?7 ORDER BY {order}",
            album_release_cte(scope)
        );
        let sql = crate::scroll_sections::section_sql(
            &ordered,
            if sort == CatalogSort::Title {
                "ordering.title_sort"
            } else {
                "ordering.album_artist_sort"
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
        let keys = load_folder_children(&mut connection, source, parent, offset, limit).await?;
        load_folder_rows(&mut connection, &keys).await
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
        Ok((
            count.max(0) as usize,
            load_folder_rows(&mut connection, &keys).await?,
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

async fn load_folder_children(
    connection: &mut SqliteConnection,
    source: SourceKey,
    parent: Option<FolderKey>,
    offset: usize,
    limit: usize,
) -> LibraryResult<Vec<FolderKey>> {
    let suffix = format!(
        " LIMIT {} OFFSET {}",
        limit.min(COLLECTION_ROW_LIMIT),
        offset.min(i64::MAX as usize)
    );
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

const ARTIST_ALBUM_SCOPE: &str = "SELECT album.album_key FROM albums album JOIN album_order ordering USING(album_key)
             WHERE (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1)) AND album.source_key=?1 AND album.album_key IN (
               SELECT album_key FROM album_artists credit JOIN albums album USING(album_key) WHERE ?3=1 AND credit.artist_key=?2 AND album.metadata IS NOT NULL
               UNION SELECT member.album_key FROM track_album_artists credit JOIN tracks member USING(track_key) JOIN albums album USING(album_key) WHERE ?3=1 AND credit.artist_key=?2 AND member.indexed=1 AND album.metadata IS NULL
               UNION SELECT track.album_key FROM track_artists credit JOIN tracks track USING(track_key) WHERE track.indexed=1 AND ?3=0 AND credit.artist_key=?2)
             AND (?4 IS NULL OR EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.indexed=1 AND track.album_key=album.album_key AND scope.folder_key=?4))
             AND (?5 OR instr(ordering.title_search,?6)>0 OR instr(ordering.artist_search,?6)>0 OR CAST(ordering.year_sort AS TEXT)=?6)";

const FILTERED_FOLDER_CHILDREN_SQL: &str = "SELECT DISTINCT folder.folder_key,folder.sort_text FROM folders folder JOIN track_folders child USING(folder_key) JOIN tracks track USING(track_key) WHERE track.indexed=1 AND folder.indexed=1 AND track.source_key=?1 AND instr(lower(folder.name),lower(?3))>0 AND ((?2 IS NULL AND child.position=0) OR EXISTS(SELECT 1 FROM track_folders parent WHERE parent.track_key=child.track_key AND parent.folder_key=?2 AND child.position=parent.position+1))";

pub(crate) fn album_artist_member(song: &str, artist: &str) -> String {
    format!(
        "{song}.track_key IN (
            SELECT member.track_key
            FROM album_artists credit
            CROSS JOIN albums album ON album.album_key=credit.album_key
            CROSS JOIN tracks member INDEXED BY tracks_album_order_idx ON member.album_key=album.album_key
            WHERE credit.artist_key={artist} AND album.metadata IS NOT NULL
            UNION
            SELECT member.track_key
            FROM (
                SELECT DISTINCT credited.album_key
                FROM track_album_artists credit
                CROSS JOIN tracks credited ON credited.track_key=credit.track_key
                CROSS JOIN albums album ON album.album_key=credited.album_key
                WHERE credit.artist_key={artist} AND credited.indexed=1 AND album.metadata IS NULL
            ) selected
            CROSS JOIN tracks member INDEXED BY tracks_album_order_idx ON member.album_key=selected.album_key
            UNION
            SELECT credit.track_key FROM track_album_artists credit
            CROSS JOIN tracks member ON member.track_key=credit.track_key
            WHERE credit.artist_key={artist} AND member.album_key IS NULL
        )"
    )
}

pub(crate) fn genre_member(song: &str, genre: &str) -> String {
    format!(
        "{song}.track_key IN (
            SELECT track_key FROM track_genres WHERE genre_key={genre}
            UNION
            SELECT member.track_key FROM album_genres credit
            CROSS JOIN albums album ON album.album_key=credit.album_key
            CROSS JOIN tracks member INDEXED BY tracks_album_order_idx ON member.album_key=album.album_key
            WHERE credit.genre_key={genre} AND album.metadata IS NOT NULL
            UNION
            SELECT member.track_key FROM (
                SELECT DISTINCT credited.album_key FROM track_genres credit
                CROSS JOIN tracks credited ON credited.track_key=credit.track_key
                CROSS JOIN albums album ON album.album_key=credited.album_key
                WHERE credit.genre_key={genre} AND credited.indexed=1 AND album.metadata IS NULL
            ) selected
            CROSS JOIN tracks member INDEXED BY tracks_album_order_idx ON member.album_key=selected.album_key
        )"
    )
}

fn member_scope(kind: &str, key: &str, album_artist: bool, folder: Option<FolderKey>) -> String {
    let membership = match kind {
        "album" => format!("track.album_key={key}"),
        "artist" if album_artist => album_artist_member("track", key),
        "artist" => format!(
            "track.track_key IN (SELECT track_key FROM track_artists WHERE artist_key={key})"
        ),
        "genre" => genre_member("track", key),
        "mood" => {
            format!("track.track_key IN (SELECT track_key FROM track_moods WHERE mood_key={key})")
        }
        _ => unreachable!(),
    };
    // Look up members through their relationships before checking the boolean flag.
    let mut scope = format!("track.indexed AND {membership}");
    if let Some(folder) = folder {
        scope.push_str(&format!(" AND EXISTS(SELECT 1 FROM track_folders WHERE track_key=track.track_key AND folder_key={})",folder.raw()));
    }
    scope
}

fn summary_field(
    kind: &str,
    alias: &str,
    field: &str,
    role: bool,
    folder: Option<FolderKey>,
) -> String {
    let scope = member_scope(kind, &format!("{alias}.{kind}_key"), role, folder);
    if field == "album_count" && folder.is_none() {
        let native = if kind == "artist" && role {
            Some(format!(
                "SELECT album.album_key FROM album_artists credit JOIN albums album USING(album_key) WHERE credit.artist_key={alias}.artist_key AND album.indexed=1"
            ))
        } else if kind == "genre" {
            Some(format!(
                "SELECT album.album_key FROM album_genres credit JOIN albums album USING(album_key) WHERE credit.genre_key={alias}.genre_key AND album.indexed=1"
            ))
        } else {
            None
        };
        if let Some(native) = native {
            return format!(
                "(SELECT count(*) FROM(SELECT track.album_key FROM tracks track WHERE {scope} AND track.album_key IS NOT NULL UNION {native}))"
            );
        }
    }
    let expression = match field {
        "album_count" => "count(DISTINCT track.album_key)".into(),
        "track_count" => "count(*)".into(),
        "duration_millis" => "COALESCE(sum(track.duration_millis),0)".into(),
        "last_played" => format!("max({})", crate::tracks::displayed_stat("last_played")),
        "play_count" => format!(
            "COALESCE(sum({}),0)",
            crate::tracks::displayed_stat("play_count")
        ),
        _ => unreachable!(),
    };
    format!("(SELECT {expression} FROM tracks track WHERE {scope})")
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
            Self::Title => "ordering.title_sort".into(),
            Self::Artist | Self::AlbumArtist => "ordering.album_artist_sort".into(),
            Self::Year => "ordering.year_sort".into(),
            Self::ReleaseDate => "ordering.release_sort".into(),
            Self::DateAdded => "ordering.added_sort".into(),
            Self::LastPlayed => summary_field("album", "album", "last_played", false, folder),
            Self::PlayCount => summary_field("album", "album", "play_count", false, folder),
            Self::UserRating => favorite("album", "rating"),
            Self::Favorite => favorite("album", "favorite"),
            Self::TrackCount => summary_field("album", "album", "track_count", false, folder),
            Self::Duration => summary_field("album", "album", "duration_millis", false, folder),
            _ => unreachable!("album sort field"),
        };
        let direction = if descending { "DESC" } else { "ASC" };
        match self {
            Self::Title => format!("{field} {direction},album.album_key"),
            Self::Artist | Self::AlbumArtist => format!(
                "({field} IS NULL),{field} {direction},(ordering.year_sort IS NULL),ordering.year_sort,ordering.title_sort,album.album_key"
            ),
            _ => format!("{field} {direction} NULLS LAST,ordering.title_sort,album.album_key"),
        }
    }
}

pub(crate) fn album_track_scope(folder: Option<FolderKey>) -> String {
    member_scope("album", "album.album_key", false, folder).replace("track.", "item.")
}

pub(crate) fn album_scope(
    source: SourceKey,
    folder: Option<FolderKey>,
    favorites_only: bool,
    filter: &str,
) -> String {
    let mut scope = format!(
        "(album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1)) AND album.source_key={}",
        source.raw()
    );
    if folder.is_some() {
        scope.push_str(&format!(
            " AND EXISTS(SELECT 1 FROM tracks track WHERE {})",
            member_scope("album", "album.album_key", false, folder)
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
        scope.push_str(&format!(" AND (instr(ordering.title_search||' '||COALESCE(ordering.artist_search,''),{filter})>0 OR CAST(ordering.year_sort AS TEXT)={filter} OR EXISTS(SELECT 1 FROM effective_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND instr(artist.normalized_name,{filter})>0) OR EXISTS(SELECT 1 FROM effective_album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND instr(genre.normalized_name,{filter})>0))"));
    }
    scope
}

pub(crate) fn named_scope(
    kind: &str,
    source: SourceKey,
    folder: Option<FolderKey>,
    filter: &str,
) -> String {
    let mut scope = format!(
        "{kind}.source_key={} AND ({kind}.indexed=1 OR EXISTS(SELECT 1 FROM tracks track WHERE {}))",
        source.raw(),
        member_scope(kind, &format!("{kind}.{kind}_key"), false, None)
    );
    if folder.is_some() {
        scope.push_str(&format!(
            " AND EXISTS(SELECT 1 FROM tracks track WHERE {})",
            member_scope(kind, &format!("{kind}.{kind}_key"), false, folder)
        ));
    }
    let filter = filter.trim().to_lowercase();
    if !filter.is_empty() {
        scope.push_str(&format!(
            " AND instr({kind}.normalized_name,{})>0",
            crate::query::quote(&filter)
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
    let mut scope = format!("artist.source_key={}", source.raw());
    if !filter.trim().is_empty() {
        scope.push_str(&format!(
            " AND instr(artist.normalized_name,{})>0",
            crate::query::quote(&filter.trim().to_lowercase())
        ));
    }
    let members = member_scope("artist", "artist.artist_key", album_artist, folder);
    if album_artist && folder.is_none() {
        scope.push_str(&format!(" AND (EXISTS(SELECT 1 FROM tracks track WHERE {members}) OR EXISTS(SELECT 1 FROM album_artists credit JOIN albums album USING(album_key) WHERE credit.artist_key=artist.artist_key AND (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1))))"));
    } else {
        scope.push_str(&format!(
            " AND EXISTS(SELECT 1 FROM tracks track WHERE {members})"
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

pub(crate) fn collection_query(input: &crate::CatalogQuery) -> crate::query::OrderedQuery {
    use crate::CatalogSubject as S;
    let (kind, source, role, sort) = match input.subject {
        S::Albums { source, sort, .. } => {
            return crate::query::OrderedQuery {
                albumless: false,
                key: "album.album_key".into(),
                text: matches!(
                    sort,
                    CatalogSort::Title | CatalogSort::Artist | CatalogSort::AlbumArtist
                )
                .then(|| {
                    if sort == CatalogSort::Title {
                        "ordering.title_sort".into()
                    } else {
                        "ordering.album_artist_sort".into()
                    }
                }),
                from: "albums album JOIN album_order ordering USING(album_key)".into(),
                predicate: album_scope(source, input.folder, input.favorites_only, &input.filter),
                order: vec![sort.album_order_sql(input.descending, input.folder)],
                uri: "album.media_uri".into(),
                entry_key: "NULL".into(),
                arguments: Default::default(),
            };
        }
        S::Artists {
            source,
            sort,
            album_artist,
        } => ("artist", source, album_artist, sort),
        S::Genres { source, sort } => ("genre", source, false, sort),
        S::Moods { source, sort } => ("mood", source, false, sort),
        S::Playlists { source, sort } => {
            return crate::playlists::playlist_list_query(
                source,
                input.folder,
                sort,
                input.descending,
                &input.filter,
            );
        }
        _ => unreachable!(),
    };
    let field = match sort {
        CatalogSort::Title | CatalogSort::Artist => format!("{kind}.sort_text"),
        CatalogSort::AlbumCount => summary_field(kind, kind, "album_count", role, input.folder),
        CatalogSort::TrackCount => summary_field(kind, kind, "track_count", role, input.folder),
        CatalogSort::Duration => summary_field(kind, kind, "duration_millis", role, input.folder),
        CatalogSort::PlayCount => summary_field(kind, kind, "play_count", role, input.folder),
        CatalogSort::LastPlayed => summary_field(kind, kind, "last_played", role, input.folder),
        CatalogSort::UserRating => favorite(kind, "rating"),
        CatalogSort::Favorite => favorite(kind, "favorite"),
        _ => unreachable!("collection sort field"),
    };
    let direction = if input.descending { "DESC" } else { "ASC" };
    let predicate = if kind == "artist" {
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
    crate::query::OrderedQuery {
        albumless: false,
        key: format!("{kind}.{kind}_key"),
        text: matches!(sort, CatalogSort::Title | CatalogSort::Artist).then(|| field.clone()),
        from: format!("{kind}s {kind}"),
        predicate,
        order: vec![format!(
            "{field} {direction} NULLS LAST,{kind}.sort_text,{kind}.{kind}_key"
        )],
        uri: format!("{kind}.media_uri"),
        entry_key: "NULL".into(),
        arguments: Default::default(),
    }
}

pub(crate) fn local_album_metadata(alias: &str) -> String {
    let first = |field: &str| {
        format!(
            "(SELECT json_extract(member.metadata,'$.{field}') FROM tracks member INDEXED BY tracks_album_order_idx WHERE member.album_key={alias}.album_key AND member.indexed=1 AND json_extract(member.metadata,'$.{field}') IS NOT NULL AND json_extract(member.metadata,'$.{field}')<>'' ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1)"
        )
    };
    let artists = format!(
        "(SELECT json_group_array(json_object('object_id',artist.object_id,'name',artist.name,'sort_text',artist.sort_text,'musicbrainz_artist_id',artist.musicbrainz_artist_id) ORDER BY credit.position) FROM effective_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key={alias}.album_key)"
    );
    let genres = format!(
        "(SELECT json_group_array(json_object('object_id',genre.object_id,'name',genre.name,'sort_text',genre.sort_text) ORDER BY credit.position) FROM effective_album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key={alias}.album_key)"
    );
    let releases = format!(
        "(SELECT json_group_array(release_type ORDER BY position) FROM effective_album_release_types WHERE album_key={alias}.album_key)"
    );
    format!(
        "json_patch(COALESCE(json({alias}.metadata),json_object('title',COALESCE({},''),'artist',COALESCE((SELECT group_concat(name,', ') FROM (SELECT artist.name FROM effective_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key={alias}.album_key ORDER BY credit.position)),''),'sort_text',COALESCE({},''),'year',{},'release_date',{},'date_added',{},'musicbrainz_release_id',{},'musicbrainz_release_group_id',{},'is_compilation',json(CASE {} WHEN 1 THEN 'true' WHEN 0 THEN 'false' ELSE 'null' END),'album_artists',json({artists}),'genres',json({genres}),'release_types',json({releases}))),json_object('date_added',(SELECT date(added_sort,'unixepoch') FROM album_order WHERE album_key={alias}.album_key),'release_types',json({releases})))",
        first("album"),
        first("album_tags.sort_text"),
        first("year"),
        first("release_date"),
        first("date_added"),
        first("musicbrainz_album_id"),
        first("musicbrainz_release_group_id"),
        first("album_tags.is_compilation")
    )
}

async fn load_collection<T>(
    connection: &mut SqliteConnection,
    kind: &str,
    keys: &[impl serde::Serialize],
    role: bool,
    activity: bool,
    columns: &str,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let scope = member_scope(kind, "owner.value", role, None);
    let activity_columns = if activity {
        format!(
            ",sum({}) play_count,max({}) last_played",
            crate::tracks::displayed_stat("play_count"),
            crate::tracks::displayed_stat("last_played")
        )
    } else {
        ",0 play_count,NULL last_played".into()
    };
    let album_count = if kind == "genre" || kind == "artist" && role {
        summary_field(kind, kind, "album_count", role, None)
    } else {
        "COALESCE(summary.album_count,0)".into()
    };
    let index = if kind == "album" {
        "INDEXED BY tracks_album_order_idx"
    } else {
        ""
    };
    let sql = format!("WITH requested AS (SELECT value,key ordinal FROM json_each(?1)), summaries AS (
        SELECT owner.value,count(DISTINCT track.album_key) album_count,count(*) track_count,COALESCE(sum(track.duration_millis),0) duration_millis,
        sum(EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download')) downloaded_count {activity_columns}
        FROM (SELECT DISTINCT value FROM requested) owner CROSS JOIN tracks track {index} ON {scope} GROUP BY owner.value)
        SELECT {columns},{album_count} album_count,COALESCE(summary.track_count,0) track_count,COALESCE(summary.duration_millis,0) duration_millis,COALESCE(summary.downloaded_count,0) downloaded_count,
        COALESCE(state.favorite,native.favorite,0) favorite,COALESCE(state.rating,native.rating)/10 rating,COALESCE(native.play_count,summary.play_count,0) play_count,COALESCE(native.last_played,summary.last_played) last_played,COALESCE(native.skip_count,0) skip_count
        FROM requested JOIN {kind}s {kind} ON {kind}.{kind}_key=requested.value
        LEFT JOIN summaries summary ON summary.value=requested.value
        LEFT JOIN user_media_state state ON state.media_uri={kind}.media_uri
        LEFT JOIN native_media_state native ON native.media_uri={kind}.media_uri
        ORDER BY requested.ordinal");
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(serde_json::to_string(keys)?)
        .fetch_all(connection)
        .await?)
}

pub(crate) async fn load_album_rows(
    connection: &mut SqliteConnection,
    keys: &[AlbumKey],
    activity: bool,
) -> LibraryResult<Vec<AlbumView>> {
    let columns = format!("album.album_key,album.source_key,album.object_id,album.media_uri,{} metadata,album.release_lookup_identity,{} artwork_binding,
        (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position) FROM effective_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key) album_artists,
        (SELECT json_group_array(json_object('genre_key',genre.genre_key,'name',genre.name) ORDER BY credit.position) FROM effective_album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key) genres",local_album_metadata("album"),crate::artwork::entity_artwork_sql("album","album"));
    load_collection(connection, "album", keys, false, activity, &columns).await
}

pub(crate) async fn load_artist_rows(
    connection: &mut SqliteConnection,
    keys: &[ArtistKey],
    role: bool,
    activity: bool,
) -> LibraryResult<Vec<ArtistView>> {
    load_collection(connection,"artist",keys,role,activity,&format!("artist.artist_key,artist.source_key,artist.object_id,artist.media_uri,json(artist.metadata) metadata,{} artwork_binding",crate::artwork::entity_artwork_sql("artist","artist"))).await
}

pub(crate) async fn load_genre_rows(
    connection: &mut SqliteConnection,
    keys: &[GenreKey],
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<GenreView>> {
    let columns = format!(
        "genre.genre_key,genre.source_key,(SELECT object_id FROM sources WHERE source_key=genre.source_key) source_id,genre.object_id,json(genre.metadata) metadata,{} artwork_binding",
        crate::artwork::preview_sql("genre.native_artwork")
    );
    let mut rows: Vec<GenreView> =
        load_collection(connection, "genre", keys, false, false, &columns).await?;
    for row in &mut rows {
        if row.artwork_binding.is_none() {
            row.representative_artwork =
                representative_artwork(connection, "genre", row.genre_key.raw(), folder).await?;
        }
    }
    Ok(rows)
}

pub(crate) async fn load_mood_rows(
    connection: &mut SqliteConnection,
    keys: &[MoodKey],
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<MoodView>> {
    let mut rows: Vec<MoodView> = load_collection(
        connection,
        "mood",
        keys,
        false,
        false,
        "mood.mood_key,mood.source_key,json(mood.metadata) metadata",
    )
    .await?;
    for row in &mut rows {
        row.representative_artwork =
            representative_artwork(connection, "mood", row.mood_key.raw(), folder).await?;
    }
    Ok(rows)
}

async fn representative_artwork(
    connection: &mut SqliteConnection,
    kind: &str,
    key: i64,
    folder: Option<FolderKey>,
) -> LibraryResult<Vec<Vec<u8>>> {
    let mut scope = member_scope(kind, "owner.key", false, None);
    if folder.is_some() {
        let visible = member_scope(kind, "owner.key", false, folder).replace("track.", "present.");
        scope.push_str(&format!(
            " AND EXISTS(SELECT 1 FROM tracks present WHERE {visible})"
        ));
    }
    // An input row keeps SQLite from recompiling cover selection for each owner key.
    let selection = crate::artwork::collection_artwork_sql(
        "(SELECT ?1 key) owner CROSS JOIN tracks track",
        &scope,
    );
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {} FROM ({selection}) selected JOIN tracks track USING(track_key) LEFT JOIN albums album USING(album_key)",crate::artwork::track_artwork_sql("track")))).bind(key).fetch_all(connection).await?)
}

pub(crate) async fn load_folder_rows(
    connection: &mut SqliteConnection,
    keys: &[FolderKey],
) -> LibraryResult<Vec<FolderView>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT folder.folder_key,folder.source_key,folder.object_id,json_patch(folder.metadata,json_object('name',folder.name,'sort_text',folder.sort_text)) metadata,{} artwork_binding,(SELECT count(*) FROM track_folders JOIN tracks USING(track_key) WHERE folder_key=folder.folder_key AND tracks.indexed=1) track_count FROM json_each(?1) requested JOIN folders folder ON folder_key=requested.value ORDER BY CAST(requested.key AS INTEGER)",crate::artwork::preview_sql("folder.native_artwork")))).bind(serde_json::to_string(keys)?).fetch_all(connection).await?)
}

pub(crate) fn album_detail_track_count(folder: Option<FolderKey>) -> String {
    summary_field("album", "album", "track_count", false, folder)
}

pub(crate) async fn load_album_details(
    connection: &mut SqliteConnection,
    input: &crate::CatalogQuery,
    _source: SourceKey,
    sort: CatalogSort,
    inline: usize,
    offset: usize,
    limit: usize,
) -> LibraryResult<Vec<AlbumDetailRow>> {
    let end = offset.saturating_add(limit);
    let relation = collection_query(input);
    let count = album_detail_track_count(input.folder);
    let sql = format!(
        "WITH ordered AS (SELECT album.album_key,{count} track_count,row_number() OVER (ORDER BY {}) album_rank FROM {} WHERE {} ORDER BY {} LIMIT {end}),spans AS (SELECT album_key,track_count,COALESCE(sum(1+max(track_count-{inline},0)) OVER (ORDER BY album_rank ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING),0) first_position FROM ordered) SELECT album_key,track_count,first_position FROM spans WHERE first_position<?1 AND first_position+max(track_count-{inline},0)>=?2 ORDER BY first_position",
        sort.album_order_sql(input.descending, input.folder),
        relation.from,
        relation.predicate,
        sort.album_order_sql(input.descending, input.folder)
    );
    let spans = sqlx::query_as::<_, (AlbumKey, i64, i64)>(sqlx::AssertSqlSafe(sql))
        .bind(end as i64)
        .bind(offset as i64)
        .fetch_all(&mut *connection)
        .await?;
    let mut rows = Vec::new();
    for (album, count, first) in spans {
        let first = first as usize;
        let count = count as usize;
        let header = first >= offset;
        let first_track = first.saturating_add(1);
        let start = if header {
            0
        } else {
            inline.saturating_add(offset.saturating_sub(first_track))
        };
        let length = inline
            .saturating_add(end.saturating_sub(first_track))
            .min(count)
            .saturating_sub(start);
        let scope = album_track_scope(input.folder).replace("album.album_key", "?1");
        let keys = sqlx::query_scalar::<_,crate::TrackKey>(sqlx::AssertSqlSafe(format!("SELECT item.track_key FROM tracks item WHERE {scope} ORDER BY item.disc_number,item.track_number,item.sort_text,item.track_key LIMIT ?2 OFFSET ?3"))).bind(album).bind(length as i64).bind(start as i64).fetch_all(&mut *connection).await?;
        let mut songs = crate::tracks::load_track_rows(connection, &keys)
            .await?
            .into_iter();
        if header
            && let Some(album) = load_album_rows(connection, &[album], input.activity)
                .await?
                .pop()
        {
            rows.push(AlbumDetailRow::Album {
                album,
                tracks: songs.by_ref().take(inline).collect(),
                last: count <= inline,
            });
        }
        for (position, track) in songs.enumerate() {
            let index = position + start.max(inline);
            rows.push(AlbumDetailRow::Track {
                track,
                index,
                last: index + 1 == count,
            });
        }
    }
    Ok(rows)
}

pub(crate) fn album_release_identity(alias: &str) -> String {
    let field = |native: &str, song: &str| {
        format!(
            "CASE WHEN {alias}.metadata IS NOT NULL THEN {alias}.{native} ELSE(SELECT json_extract(member.metadata,'$.{song}') FROM tracks member INDEXED BY tracks_album_order_idx WHERE member.album_key={alias}.album_key AND member.indexed=1 AND json_extract(member.metadata,'$.{song}') IS NOT NULL ORDER BY coalesce(member.cue_path,member.source_path),member.disc_number,member.track_number,member.object_id LIMIT 1) END"
        )
    };
    let group = field(
        "musicbrainz_release_group_id",
        "musicbrainz_release_group_id",
    );
    let release = field("musicbrainz_release_id", "musicbrainz_album_id");
    format!(
        "CASE WHEN ({group}) IS NOT NULL THEN 'release-group:'||({group}) WHEN({release}) IS NOT NULL THEN 'release:'||({release}) END"
    )
}
