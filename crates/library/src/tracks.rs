//! Owns complete Track orders, bounded final rows, and details.
//! Each bounded row query includes its ordered artist and genre links.

use sqlx::{FromRow, Sqlite, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, Database, FolderKey, GenreKey, LibraryError, LibraryResult,
    ReadCancellation, SourceKey, TrackKey,
};

// Provider dates take precedence over the preserved first-seen timestamp.
macro_rules! added_at {
    ($row:literal) => {
        concat!(
            "COALESCE(unixepoch(",
            $row,
            ".date_added),",
            $row,
            ".first_seen_at)"
        )
    };
}
pub(crate) use added_at;

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

    pub(crate) fn order_terms(self, descending: bool, playlist: bool) -> Vec<String> {
        self.order_by(descending, |field| match field {
            "rating" => "COALESCE((SELECT rating FROM user_media_state WHERE media_uri=track.media_uri),(SELECT rating FROM native_media_state WHERE media_uri=track.media_uri))".into(),
            "favorite" => "COALESCE((SELECT favorite FROM user_media_state WHERE media_uri=track.media_uri),(SELECT favorite FROM native_media_state WHERE media_uri=track.media_uri),0)".into(),
            "genre" => "(SELECT genre.sort_text FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key ORDER BY relation.position LIMIT 1)".into(),
            "play_count" => displayed_stat("play_count"),
            "last_played" => displayed_stat("last_played"),
            "key" if playlist && self == Self::Position => "entry.position".into(),
            "key" => "track.track_key".into(),
            "artist_sort" => "track.display_artist".into(),
            "album_artist_sort" => "(SELECT album_artist_sort FROM album_order WHERE album_key=track.album_key)".into(),
            "album_sort" => "(SELECT title_sort FROM album_order WHERE album_key=track.album_key)".into(),
            "album_year" => "(SELECT year_sort FROM album_order WHERE album_key=track.album_key)".into(),
            "date_added" => added_at!("track").into(),
            _ => format!("track.{field}")
        })
    }

    pub(crate) fn order_by(self, descending: bool, column: impl Fn(&str) -> String) -> Vec<String> {
        if self == Self::Position {
            return vec![format!(
                "{} {}",
                column("key"),
                if descending { "DESC" } else { "ASC" }
            )];
        }
        let field = match self {
            Self::Position => unreachable!(),
            Self::Title => "sort_text",
            Self::TrackNumber => "disc_number",
            Self::Artist => "artist_sort",
            Self::AlbumArtist => "album_artist_sort",
            Self::Album => "album_sort",
            Self::Year => "year",
            Self::ReleaseDate => "release_date",
            Self::DateAdded => "date_added",
            Self::LastPlayed => "last_played",
            Self::PlayCount => "play_count",
            Self::UserRating => "rating",
            Self::Genre => "genre",
            Self::Bpm => "bpm",
            Self::Duration => "duration_millis",
            Self::Favorite => "favorite",
            Self::AlbumCount | Self::TrackCount => unreachable!("track sort field"),
        };
        let mut fields = vec![field];
        match self {
            Self::Artist | Self::AlbumArtist => {
                fields.extend([
                    "album_year",
                    "album_sort",
                    "album_key",
                    "disc_number",
                    "track_number",
                ]);
            }
            Self::Album => fields.extend(["album_key", "disc_number", "track_number"]),
            Self::TrackNumber => fields.push("track_number"),
            _ => {}
        }
        if self != Self::Title {
            fields.push("sort_text");
        }
        fields.push("key");
        fields
            .into_iter()
            .enumerate()
            .flat_map(|(index, field)| {
                let direction =
                    if descending && (index == 0 || (self == Self::TrackNumber && index == 1)) {
                        "DESC"
                    } else {
                        "ASC"
                    };
                let column = column(field);
                if matches!(field, "album_artist_sort" | "album_year" | "album_key") {
                    return vec![
                        format!("({column} IS NULL) ASC"),
                        format!("{column} {direction}"),
                    ];
                }
                let nulls = if matches!(
                    field,
                    "year"
                        | "release_date"
                        | "date_added"
                        | "last_played"
                        | "rating"
                        | "genre"
                        | "bpm"
                ) {
                    " NULLS LAST"
                } else {
                    ""
                };
                vec![format!("{column} {direction}{nulls}")]
            })
            .collect()
    }
}

#[derive(Clone, Debug, FromRow, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TrackArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

// Included only in bounded rows, with track and album joined by identity.
pub(crate) const TRACK_ARTISTS: &str = "(SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
    FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key)";
pub(crate) const TRACK_ALBUM_ARTISTS: &str = "(SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position)
    FROM effective_album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=track.album_key)";

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

impl Database {
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

pub(crate) fn downloaded_filter(query: &mut crate::query::OrderedQuery) {
    query.predicate.push_str(" AND EXISTS(SELECT 1 FROM local_access_files access WHERE access.media_uri=track.media_uri AND access.origin='download')");
}

pub(crate) fn track_filter(
    query: &mut crate::query::OrderedQuery,
    folder: Option<FolderKey>,
    filter: &str,
    favorites_only: bool,
) {
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND EXISTS(SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key={})",folder.raw()));
    }
    if favorites_only {
        query.predicate.push_str(&format!(
            " AND track.media_uri IN({})",
            crate::favorites::FAVORITE_URIS
        ));
    }
    let filter: String = filter.trim().to_lowercase().chars().take(256).collect();
    if !filter.is_empty() {
        let filter = crate::query::quote(&filter);
        query
            .predicate
            .push_str(&format!(" AND instr(track.normalized_search,{filter})>0"));
    }
}

pub(crate) fn displayed_stat(field: &str) -> String {
    let local = local_stat(field);
    let remote = if field == "last_played" {
        format!("(SELECT {field} FROM native_media_state WHERE media_uri=track.media_uri)")
    } else {
        format!(
            "COALESCE((SELECT {field} FROM native_media_state WHERE media_uri=track.media_uri),0)"
        )
    };
    format!(
        "CASE WHEN (SELECT kind FROM sources WHERE source_key=track.source_key) IN ('local','files','webdav','smb','documents') OR track.source_key IS NULL THEN {local} ELSE {remote} END"
    )
}

pub(crate) fn local_stat(field: &str) -> &'static str {
    match field {
        "play_count" => {
            "track.local_play_count+COALESCE((SELECT play_count FROM legacy_activity old JOIN sources origin ON origin.object_id=old.source_id WHERE origin.source_key=track.source_key AND old.track_object_id=track.object_id AND period='lifetime' AND item_kind='track'),0)"
        }
        "skip_count" => {
            "COALESCE((SELECT sum(skipped) FROM listens WHERE media_uri=track.media_uri),0)+COALESCE((SELECT skip_count FROM legacy_activity old JOIN sources origin ON origin.object_id=old.source_id WHERE origin.source_key=track.source_key AND old.track_object_id=track.object_id AND period='lifetime' AND item_kind='track'),0)"
        }
        "last_played" => {
            "(SELECT max(value) FROM (SELECT max(started_at) value FROM listens WHERE media_uri=track.media_uri UNION ALL SELECT last_played_at FROM legacy_activity old JOIN sources origin ON origin.object_id=old.source_id WHERE origin.source_key=track.source_key AND old.track_object_id=track.object_id AND period='lifetime' AND item_kind='track'))"
        }
        _ => unreachable!(),
    }
}

pub(crate) async fn load_track_rows(
    connection: &mut SqliteConnection,
    keys: &[TrackKey],
) -> LibraryResult<Vec<TrackView>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "WITH requested AS (SELECT value track_key,key position FROM json_each(?1))
        SELECT track.track_key,track.source_key,source.object_id source_id,track.object_id,track.media_uri,
        json_patch(track.metadata,json_object('date_added',date(COALESCE(unixepoch(track.date_added),track.first_seen_at),'unixepoch'),'album_display_artist',COALESCE(album.display_artist,json_extract(track.metadata,'$.album_display_artist')),'musicbrainz_album_id',COALESCE(album.musicbrainz_release_id,json_extract(track.metadata,'$.musicbrainz_album_id')),'musicbrainz_release_group_id',COALESCE(album.musicbrainz_release_group_id,json_extract(track.metadata,'$.musicbrainz_release_group_id')),'primary_artist_musicbrainz_id',COALESCE((SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=track.track_key ORDER BY credit.position LIMIT 1),json_extract(track.metadata,'$.primary_artist_musicbrainz_id')))) metadata,COALESCE(source.display_name,'') source_name,
        CASE WHEN source.kind='local' THEN(SELECT access.source_path FROM local_track_access access WHERE access.track_key=track.track_key) ELSE track.source_path END source_path,
        coalesce((SELECT access.cue_path FROM local_track_access access WHERE access.track_key=track.track_key),track.cue_path) cue_path,
        track.cue_start_millis,track.cue_end_millis,track.loudness_analysis_key,
        CASE WHEN album.indexed OR track.indexed OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1) THEN track.album_key END album_key,
        CASE WHEN album.indexed OR track.indexed OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1) THEN album.media_uri END album_media_uri,
        COALESCE(state.favorite,native.favorite,0) favorite,COALESCE(state.rating,native.rating)/10 rating,
        {} last_played,{} play_count,{} skip_count,
        EXISTS(SELECT 1 FROM local_access_files WHERE media_uri=track.media_uri AND origin='download') is_downloaded,
        {} artists,{} album_artists,
        (SELECT json_group_array(json_object('genre_key',genre.genre_key,'name',genre.name) ORDER BY relation.position)
            FROM track_genres relation JOIN genres genre USING(genre_key) WHERE relation.track_key=track.track_key) genres,
        {} artwork_binding
        FROM requested JOIN tracks track USING(track_key)
        LEFT JOIN sources source USING(source_key) LEFT JOIN albums album USING(album_key)
        LEFT JOIN user_media_state state ON state.media_uri=track.media_uri
        LEFT JOIN native_media_state native ON native.media_uri=track.media_uri
        ORDER BY requested.position",
        displayed_stat("last_played"),displayed_stat("play_count"),displayed_stat("skip_count"),
        TRACK_ARTISTS, TRACK_ALBUM_ARTISTS, crate::artwork::track_artwork_sql("track")
    );
    let mut rows: Vec<TrackView> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(serde_json::to_string(keys)?)
        .fetch_all(&mut *connection)
        .await?;
    for row in &mut rows {
        if row.metadata.title.is_empty() {
            row.metadata.title.clone_from(&row.media_uri);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::open_writer, schema};

    #[tokio::test]
    async fn bounded_track_row_window_preserves_order_and_relations() {
        let file = tempfile::NamedTempFile::new().expect("create Track row Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track row Store");
        schema::initialize_durable(&mut connection)
            .await
            .expect("initialize durable Store");
        crate::connect::initialize(&mut connection).await.unwrap();
        let source = sqlx::query_scalar::<_, SourceKey>(
            "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32)) RETURNING source_key",
        )
        .fetch_one(&mut connection)
        .await
        .expect("insert source");
        let artist = sqlx::query_scalar::<_, ArtistKey>("INSERT INTO artists(source_key,object_id,media_uri,metadata) VALUES (?1,'artist','rufin:source/artist/%73%6F%75%72%63%65/%61%72%74%69%73%74',jsonb(json_object('name','Artist','sort_text','artist'))) RETURNING artist_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Artist");
        let genre = sqlx::query_scalar::<_, GenreKey>("INSERT INTO genres(source_key,object_id,media_uri,metadata) VALUES (?1,'genre','genre:one',jsonb(json_object('name','Genre','sort_text','genre'))) RETURNING genre_key")
            .bind(source).fetch_one(&mut connection).await.expect("insert Genre");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<255) INSERT INTO tracks(source_key,object_id,media_uri,normalized_search,indexed,metadata) SELECT ?1,'track-'||value,'file:///track-'||value,'track artist',1,jsonb(json_object('title',COALESCE('Track',''),'album',COALESCE('',''),'artist',COALESCE('Artist',''),'sort_text',COALESCE('track-'||printf('%03d',value),''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(0,0),'track_number',COALESCE(value,0))) FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert Track window");
        sqlx::query("INSERT INTO track_artists(track_key,artist_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(artist).bind(source).execute(&mut connection).await.expect("insert Track Artists");
        sqlx::query("INSERT INTO track_genres(track_key,genre_key,position) SELECT track_key,?1,0 FROM tracks WHERE source_key=?2")
            .bind(genre).bind(source).execute(&mut connection).await.expect("insert Track Genres");
        let mut keys = sqlx::query_scalar::<_, TrackKey>(
            "SELECT track_key FROM tracks WHERE source_key=?1 ORDER BY track_key",
        )
        .bind(source)
        .fetch_all(&mut connection)
        .await
        .expect("read Track window keys");
        keys.reverse();
        let one = load_track_rows(&mut connection, &keys[..1])
            .await
            .expect("load one Track row");
        let window = load_track_rows(&mut connection, &keys)
            .await
            .expect("load bounded Track row window");
        assert_eq!(one.len(), 1);
        assert_eq!(window.len(), 256);
        assert_eq!(one[0].track_key, keys[0]);
        for (row, key) in window.iter().zip(&keys) {
            assert_eq!(row.track_key, *key);
            assert_eq!(row.links.artists.len(), 1);
            assert_eq!(row.links.artists[0].artist_key, artist);
            assert_eq!(row.links.genres.len(), 1);
            assert_eq!(row.links.genres[0].genre_key, genre);
        }
        let album = sqlx::query_scalar::<_, AlbumKey>("INSERT INTO albums(source_key,object_id,media_uri,metadata) VALUES (?1,'album','album:one',jsonb(json_object('title',COALESCE('Album',''),'artist',COALESCE('Artist',''),'sort_text',COALESCE('album',''),'year',2000))) RETURNING album_key")
            .bind(source).fetch_one(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO album_order(source_key,album_key,title_sort,album_artist_sort,year_sort,title_search,artist_search) VALUES(?1,?2,'album','artist',2000,'album','artist')").bind(source).bind(album).execute(&mut connection).await.unwrap();
        sqlx::query("INSERT INTO album_artists VALUES(?1,?2,0)")
            .bind(album)
            .bind(artist)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("UPDATE tracks SET album_key=?1 WHERE track_number<128")
            .bind(album)
            .execute(&mut connection)
            .await
            .unwrap();
        let database = Database::open(file.path()).await.unwrap();
        for sort in [
            CatalogSort::Artist,
            CatalogSort::AlbumArtist,
            CatalogSort::Album,
        ] {
            for descending in [false, true] {
                let query = crate::query::track_query_on(
                    &mut connection,
                    &crate::CatalogQuery {
                        subject: crate::CatalogSubject::Tracks {
                            scope: crate::TrackScope::Tracks(source),
                            sort: Some(sort),
                        },
                        descending,
                        ..crate::CatalogQuery::tracks(crate::TrackScope::Tracks(source))
                    },
                )
                .await
                .unwrap();
                let plan =
                    sqlx::query_as::<_, (i64, i64, i64, String)>(sqlx::AssertSqlSafe(format!(
                        "EXPLAIN QUERY PLAN {} LIMIT 48",
                        query.select("track.track_key")
                    )))
                    .fetch_all(&mut connection)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.3)
                    .collect::<Vec<_>>()
                    .join(" | ");
                if matches!(sort, CatalogSort::Album | CatalogSort::AlbumArtist) {
                    assert!(plan.contains("MERGE (UNION ALL)"), "{plan}");
                    assert!(plan.contains("album_order_"), "{plan}");
                }
                if sort == CatalogSort::AlbumArtist {
                    let sections = crate::CatalogQuery {
                        subject: crate::CatalogSubject::Tracks {
                            scope: crate::TrackScope::Tracks(source),
                            sort: Some(sort),
                        },
                        descending,
                        ..crate::CatalogQuery::tracks(crate::TrackScope::Tracks(source))
                    }
                    .sections(&database, &ReadCancellation::new())
                    .await
                    .unwrap();
                    assert_eq!(
                        sections
                            .iter()
                            .map(|section| (section.title.as_str(), section.index))
                            .collect::<Vec<_>>(),
                        [("A", 0), ("#", 128)]
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn object_identity_order_pages_without_losing_a_large_folder() {
        let file = tempfile::NamedTempFile::new().expect("create Track identity Store");
        let mut connection = open_writer(file.path())
            .await
            .expect("open Track identity Store");
        schema::initialize_durable(&mut connection)
            .await
            .expect("initialize durable Store");
        let source = sqlx::query_scalar::<_, SourceKey>(
            "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('source','Source','source',zeroblob(32)) RETURNING source_key",
        )
        .fetch_one(&mut connection)
        .await
        .expect("insert source");
        sqlx::query("WITH RECURSIVE sequence(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM sequence WHERE value<299) INSERT INTO tracks(source_key,object_id,media_uri,normalized_search,indexed,metadata) SELECT ?1,'track-'||printf('%03d',value),'file:///track-'||printf('%03d',value),'track artist',1,jsonb(json_object('title',COALESCE('Track',''),'album',COALESCE('',''),'artist',COALESCE('Artist',''),'sort_text',COALESCE('track-'||printf('%03d',value),''),'duration_millis',COALESCE(1000,0),'disc_number',COALESCE(0,0),'track_number',COALESCE(value,0))) FROM sequence")
            .bind(source).execute(&mut connection).await.expect("insert large Folder Track order");
        drop(connection);

        let database = Database::open(file.path())
            .await
            .expect("open Track identity Database");
        let object_ids = (0..300)
            .map(|index| format!("track-{index:03}"))
            .collect::<Vec<_>>();
        let cancel = ReadCancellation::new();
        assert_eq!(
            database
                .catalog_keys(
                    crate::CatalogKind::Track,
                    Some(source),
                    &object_ids,
                    &cancel
                )
                .await
                .map(|rows| rows.len() as i64)
                .unwrap(),
            300
        );
        let query = crate::CatalogQuery {
            filter: "track".into(),
            subject: crate::CatalogSubject::Tracks {
                scope: crate::TrackScope::Objects {
                    source,
                    ids: object_ids
                        .clone()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .into(),
                },
                sort: Some(CatalogSort::Title),
            },
            ..crate::CatalogQuery::tracks(crate::TrackScope::Objects {
                source,
                ids: object_ids
                    .clone()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .into(),
            })
        };
        assert_eq!((&query).count(&database, &cancel).await.unwrap(), 300);
        let mut ordered = Vec::new();
        for offset in (0..300).step_by(64) {
            ordered.extend(
                (&query)
                    .rows(&database, offset, 64, &cancel)
                    .await
                    .map(|rows| match rows {
                        crate::CatalogRows::Tracks(rows) => rows,
                        _ => unreachable!("typed song projection"),
                    })
                    .unwrap()
                    .into_iter()
                    .map(|row| row.track_key),
            );
        }
        assert_eq!(
            ordered,
            (0..300)
                .map(|index| TrackKey::from_raw(index + 1))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            database
                .selected_track_keys(&query.clone().selection(), &cancel)
                .await
                .unwrap(),
            ordered
        );
        assert_eq!(
            database
                .selected_track_keys(
                    &crate::TrackSelection::Query {
                        query: query.clone(),
                        ranges: Some(vec![1..3, 129..132])
                    },
                    &cancel
                )
                .await
                .unwrap(),
            [2, 3, 130, 131, 132].map(TrackKey::from_raw),
        );
        assert_eq!(
            (&query)
                .sections(&database, &cancel)
                .await
                .unwrap()
                .into_iter()
                .map(|section| (section.title, section.index))
                .collect::<Vec<_>>(),
            [("T".to_string(), 0)],
        );
        assert!(
            (&crate::CatalogQuery {
                subject: crate::CatalogSubject::Tracks {
                    scope: match &query.subject {
                        crate::CatalogSubject::Tracks { scope, .. } => scope.clone(),
                        _ => unreachable!(),
                    },
                    sort: Some(CatalogSort::TrackNumber)
                },
                ..query.clone()
            })
                .disc_sections(&database, &cancel)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            (&crate::CatalogQuery {
                favorites_only: true,
                ..query.clone()
            })
                .count(&database, &cancel)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            (&crate::CatalogQuery {
                filter: "missing".into(),
                ..query
            })
                .count(&database, &cancel)
                .await
                .unwrap(),
            0
        );
    }
}
