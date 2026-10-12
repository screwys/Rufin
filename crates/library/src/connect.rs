//! Durable Connect capture and replay at the existing Store writer boundary.
//! Journal writes share the user edit's transaction; replay never enters service outboxes.
use crate::{Database, LibraryError, LibraryResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Connection, QueryBuilder, Row, Sqlite, SqliteConnection};
use std::collections::BTreeSet;

pub const CONNECT_PAGE_SIZE: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConnectRecord {
    pub kind: String,
    pub key: String,
    pub value: Option<Value>,
}
#[derive(Clone, Debug)]
pub struct ConnectChange {
    pub sequence: i64,
    pub record: ConnectRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct ConnectRoot {
    pub id: String,
    pub label: String,
}

// Fixed schema projections, not arbitrary SQL supplied by a peer.
struct Projection {
    kind: &'static str,
    table: &'static str,
    key: &'static str,
    fields: &'static str,
    extra: &'static str,
}

fn source_projection(kind: &str) -> bool {
    matches!(
        kind,
        "track" | "album" | "artist" | "genre" | "mood" | "folder" | "native_playlist"
    )
}

fn captured_projection(kind: &str) -> bool {
    matches!(
        kind,
        "playlist"
            | "playlist_order"
            | "playlist_artwork"
            | "entry"
            | "smart"
            | "state"
            | "listen"
            | "legacy_activity"
            | "track"
            | "cache_lyrics"
    )
}

fn projection_filter(projection: &Projection, alias: &str) -> String {
    match projection.kind {
        "native_playlist" | "playlist_order" => format!("{alias}.owner='source'"),
        "playlist" | "playlist_artwork" => format!("{alias}.owner='rufin'"),
        "entry" | "native_entry" => format!(
            "EXISTS(SELECT 1 FROM playlists WHERE playlist_key={alias}.playlist_key AND owner='{}')",
            if projection.kind == "entry" {
                "rufin"
            } else {
                "source"
            }
        ),
        "track" => needed_media_sql(alias),
        _ => "1".into(),
    }
}
pub(crate) const NEEDED_TRACK_KEYS: &str = "SELECT entry.track_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.owner='rufin' UNION SELECT track_key FROM queue_entries UNION SELECT track.track_key FROM local_locators locator JOIN tracks track USING(media_uri) WHERE locator.origin='download'";

pub(crate) fn needed_media_sql(alias: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE entry.track_key={alias}.track_key AND playlist.owner='rufin') OR EXISTS(SELECT 1 FROM queue_entries WHERE track_key={alias}.track_key) OR EXISTS(SELECT 1 FROM local_locators WHERE media_uri={alias}.media_uri AND origin='download')"
    )
}

const PROJECTIONS: &[Projection] = &[
    Projection {
        kind: "playlist_order",
        table: "main.playlists",
        key: "json_array((SELECT object_id FROM sources WHERE source_key=r.source_key),r.object_id)",
        fields: "position",
        extra: "'source_owned',json('true')",
    },
    Projection {
        kind: "native_playlist",
        table: "main.playlists",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text artwork_binding writable metadata_writable indexed provider_revision valid_until",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "native_entry",
        table: "main.playlist_entries",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key),r.object_id)",
        fields: "object_id media_uri title artist album album_display_artist duration_millis disc_number track_number year release_date source_format musicbrainz_recording_id musicbrainz_release_track_id position",
        extra: "'playlist',json_array((SELECT object_id FROM main.sources WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key))",
    },
    Projection {
        kind: "playlist",
        table: "main.playlists",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text position",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "playlist_artwork",
        table: "main.playlists",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "artwork_bytes artwork_mime artwork_revision",
        extra: "",
    },
    Projection {
        kind: "entry",
        table: "main.playlist_entries",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key),r.object_id)",
        fields: "object_id media_uri title artist album album_display_artist duration_millis disc_number track_number year release_date source_format musicbrainz_recording_id musicbrainz_release_track_id position",
        extra: "'playlist',json_array((SELECT object_id FROM main.sources WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key))",
    },
    Projection {
        kind: "smart",
        table: "main.smart_playlists",
        key: "r.object_id",
        fields: "object_id name normalized_name definition_json position artwork_bytes artwork_revision",
        extra: "",
    },
    Projection {
        kind: "state",
        table: "main.user_media_state",
        key: "r.media_uri",
        fields: "media_uri favorite rating",
        extra: "",
    },
    Projection {
        kind: "cache_lyrics",
        table: "main.lyrics_cache",
        key: "json_array(r.media_uri,r.authority,r.role,r.language,r.script)",
        fields: "media_uri authority role language script cache_input_digest lyrics updated_at",
        extra: "",
    },
    Projection {
        kind: "listen",
        table: "main.listens",
        key: "coalesce(r.external_id,'legacy-'||r.media_uri||'-'||r.started_at||'-'||r.listen_key)",
        fields: "external_id source_id media_uri track_title artist_name album_title disc_number track_number year release_date source_format musicbrainz_recording_id musicbrainz_release_track_id started_at local_period duration_millis listened_millis skipped",
        extra: "",
    },
    Projection {
        kind: "legacy_activity",
        table: "main.legacy_activity",
        key: "json_array(r.source_id,r.period,r.item_kind,r.track_object_id)",
        fields: "source_id period item_kind track_object_id play_count skip_count last_played_at",
        extra: "",
    },
    Projection {
        kind: "track",
        table: "main.tracks",
        key: "r.media_uri",
        fields: "object_id media_uri title normalized_search display_album display_artist album_display_artist musicbrainz_album_id musicbrainz_release_group_id primary_artist_musicbrainz_id sort_text duration_millis disc_number track_number year release_date date_added source_format comment bpm musicbrainz_recording_id musicbrainz_release_track_id source_favorite source_rating first_seen_at artwork_binding",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key),'album_id',(SELECT object_id FROM main.albums WHERE album_key=r.album_key),'_local_root',coalesce((SELECT root FROM main.local_files WHERE source_key=r.source_key AND path=r.source_path),(SELECT root FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),'relative_path',coalesce((SELECT relative_path FROM main.local_files WHERE source_key=r.source_key AND path=r.source_path),(SELECT relative_path FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),'revision',(SELECT revision FROM main.local_files WHERE source_key=r.source_key AND path=coalesce(r.source_path,(SELECT path FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)) LIMIT 1)",
    },
    Projection {
        kind: "album",
        table: "main.albums",
        key: "r.media_uri",
        fields: "object_id media_uri title normalized_title display_artist sort_text year release_date date_added musicbrainz_release_id musicbrainz_release_group_id is_compilation release_lookup_identity source_favorite source_rating first_seen_at artwork_binding",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "artist",
        table: "main.artists",
        key: "r.media_uri",
        fields: "object_id media_uri name normalized_name sort_text musicbrainz_artist_id source_favorite source_rating artwork_binding",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "genre",
        table: "main.genres",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text artwork_binding",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "mood",
        table: "main.moods",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "folder",
        table: "main.folders",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text artwork_binding",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key)",
    },
];

const LINKS: &[(&str, &str, &str)] = &[
    ("album", "artist", "album_artists"),
    ("album", "genre", "album_genres"),
    ("track", "artist", "track_artists"),
    ("track", "genre", "track_genres"),
    ("track", "mood", "track_moods"),
    ("track", "folder", "track_folders"),
];

fn portable_relative(value: &mut Value) {
    if value.get("local_media").and_then(Value::as_bool) == Some(false) {
        value.as_object_mut().unwrap().remove("local_media");
    }
    if let Some(backing) = value
        .as_object_mut()
        .and_then(|value| value.remove("_document_backing"))
        .and_then(|value| value.as_str().map(str::to_owned))
    {
        value["backing_id"] = crate::document_media_uri(&backing).into();
    }
    if let Some(root) = value
        .as_object_mut()
        .and_then(|v| v.remove("_local_root"))
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        value["root_id"] = blake3::hash(root.as_bytes()).to_hex().to_string().into();
        value["root_label"] = std::path::Path::new(&root)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
            .into();
    }
    if let Some(relative) = value["relative_path"].as_str() {
        value["relative_path"] = Value::String(
            std::path::Path::new(relative)
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        );
    }
}

fn object(projection: &Projection, alias: &str) -> String {
    let mut pairs = projection
        .fields
        .split_whitespace()
        .map(|field| if matches!(projection.kind, "entry" | "native_entry") && !matches!(field, "object_id" | "position") {
            let column = match field { "artist" => "display_artist", "album" => "display_album", _ => field };
            if field == "album_display_artist" {
                format!("'{field}',(SELECT COALESCE(album.display_artist,track.album_display_artist) FROM tracks track LEFT JOIN albums album USING(album_key) WHERE track.track_key={alias}.track_key)")
            } else { format!("'{field}',(SELECT {column} FROM tracks WHERE track_key={alias}.track_key)") }
        } else if projection.kind == "track" && field == "album_display_artist" {
            format!("'{field}',COALESCE((SELECT display_artist FROM albums WHERE album_key={alias}.album_key),{alias}.album_display_artist)")
        } else if matches!(field, "artwork_binding" | "artwork_bytes" | "cache_input_digest") {
            format!("'{field}',CASE WHEN {alias}.{field} IS NULL THEN NULL ELSE hex({alias}.{field}) END")
        } else { format!("'{field}',{alias}.{field}") })
        .collect::<Vec<_>>();
    if !projection.extra.is_empty() {
        pairs.push(projection.extra.replace("r.", &format!("{alias}.")));
    }
    if projection.kind == "track" {
        pairs.push(format!("'local_media',json(CASE WHEN EXISTS(SELECT 1 FROM main.local_files f WHERE f.source_key={alias}.source_key AND f.path={alias}.source_path AND f.path LIKE 'rufin-document:%') OR EXISTS(SELECT 1 FROM main.local_locators l WHERE l.media_uri={alias}.media_uri AND l.origin IN ('local','import') AND l.access_uri LIKE 'content://%') THEN 'true' ELSE 'false' END),'cue_start_millis',{alias}.cue_start_millis,'cue_end_millis',{alias}.cue_end_millis,'_document_backing',(SELECT f.native_id FROM main.local_files f WHERE f.source_key={alias}.source_key AND f.path={alias}.source_path AND f.path LIKE 'rufin-document:%')"));
    }
    format!("json_object({})", pairs.join(","))
}

pub(crate) async fn initialize(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS connect_capture(singleton INTEGER PRIMARY KEY CHECK(singleton=1), enabled INTEGER NOT NULL DEFAULT 0, applying INTEGER NOT NULL DEFAULT 0) STRICT;
        INSERT OR IGNORE INTO connect_capture(singleton) VALUES(1);
        CREATE TABLE IF NOT EXISTS connect_changes(sequence INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL, object_key TEXT NOT NULL, payload TEXT, UNIQUE(kind,object_key)) STRICT;
        CREATE TABLE IF NOT EXISTS connect_collection(media_uri TEXT PRIMARY KEY,payload TEXT NOT NULL) STRICT;
        CREATE TABLE IF NOT EXISTS connect_roots(source_id TEXT NOT NULL,id TEXT NOT NULL,label TEXT NOT NULL,PRIMARY KEY(source_id,id)) STRICT;
        CREATE INDEX IF NOT EXISTS connect_collection_source_album ON connect_collection(json_extract(payload,'$.source_id'),json_extract(payload,'$.album_id'));
        CREATE TABLE IF NOT EXISTS connect_seed(kind TEXT PRIMARY KEY,cursor INTEGER NOT NULL DEFAULT 0,complete INTEGER NOT NULL DEFAULT 0) STRICT;
        CREATE TABLE IF NOT EXISTS connect_playlist_seed(singleton INTEGER PRIMARY KEY CHECK(singleton=1),playlist INTEGER NOT NULL,position INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO connect_playlist_seed VALUES(1,0,-1);")
        .execute(&mut *connection).await?;
    for kind in PROJECTIONS
        .iter()
        .filter(|projection| captured_projection(projection.kind))
        .map(|projection| projection.kind)
    {
        sqlx::query("INSERT OR IGNORE INTO connect_seed(kind) VALUES(?1)")
            .bind(kind)
            .execute(&mut *connection)
            .await?;
    }
    let mut transaction = connection.begin().await?;
    sqlx::query("DELETE FROM connect_seed WHERE kind NOT IN ('playlist','playlist_order','playlist_artwork','entry','smart','state','listen','legacy_activity','track','cache_lyrics')")
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
    if sqlx::query_scalar::<_, bool>("SELECT enabled FROM connect_capture WHERE singleton=1")
        .fetch_one(&mut *connection)
        .await?
    {
        install_capture(connection).await?;
    }
    Ok(())
}

async fn install_capture(connection: &mut SqliteConnection) -> LibraryResult<()> {
    for projection in PROJECTIONS
        .iter()
        .filter(|projection| captured_projection(projection.kind))
    {
        for (operation, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
            let key = projection.key.replace("r.", &format!("{row}."));
            let payload = if operation == "DELETE" {
                "NULL".to_string()
            } else {
                object(projection, row)
            };
            let mut predicate = if operation == "UPDATE" {
                format!(
                    " AND {} IS NOT {}",
                    object(projection, "NEW"),
                    object(projection, "OLD")
                )
            } else {
                String::new()
            };
            if projection.kind == "playlist_artwork" {
                predicate = if operation == "UPDATE" {
                    " AND NEW.artwork_revision IS NOT OLD.artwork_revision".into()
                } else {
                    format!(" AND {row}.artwork_bytes IS NOT NULL")
                };
            }
            predicate.push_str(&format!(" AND ({})", projection_filter(projection, row)));
            if operation == "DELETE" && projection.kind == "entry" {
                predicate.push_str(
                    " AND EXISTS(SELECT 1 FROM main.playlists WHERE playlist_key=OLD.playlist_key)",
                );
            }
            let timing = if operation == "DELETE" {
                "BEFORE"
            } else {
                "AFTER"
            };
            let statement = format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS connect_{}_{operation} {timing} {operation} ON {} WHEN (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1){predicate} BEGIN INSERT INTO connect_changes(kind,object_key,payload) VALUES('{}',{key},{payload}) ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END",
                projection.kind, projection.table, projection.kind
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(&mut *connection)
                .await?;
        }
    }
    let tracks = PROJECTIONS
        .iter()
        .find(|projection| projection.kind == "track")
        .unwrap();
    let needed = projection_filter(tracks, "r");
    for operation in ["INSERT", "UPDATE"] {
        let statement = format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS connect_locator_{operation} AFTER {operation} ON main.local_locators WHEN NEW.origin IN ('local','import') AND (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1) BEGIN INSERT INTO connect_changes(kind,object_key,payload) SELECT 'track',r.media_uri,{} FROM main.tracks r WHERE r.media_uri=NEW.media_uri AND ({needed}) ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END",
            object(tracks, "r")
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await?;
        let statement = format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS connect_local_file_{operation} AFTER {operation} ON main.local_files WHEN (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1) BEGIN INSERT INTO connect_changes(kind,object_key,payload) SELECT 'track',r.media_uri,{} FROM main.tracks r WHERE r.source_key=NEW.source_key AND r.source_path=NEW.path AND ({needed}) ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END",
            object(tracks, "r")
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await?;
    }
    for (table, suffix) in [
        ("queue_entries", ""),
        (
            "playlist_entries",
            " AND EXISTS(SELECT 1 FROM playlists WHERE playlist_key=NEW.playlist_key AND owner='rufin')",
        ),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS connect_{table}_media AFTER INSERT ON {table} WHEN (SELECT enabled AND NOT applying FROM connect_capture WHERE singleton=1){suffix} BEGIN INSERT INTO connect_changes(kind,object_key,payload) SELECT 'track',r.media_uri,{} FROM tracks r WHERE r.track_key=NEW.track_key ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END",
            object(tracks, "r")
        ))).execute(&mut *connection).await?;
    }
    Ok(())
}

impl Database {
    pub async fn connect_initialize_profile(&self) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::raw_sql("DELETE FROM connect_changes; UPDATE connect_seed SET cursor=0,complete=0; UPDATE connect_playlist_seed SET playlist=0,position=-1; UPDATE connect_capture SET enabled=1,applying=0;").execute(&mut *transaction).await?;
        install_capture(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn connect_capture_enabled(&self, enabled: bool) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let current: bool =
            sqlx::query_scalar("SELECT enabled FROM connect_capture WHERE singleton=1")
                .fetch_one(&mut *connection)
                .await?;
        if current == enabled {
            return Ok(());
        }
        let mut transaction = connection.begin().await?;
        if enabled {
            install_capture(&mut transaction).await?;
        } else {
            let triggers: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_temp_master WHERE type='trigger' AND name GLOB 'connect_*'")
                .fetch_all(&mut *transaction).await?;
            for trigger in triggers {
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                    "DROP TRIGGER temp.\"{}\"",
                    trigger.replace('"', "\"\"")
                )))
                .execute(&mut *transaction)
                .await?;
            }
        }
        sqlx::query("UPDATE connect_capture SET enabled=?1 WHERE singleton=1")
            .bind(enabled)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Seeds one bounded page. Subsequent calls resume after interruption.
    pub async fn connect_seed_page(&self) -> LibraryResult<bool> {
        self.connect_seed_page_for_cache(false).await
    }

    pub async fn connect_seed_cache_page(&self) -> LibraryResult<bool> {
        self.connect_seed_page_for_cache(true).await
    }

    async fn connect_seed_page_for_cache(&self, cache: bool) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let row = sqlx::query(
            "SELECT kind,cursor FROM connect_seed WHERE complete=0 AND (kind GLOB 'cache_*')=?1 ORDER BY rowid LIMIT 1",
        )
        .bind(cache)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let kind: String = row.get(0);
        let cursor: i64 = row.get(1);
        let statement = if let Some(projection) = PROJECTIONS.iter().find(|p| p.kind == kind) {
            if kind == "track" {
                format!(
                    "SELECT r.rowid,{} AS object_key,{} AS payload FROM ({NEEDED_TRACK_KEYS}) needed JOIN tracks r USING(track_key) WHERE r.rowid>?1 ORDER BY r.rowid LIMIT ?2",
                    projection.key,
                    object(projection, "r")
                )
            } else if matches!(kind.as_str(), "playlist" | "smart") {
                format!(
                    "SELECT r.position+1,{} AS object_key,{} AS payload FROM {} r WHERE ({}) AND r.position>=?1 ORDER BY r.position LIMIT ?2",
                    projection.key,
                    object(projection, "r"),
                    projection.table,
                    projection_filter(projection, "r")
                )
            } else if kind == "entry" {
                let seed = "connect_playlist_seed";
                format!(
                    "SELECT r.rowid,{} AS object_key,{} AS payload FROM {} r WHERE ({}) AND (r.playlist_key,r.position)>(SELECT playlist,position FROM {seed} WHERE singleton=1) ORDER BY r.playlist_key,r.position LIMIT ?2",
                    projection.key,
                    object(projection, "r"),
                    projection.table,
                    projection_filter(projection, "r")
                )
            } else {
                format!(
                    "SELECT r.rowid,{} AS object_key,{} AS payload FROM {} r WHERE ({}) AND r.rowid>?1{} ORDER BY r.rowid LIMIT ?2",
                    projection.key,
                    object(projection, "r"),
                    projection.table,
                    projection_filter(projection, "r"),
                    if kind == "playlist_artwork" {
                        " AND r.artwork_bytes IS NOT NULL"
                    } else {
                        ""
                    },
                )
            }
        } else {
            unreachable!("fixed saved-object seed kind")
        };
        let page_size = if matches!(kind.as_str(), "playlist_artwork" | "cache_lyrics") {
            1
        } else {
            CONNECT_PAGE_SIZE
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(cursor)
            .bind(page_size as i64)
            .fetch_all(&mut *transaction)
            .await?;
        let last = rows.last().map_or(cursor, |row| row.get(0));
        if !rows.is_empty() {
            let mut insert = QueryBuilder::<Sqlite>::new(
                "INSERT OR IGNORE INTO connect_changes(kind,object_key,payload) ",
            );
            insert.push_values(&rows, |mut values, row| {
                values
                    .push_bind(&kind)
                    .push_bind(row.get::<String, _>(1))
                    .push_bind(row.get::<String, _>(2));
            });
            insert.build().execute(&mut *transaction).await?;
        }
        sqlx::query("UPDATE connect_seed SET cursor=?2,complete=?3 WHERE kind=?1")
            .bind(&kind)
            .bind(last)
            .bind(rows.len() < page_size)
            .execute(&mut *transaction)
            .await?;
        if kind == "entry" && !rows.is_empty() {
            sqlx::query("UPDATE connect_playlist_seed SET (playlist,position)=(SELECT playlist_key,position FROM main.playlist_entries WHERE rowid=?1) WHERE singleton=1").bind(last).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn connect_changes(&self) -> LibraryResult<Vec<ConnectChange>> {
        let mut connection = self.acquire_reader().await?;
        let rows=sqlx::query("SELECT sequence,kind,object_key,payload FROM connect_changes WHERE sequence<=coalesce((SELECT min(sequence) FROM connect_changes WHERE kind='playlist_artwork'),9223372036854775807) ORDER BY sequence LIMIT ?1").bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *connection).await?;
        rows.into_iter()
            .map(|row| {
                Ok(ConnectChange {
                    sequence: row.get(0),
                    record: ConnectRecord {
                        kind: row.get(1),
                        key: row.get(2),
                        value: row
                            .get::<Option<String>, _>(3)
                            .map(|s| {
                                serde_json::from_str::<Value>(&s).map(|mut value| {
                                    if row.get::<String, _>(1) == "track" {
                                        portable_relative(&mut value);
                                    }
                                    value
                                })
                            })
                            .transpose()?,
                    },
                })
            })
            .collect()
    }

    /// Acknowledge only the exact captured version, preserving edits made during publication.
    pub async fn connect_acknowledge(&self, changes: &[ConnectChange]) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        for page in changes.chunks(CONNECT_PAGE_SIZE) {
            let roots: Vec<_> = page
                .iter()
                .filter(|change| change.record.kind == "track")
                .filter_map(|change| change.record.value.as_ref())
                .collect();
            remember_roots(&mut transaction, &roots).await?;
            let mut delete =
                QueryBuilder::<Sqlite>::new("DELETE FROM connect_changes WHERE sequence IN (");
            let mut sequences = delete.separated(",");
            for change in page {
                sequences.push_bind(change.sequence);
            }
            sequences.push_unseparated(")");
            delete.build().execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Returns whether saved objects or known media changed. Listens are stored as local
    /// activity; recording a play does not reload an open collection.
    pub async fn connect_apply(&self, records: &[ConnectRecord]) -> LibraryResult<bool> {
        if records.is_empty() {
            return Ok(false);
        }
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::query("UPDATE connect_capture SET applying=1 WHERE singleton=1")
            .execute(&mut *transaction)
            .await?;
        let mut changed = false;
        let mut remaining = records;
        while let Some(record) = remaining.first() {
            if record.value.is_some()
                && (matches!(
                    record.kind.as_str(),
                    "track" | "album" | "artist" | "genre" | "mood" | "folder" | "native_playlist"
                ) || LINKS.iter().any(|(_, _, table)| *table == record.kind))
            {
                let mut keys = BTreeSet::new();
                let count = remaining
                    .iter()
                    .take(CONNECT_PAGE_SIZE)
                    .take_while(|next| {
                        next.kind == record.kind && next.value.is_some() && keys.insert(&next.key)
                    })
                    .count();
                changed |= if let Some((left, right, table)) =
                    LINKS.iter().find(|(_, _, table)| *table == record.kind)
                {
                    apply_link_page(&mut transaction, &remaining[..count], left, right, table)
                        .await?
                } else {
                    apply_catalog_page(&mut transaction, &remaining[..count]).await?
                };
                remaining = &remaining[count..];
            } else {
                let applied = apply_record(&mut transaction, record).await?;
                changed |= applied
                    && !matches!(
                        record.kind.as_str(),
                        "listen" | "legacy_activity" | "cache_lyrics"
                    );
                remaining = &remaining[1..];
            }
        }
        sqlx::query("UPDATE connect_capture SET applying=0 WHERE singleton=1")
            .execute(&mut *transaction)
            .await?;
        crate::collections::refresh_facts_on(&mut transaction).await?;
        transaction.commit().await?;
        Ok(changed)
    }

    /// Profile replacement changes Rufin data only. It never removes original media files.
    pub async fn connect_replace_profile(&self) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::raw_sql("UPDATE connect_capture SET applying=1; DELETE FROM main.playlists WHERE owner='rufin'; DELETE FROM smart_playlists; DELETE FROM user_media_state; DELETE FROM favorite_outbox; DELETE FROM listens; DELETE FROM legacy_activity; DELETE FROM local_locators; DELETE FROM connect_media_files; DELETE FROM queue_state; DELETE FROM queue_positions; DELETE FROM queue_entries; DELETE FROM queue_contexts; DELETE FROM connect_collection; DELETE FROM connect_roots; DELETE FROM connect_changes; UPDATE connect_seed SET complete=1; UPDATE connect_capture SET applying=0;").execute(&mut *transaction).await?;
        crate::collections::refresh_facts_on(&mut transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn connect_track_reference(&self, media_uri: &str) -> LibraryResult<Option<Value>> {
        let mut connection = self.acquire_reader().await?;
        let projection = PROJECTIONS.iter().find(|p| p.kind == "track").unwrap();
        let payload: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM tracks r WHERE media_uri=?1 AND (r.indexed=1 OR EXISTS(SELECT 1 FROM connect_collection transport WHERE transport.media_uri=r.media_uri))",
            object(projection, "r")
        )))
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let mut value: Value = serde_json::from_str(&payload)?;
        if let Some(transport) = sqlx::query_scalar::<_, String>(
            "SELECT payload FROM connect_collection WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?
        {
            if let Value::Object(fields) = serde_json::from_str(&transport)? {
                value.as_object_mut().unwrap().extend(fields);
            }
        }
        portable_relative(&mut value);
        value["root_count"] =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM connect_roots WHERE source_id=?1")
                .bind(value["source_id"].as_str())
                .fetch_one(&mut *connection)
                .await?
                .into();
        Ok(Some(value))
    }

    pub async fn connect_roots(&self, source_id: &str) -> LibraryResult<Vec<ConnectRoot>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT id,label FROM connect_roots WHERE source_id=?1 ORDER BY label,id",
        )
        .bind(source_id)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn connect_local_file(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<crate::LocalMediaLocation>> {
        let mut connection = self.acquire_reader().await?;
        let backing = crate::cue_media_parts(media_uri).map(|(_, backing, _, _)| backing);
        let paths:Vec<String>=sqlx::query_scalar("SELECT access_uri FROM main.local_locators WHERE media_uri IN (?1,?2) ORDER BY media_uri=?1 DESC, CASE origin WHEN 'local' THEN 0 WHEN 'import' THEN 1 WHEN 'mapping' THEN 2 ELSE 3 END LIMIT 4").bind(media_uri).bind(backing.as_deref()).fetch_all(&mut *connection).await?;
        Ok(paths.into_iter().find_map(|uri| {
            if uri.starts_with("content://") {
                return Some(crate::LocalMediaLocation::Document(uri));
            }
            crate::file_media_path(&uri)
                .filter(|path| path.is_file())
                .map(crate::LocalMediaLocation::File)
        }))
    }

    /// Register a completed transferred file or a trusted corresponding-folder file
    /// through the existing local-access owner. Mapping never enrolls a scan root.
    pub async fn connect_set_local_file(
        &self,
        media_uri: &str,
        path: &std::path::Path,
        managed: bool,
    ) -> LibraryResult<()> {
        let metadata = std::fs::metadata(path)?;
        let access_uri = url::Url::from_file_path(path)
            .map_err(|()| {
                LibraryError::InvalidRequest("Connect media path must be absolute".into())
            })?
            .to_string();
        self.connect_set_file_access(
            media_uri,
            managed,
            path.to_string_lossy().into_owned(),
            path.parent().unwrap_or(path).to_string_lossy().into_owned(),
            access_uri,
            i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
                .unwrap_or(0),
        )
        .await
    }

    pub async fn connect_set_document_file(
        &self,
        media_uri: &str,
        document_uri: &str,
        root: &str,
        managed: bool,
        size: Option<u64>,
    ) -> LibraryResult<()> {
        self.connect_set_file_access(
            media_uri,
            managed,
            document_uri.to_string(),
            root.to_string(),
            document_uri.to_string(),
            size.and_then(|size| i64::try_from(size).ok()).unwrap_or(0),
            0,
        )
        .await
    }

    pub async fn connect_document_mapping_matches(
        &self,
        media_uri: &str,
        document_uri: &str,
        root: &str,
        relative: &str,
    ) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM main.local_locators WHERE media_uri=?1 AND access_uri=?2 AND root=?3 AND relative_path=?4 AND origin IN ('mapping','download'))")
            .bind(media_uri).bind(document_uri).bind(root).bind(relative).fetch_one(&mut *reader).await?)
    }

    async fn connect_set_file_access(
        &self,
        media_uri: &str,
        managed: bool,
        path: String,
        root: String,
        access_uri: String,
        size_bytes: i64,
        mtime_ns: i64,
    ) -> LibraryResult<()> {
        let reference = self
            .connect_track_reference(media_uri)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest("Connect track is not in the collection".into())
            })?;
        let source = match reference["source_id"].as_str() {
            Some(id) => self.source_identity_key(&crate::SourceId::new(id)).await?,
            None => None,
        };
        self.upsert_local_access(
            source,
            &crate::LocalAccessWrite {
                media_uri: media_uri.to_owned(),
                origin: if managed {
                    crate::LocalAccessOrigin::Download
                } else {
                    crate::LocalAccessOrigin::Mapping
                },
                path,
                root,
                relative_path: reference["relative_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                size_bytes,
                mtime_ns,
                device_id: None,
                inode: None,
                parser_version: 1,
                title: reference["title"].as_str().unwrap_or_default().to_owned(),
                album: reference["display_album"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                artist: reference["display_artist"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                disc_number: reference["disc_number"].as_i64().unwrap_or(0),
                track_number: reference["track_number"].as_i64().unwrap_or(0),
                duration_millis: reference["duration_millis"].as_i64().unwrap_or(0),
                access_uri,
                loudness_analysis_key: None,
            },
        )
        .await?;
        Ok(())
    }
}

async fn playlist_key(connection: &mut SqliteConnection, identity: &str) -> LibraryResult<i64> {
    let identity: (Option<String>, String) = serde_json::from_str(identity)?;
    let source = match identity.0 {
        Some(source) => Some(crate::db::write_source_identity(connection, &source).await?),
        None => None,
    };
    if let Some(key) = sqlx::query_scalar(
        "SELECT playlist_key FROM main.playlists WHERE source_key IS ?1 AND object_id=?2",
    )
    .bind(source)
    .bind(&identity.1)
    .fetch_optional(&mut *connection)
    .await?
    {
        return Ok(key);
    }
    Ok(sqlx::query_scalar("INSERT INTO main.playlists(source_key,object_id,position) VALUES(?1,?2,(SELECT coalesce(max(position),-1)+1 FROM main.playlists)) RETURNING playlist_key").bind(source).bind(identity.1).fetch_one(connection).await?)
}

async fn remember_roots(connection: &mut SqliteConnection, values: &[&Value]) -> LibraryResult<()> {
    if !values.is_empty() {
        sqlx::query("INSERT INTO connect_roots(source_id,id,label) SELECT json_extract(value,'$.source_id'),json_extract(value,'$.root_id'),CASE WHEN json_type(value,'$.root_label')='text' THEN json_extract(value,'$.root_label') ELSE '' END FROM json_each(?1) WHERE json_type(value,'$.source_id')='text' AND json_type(value,'$.root_id')='text' ON CONFLICT(source_id,id) DO UPDATE SET label=excluded.label WHERE label IS NOT excluded.label")
            .bind(serde_json::to_string(values)?).execute(connection).await?;
    }
    Ok(())
}

async fn apply_catalog_page(
    connection: &mut SqliteConnection,
    records: &[ConnectRecord],
) -> LibraryResult<bool> {
    let before: i64 = sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(&mut *connection)
        .await?;
    for record in records {
        let value = record.value.as_ref().unwrap();
        if record.kind == "track" {
            let uri = value["media_uri"].as_str().unwrap_or(&record.key);
            remember_media(connection, uri, value).await?;
            let fields = [
                "source_id",
                "album_id",
                "root_id",
                "root_label",
                "relative_path",
                "revision",
                "local_media",
                "cue_start_millis",
                "cue_end_millis",
                "backing_id",
            ];
            let transport: serde_json::Map<String, Value> = fields
                .into_iter()
                .filter_map(|field| value.get(field).map(|value| (field.into(), value.clone())))
                .collect();
            remember_roots(connection, &[value]).await?;
            sqlx::query("INSERT INTO connect_collection(media_uri,payload) VALUES(?1,?2) ON CONFLICT(media_uri) DO UPDATE SET payload=excluded.payload WHERE payload IS NOT excluded.payload")
                .bind(uri).bind(serde_json::to_string(&transport)?).execute(&mut *connection).await?;
            continue;
        }
        let Some(source) = value["source_id"].as_str() else {
            continue;
        };
        let source = crate::db::write_source_identity(connection, source).await?;
        let projection = PROJECTIONS
            .iter()
            .find(|projection| projection.kind == record.kind)
            .unwrap();
        let fields = projection
            .fields
            .split_whitespace()
            .filter(|field| *field != "indexed")
            .collect::<Vec<_>>();
        let columns = fields.join(",");
        let values = fields
            .iter()
            .map(|field| {
                if *field == "artwork_binding" {
                    format!("unhex(json_extract(?2,'$.{field}'))")
                } else if *field == "metadata_writable" {
                    "COALESCE(json_extract(?2,'$.metadata_writable'),json_extract(?2,'$.writable'),1)".into()
                } else if *field == "source_favorite" {
                    "COALESCE(json_extract(?2,'$.source_favorite'),0)".into()
                } else {
                    format!("json_extract(?2,'$.{field}')")
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let updates = fields
            .iter()
            .filter(|field| **field != "object_id")
            .map(|field| {
                format!(
                    "{field}=COALESCE({table}.{field},excluded.{field})",
                    table = projection.table.trim_start_matches("main.")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let missing = fields
            .iter()
            .filter(|field| **field != "object_id")
            .map(|field| format!("({field} IS NULL AND excluded.{field} IS NOT NULL)"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let (extra, extra_values) = if record.kind == "native_playlist" {
            (
                ",owner,position",
                ",'source',(SELECT COALESCE(max(position)+1,0) FROM playlists)",
            )
        } else {
            ("", "")
        };
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {table}(source_key,indexed,{columns}{extra}) SELECT ?1,0,{values}{extra_values} WHERE true ON CONFLICT(source_key,object_id) DO UPDATE SET {updates} WHERE {missing}",table=projection.table)))
            .bind(source).bind(serde_json::to_string(value)?).execute(&mut *connection).await?;
    }
    Ok(sqlx::query_scalar::<_, i64>("SELECT total_changes()")
        .fetch_one(connection)
        .await?
        != before)
}

async fn remember_media(
    connection: &mut SqliteConnection,
    uri: &str,
    value: &Value,
) -> LibraryResult<crate::TrackKey> {
    let key = crate::tracks::remember_song(
        connection,
        uri,
        crate::tracks::SongMetadata {
            title: value["title"]
                .as_str()
                .or_else(|| value["track_title"].as_str()),
            artist: value["display_artist"]
                .as_str()
                .or_else(|| value["artist"].as_str())
                .or_else(|| value["artist_name"].as_str()),
            album: value["display_album"]
                .as_str()
                .or_else(|| value["album"].as_str())
                .or_else(|| value["album_title"].as_str()),
            album_display_artist: value["album_display_artist"].as_str(),
            duration_millis: value["duration_millis"].as_i64(),
            disc_number: value["disc_number"].as_i64(),
            track_number: value["track_number"].as_i64(),
            year: value["year"].as_i64(),
            release_date: value["release_date"].as_str(),
            source_format: value["source_format"].as_str(),
            musicbrainz_recording_id: value["musicbrainz_recording_id"].as_str(),
            musicbrainz_release_track_id: value["musicbrainz_release_track_id"].as_str(),
            musicbrainz_album_id: value["musicbrainz_album_id"].as_str(),
            musicbrainz_release_group_id: value["musicbrainz_release_group_id"].as_str(),
            primary_artist_musicbrainz_id: value["primary_artist_musicbrainz_id"].as_str(),
        },
    )
    .await?;
    let payload = serde_json::to_string(value)?;
    sqlx::query("UPDATE tracks SET artwork_binding=COALESCE(artwork_binding,unhex(json_extract(?2,'$.artwork_binding'))),comment=COALESCE(comment,json_extract(?2,'$.comment')),bpm=COALESCE(bpm,json_extract(?2,'$.bpm')),date_added=COALESCE(date_added,json_extract(?2,'$.date_added'))
        WHERE track_key=?1 AND ((artwork_binding IS NULL AND json_extract(?2,'$.artwork_binding') IS NOT NULL) OR (comment IS NULL AND json_extract(?2,'$.comment') IS NOT NULL) OR (bpm IS NULL AND json_extract(?2,'$.bpm') IS NOT NULL) OR (date_added IS NULL AND json_extract(?2,'$.date_added') IS NOT NULL))")
        .bind(key).bind(payload).execute(connection).await?;
    Ok(key)
}

async fn apply_link_page(
    connection: &mut SqliteConnection,
    records: &[ConnectRecord],
    left: &str,
    right: &str,
    table: &str,
) -> LibraryResult<bool> {
    let joins = format!(
        "FROM json_each(?1) j LEFT JOIN main.sources s ON s.object_id=json_extract(j.value,'$.value.source_id') LEFT JOIN main.{left}s l ON l.source_key=s.source_key AND l.object_id=json_extract(j.value,'$.value.left') LEFT JOIN main.{right}s r ON r.source_key=s.source_key AND r.object_id=json_extract(j.value,'$.value.right')"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT j.key,s.source_key,l.{left}_key,r.{right}_key,coalesce(json_extract(j.value,'$.value.position'),0),x.position {joins} LEFT JOIN main.{table} x ON x.{left}_key=l.{left}_key AND x.{right}_key=r.{right}_key WHERE l.indexed=0")))
        .bind(serde_json::to_string(records)?).fetch_all(&mut *connection).await?;
    let mut changed = Vec::new();
    for row in rows {
        let left_key: Option<i64> = row.get(2);
        let right_key: Option<i64> = row.get(3);
        if left_key.is_none() || right_key.is_none() {
            continue;
        }
        if row.get::<Option<i64>, _>(5).is_none() {
            changed.push(&records[row.get::<i64, _>(0) as usize]);
        }
    }
    if changed.is_empty() {
        return Ok(false);
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO main.{table}({left}_key,{right}_key,position) SELECT l.{left}_key,r.{right}_key,coalesce(json_extract(j.value,'$.value.position'),0) {joins} WHERE l.indexed=0 ON CONFLICT DO NOTHING")))
        .bind(serde_json::to_string(&changed)?).execute(connection).await?;
    Ok(true)
}

fn same_record(old: Option<&mut Value>, value: &Value) -> bool {
    let Some(Value::Object(old)) = old else {
        return false;
    };
    if value.get("artwork_binding").is_none() {
        old.remove("artwork_binding");
    }
    old.iter().all(|(field, old)| {
        let incoming = value.get(field).unwrap_or(&Value::Null);
        match (old, incoming) {
            (Value::Number(old), Value::Bool(incoming)) => {
                old.as_i64() == Some(i64::from(*incoming))
            }
            _ => incoming == old,
        }
    })
}

async fn apply_record(
    connection: &mut SqliteConnection,
    record: &ConnectRecord,
) -> LibraryResult<bool> {
    if record.kind == "playlist_artwork" {
        let payload = record
            .value
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let identity: (Option<String>, String) = serde_json::from_str(&record.key)?;
        return Ok(sqlx::query("UPDATE main.playlists SET artwork_bytes=unhex(json_extract(?3,'$.artwork_bytes')),artwork_mime=json_extract(?3,'$.artwork_mime'),artwork_revision=json_extract(?3,'$.artwork_revision'),artwork_binding=CASE WHEN json_extract(?3,'$.artwork_revision') IS NOT NULL THEN CAST(json_object('kind','playlist','source_id',?2,'object_id',?1,'revision',json_extract(?3,'$.artwork_revision')) AS BLOB) END WHERE owner='rufin' AND object_id=?1 AND source_key IS (SELECT source_key FROM main.sources WHERE object_id=?2) AND artwork_revision IS NOT json_extract(?3,'$.artwork_revision')")
            .bind(identity.1).bind(identity.0).bind(payload).execute(connection).await?.rows_affected()>0);
    }
    if record.kind == "root" {
        if let Some(value) = &record.value {
            remember_roots(connection, &[value]).await?;
        }
        return Ok(false);
    }
    if matches!(record.kind.as_str(), "playlist_order" | "smart_order") {
        let Some(value) = &record.value else {
            return Ok(false);
        };
        let (table, key_column, key) = if record.kind == "playlist_order" {
            (
                "playlists",
                "playlist_key",
                if value["source_owned"].as_bool() == Some(true) {
                    let (source_id, object_id): (Option<String>, String) =
                        serde_json::from_str(&record.key)?;
                    crate::playlists::write_playlist_identity(
                        connection,
                        &crate::PlaylistIdentity {
                            source_owned: true,
                            source_id,
                            object_id,
                            name: None,
                            position: value["position"].as_i64().unwrap_or(0),
                        },
                    )
                    .await?
                    .raw()
                } else {
                    playlist_key(connection, &record.key).await?
                },
            )
        } else {
            let key: Option<i64> = sqlx::query_scalar(
                "SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1",
            )
            .bind(&record.key)
            .fetch_optional(&mut *connection)
            .await?;
            let Some(key) = key else { return Ok(false) };
            ("smart_playlists", "smart_playlist_key", key)
        };
        let old: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT position FROM main.{table} WHERE {key_column}=?1"
        )))
        .bind(key)
        .fetch_one(&mut *connection)
        .await?;
        let maximum: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(max(position),0) FROM main.{table}"
        )))
        .fetch_one(&mut *connection)
        .await?;
        let desired = value["position"]
            .as_i64()
            .unwrap_or(old)
            .min(maximum)
            .max(0);
        if desired == old {
            return Ok(false);
        };
        let offset = maximum + 1;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE main.{table} SET position=position+?1"
        )))
        .bind(offset)
        .execute(&mut *connection)
        .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE main.{table} SET position=CASE WHEN {key_column}=?1 THEN ?2 ELSE position-?3+CASE WHEN ?2<?4 AND position-?3>=?2 AND position-?3<?4 THEN 1 WHEN ?2>?4 AND position-?3>?4 AND position-?3<=?2 THEN -1 ELSE 0 END END"))).bind(key).bind(desired).bind(offset).bind(old).execute(connection).await?;
        return Ok(true);
    }
    if LINKS.iter().any(|(_, _, table)| *table == record.kind) {
        return Ok(false);
    }
    let Some(projection) = PROJECTIONS.iter().find(|p| p.kind == record.kind) else {
        return Ok(false);
    };
    let identity = match record.kind.as_str() {
        "native_playlist" => "r.source_key=(SELECT source_key FROM main.sources WHERE object_id=json_extract(?1,'$[0]')) AND r.object_id=json_extract(?1,'$[1]')".to_owned(),
        "native_entry" => "r.playlist_key=(SELECT playlist_key FROM main.playlists WHERE source_key=(SELECT source_key FROM main.sources WHERE object_id=json_extract(?1,'$[0]')) AND object_id=json_extract(?1,'$[1]')) AND r.object_id=json_extract(?1,'$[2]')".to_owned(),
        _ => format!("{}=?1", projection.key),
    };
    let Some(value) = &record.value else {
        if record.kind == "track" {
            sqlx::query("DELETE FROM connect_collection WHERE media_uri=?1")
                .bind(&record.key)
                .execute(&mut *connection)
                .await?;
        }
        if source_projection(&record.kind) || record.kind == "native_entry" {
            return Ok(false);
        }
        let statement = format!(
            "DELETE FROM {} AS r WHERE ({identity}) AND ({})",
            projection.table,
            projection_filter(projection, "r")
        );
        return Ok(sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(&record.key)
            .execute(connection)
            .await?
            .rows_affected()
            > 0);
    };
    let payload = serde_json::to_string(value)?;
    let query = format!(
        "SELECT {} FROM {} r WHERE {identity}",
        object(projection, "r"),
        projection.table
    );
    let old: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .bind(&record.key)
        .fetch_optional(&mut *connection)
        .await?;
    let mut old = old
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?;
    let entry_track = if matches!(record.kind.as_str(), "entry" | "native_entry") {
        let uri = value["media_uri"].as_str().ok_or_else(|| {
            LibraryError::InvalidRequest("Connect playlist media is missing".into())
        })?;
        let before: i64 = sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(&mut *connection)
            .await?;
        let track = remember_media(connection, uri, value).await?;
        if old.as_ref().is_some_and(|old| {
            old["media_uri"] == value["media_uri"]
                && old["position"] == value["position"]
                && old["object_id"] == value["object_id"]
        }) {
            return Ok(sqlx::query_scalar::<_, i64>("SELECT total_changes()")
                .fetch_one(connection)
                .await?
                != before);
        }
        Some(track)
    } else {
        None
    };
    let remembered = if record.kind == "listen" {
        let uri = value["media_uri"].as_str().ok_or_else(|| {
            LibraryError::InvalidRequest("Connect listen media is missing".into())
        })?;
        let before: i64 = sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(&mut *connection)
            .await?;
        remember_media(connection, uri, value).await?;
        sqlx::query_scalar::<_, i64>("SELECT total_changes()")
            .fetch_one(&mut *connection)
            .await?
            != before
    } else {
        false
    };
    if same_record(old.as_mut(), value) {
        return Ok(remembered);
    }
    match record.kind.as_str() {
        "cache_lyrics" => {
            sqlx::query("INSERT INTO lyrics_cache(media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at) VALUES(json_extract(?1,'$[0]'),json_extract(?1,'$[1]'),json_extract(?1,'$[2]'),json_extract(?1,'$[3]'),json_extract(?1,'$[4]'),unhex(json_extract(?2,'$.cache_input_digest')),json_extract(?2,'$.lyrics'),json_extract(?2,'$.updated_at')) ON CONFLICT(media_uri,authority,role,language,script) DO UPDATE SET cache_input_digest=excluded.cache_input_digest,lyrics=excluded.lyrics,updated_at=excluded.updated_at")
                .bind(&record.key).bind(&payload).execute(connection).await?;
        }
        "state" => {
            sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating) VALUES(?1,json_extract(?2,'$.favorite'),json_extract(?2,'$.rating')) ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite,rating=excluded.rating").bind(&record.key).bind(&payload).execute(connection).await?;
        }
        "listen" => {
            // History import deliberately bypasses record_listen and its service outbox.
            let fields = projection.fields;
            let columns = fields.split_whitespace().collect::<Vec<_>>().join(",");
            let values = fields
                .split_whitespace()
                .map(|f| {
                    if f == "external_id" {
                        "?2".to_string()
                    } else {
                        format!("json_extract(?1,'$.{f}')")
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            let statement = format!(
                "INSERT INTO listens({columns}) VALUES({values}) ON CONFLICT(external_id) DO UPDATE SET listened_millis=max(listens.listened_millis,excluded.listened_millis),skipped=min(listens.skipped,excluded.skipped)"
            );
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .bind(payload)
                .bind(&record.key)
                .execute(connection)
                .await?;
        }
        "legacy_activity" => {
            sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES(json_extract(?1,'$.source_id'),json_extract(?1,'$.period'),json_extract(?1,'$.item_kind'),json_extract(?1,'$.track_object_id'),json_extract(?1,'$.play_count'),json_extract(?1,'$.skip_count'),json_extract(?1,'$.last_played_at')) ON CONFLICT(source_id,period,item_kind,track_object_id) DO UPDATE SET play_count=excluded.play_count,skip_count=excluded.skip_count,last_played_at=excluded.last_played_at").bind(payload).execute(connection).await?;
        }
        "playlist" => {
            let key = playlist_key(connection, &record.key).await?;
            sqlx::query("UPDATE main.playlists SET name=json_extract(?2,'$.name'),normalized_name=json_extract(?2,'$.normalized_name'),sort_text=json_extract(?2,'$.sort_text') WHERE playlist_key=?1 AND owner='rufin'").bind(key).bind(payload).execute(connection).await?;
        }
        "entry" | "native_entry" => {
            let (source, playlist, _): (Option<String>, String, String) =
                serde_json::from_str(&record.key)?;
            let table = projection.table;
            let key = if record.kind == "native_entry" {
                let row = sqlx::query("SELECT playlist_key,source_key,indexed FROM main.playlists WHERE owner='source' AND source_key=(SELECT source_key FROM main.sources WHERE object_id=?1) AND object_id=?2")
                    .bind(source).bind(playlist).fetch_optional(&mut *connection).await?;
                let Some(row) = row else {
                    return Err(LibraryError::ConnectPending);
                };
                if row.get::<bool, _>(2) {
                    return Ok(false);
                }
                row.get::<i64, _>(0)
            } else {
                playlist_key(connection, &serde_json::to_string(&(source, playlist))?).await?
            };
            let existing: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT position FROM {table} WHERE playlist_key=?1 AND object_id=?2"
            )))
            .bind(key)
            .bind(value["object_id"].as_str())
            .fetch_optional(&mut *connection)
            .await?;
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT coalesce(max(position),-1)+1 FROM {table} WHERE playlist_key=?1"
            )))
            .bind(key)
            .fetch_one(&mut *connection)
            .await?;
            let position = value["position"].as_i64().unwrap_or(count).min(count);
            let desired = position.min(count - i64::from(existing.is_some()));
            let moves_entries = existing.is_some_and(|old| old != desired)
                || (existing.is_none() && desired < count);
            let offset = count.saturating_add(1);
            // Move positions out of the unique-index range before closing/opening the gap.
            if moves_entries {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "UPDATE {table} SET position=position+?2 WHERE playlist_key=?1"
                )))
                .bind(key)
                .bind(offset)
                .execute(&mut *connection)
                .await?;
            }
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE playlist_key=?1 AND object_id=?2"
            )))
            .bind(key)
            .bind(value["object_id"].as_str())
            .execute(&mut *connection)
            .await?;
            if moves_entries {
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET position=position-?2-CASE WHEN ?4 IS NOT NULL AND position-?2>?4 THEN 1 ELSE 0 END+CASE WHEN position-?2-CASE WHEN ?4 IS NOT NULL AND position-?2>?4 THEN 1 ELSE 0 END>=?3 THEN 1 ELSE 0 END WHERE playlist_key=?1"))).bind(key).bind(offset).bind(desired).bind(existing).execute(&mut *connection).await?;
            }
            let track = entry_track.expect("entry media was admitted");
            sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) VALUES(?1,?2,?3,?4)")
                .bind(key).bind(value["object_id"].as_str()).bind(track).bind(desired).execute(connection).await?;
        }
        "smart" => {
            sqlx::query("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position,artwork_bytes,artwork_revision) VALUES(?1,json_extract(?2,'$.name'),json_extract(?2,'$.normalized_name'),json_extract(?2,'$.definition_json'),(SELECT coalesce(max(position),-1)+1 FROM smart_playlists),unhex(json_extract(?2,'$.artwork_bytes')),json_extract(?2,'$.artwork_revision')) ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,definition_json=excluded.definition_json,artwork_bytes=excluded.artwork_bytes,artwork_revision=excluded.artwork_revision").bind(&record.key).bind(payload).execute(connection).await?;
        }
        _ => unreachable!(),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FavoriteTarget, ReadCancellation, Scan};

    #[tokio::test]
    async fn saved_playlist_media_replay_preserves_artwork_without_catalog_membership() {
        let directory = tempfile::tempdir().unwrap();
        let source = Database::open(directory.path().join("source.sqlite"))
            .await
            .unwrap();
        let destination_path = directory.path().join("destination.sqlite");
        let destination = Database::open(&destination_path).await.unwrap();
        // A different source keeps the imported source key independent of the sender.
        crate::db::write_source_identity(
            destination.writer().await.unwrap().as_mut().unwrap(),
            "other",
        )
        .await
        .unwrap();
        let art = b"{\"source_id\":\"server\",\"image\":\"cover\"}";
        let mut scan = Scan::begin(&source, "server", "Server", "server", None)
            .await
            .unwrap();
        scan.write_album(&crate::AlbumInput {
            object_id: "album".into(),
            title: "Album".into(),
            display_artist: "Artist".into(),
            sort_text: "album".into(),
            artwork_binding: Some(art.to_vec()),
            favorite: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
        scan.write_artist(&crate::ArtistInput {
            object_id: "artist".into(),
            name: "Artist".into(),
            sort_text: Some("artist".into()),
            artwork_binding: Some(art.to_vec()),
            ..Default::default()
        })
        .await
        .unwrap();
        scan.write_track(&crate::TrackInput {
            object_id: "track".into(),
            album_object_id: Some("album".into()),
            title: "Track".into(),
            display_album: "Album".into(),
            display_artist: "Artist".into(),
            sort_text: "track".into(),
            duration_millis: 180000,
            disc_number: 1,
            track_number: 1,
            artwork_binding: Some(art.to_vec()),
            favorite: Some(false),
            loudness_analysis_key: [1; 32],
            ..Default::default()
        })
        .await
        .unwrap();
        scan.write_track_relations(&[("track", "artist")], &[], &[])
            .await
            .unwrap();
        scan.write_album_relations(&[("album", "artist")], &[], &[])
            .await
            .unwrap();
        scan.finish().await.unwrap();
        let uri = crate::source_entity_uri(&crate::SourceId::new("server"), "track", "track");
        source
            .create_playlist(None, "Saved", std::slice::from_ref(&uri))
            .await
            .unwrap();
        source.connect_initialize_profile().await.unwrap();
        while source.connect_seed_page().await.unwrap() {}
        let records = source
            .connect_changes()
            .await
            .unwrap()
            .into_iter()
            .map(|change| change.record)
            .collect::<Vec<_>>();
        assert!(records.iter().all(|record| !matches!(
            record.kind.as_str(),
            "album" | "artist" | "track_artists" | "native_playlist"
        )));
        assert!(destination.connect_apply(&records).await.unwrap());
        let cancellation = ReadCancellation::new();
        assert!(
            !destination
                .cached_source("server", &cancellation)
                .await
                .unwrap()
                .unwrap()
                .catalog_complete
        );
        let track: (String, bool) =
            sqlx::query_as("SELECT title,indexed FROM tracks WHERE media_uri=?1")
                .bind(&uri)
                .fetch_one(&mut *destination.acquire_reader().await.unwrap())
                .await
                .unwrap();
        assert_eq!(track, ("Track".into(), false));
        assert_eq!(
            destination
                .track_artwork_bindings(std::slice::from_ref(&uri), &cancellation)
                .await
                .unwrap(),
            vec![art.to_vec()]
        );
        assert!(!destination.connect_apply(&records).await.unwrap());
        assert!(destination.connect_changes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn accepted_edits_survive_restart_and_incoming_replay_does_not_echo() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.sqlite");
        let database = Database::open(&path).await.unwrap();
        database.connect_capture_enabled(true).await.unwrap();
        for index in 0..200 {
            database
                .set_favorite(
                    &FavoriteTarget::Track(format!("rufin:source/track/source/{index}")),
                    true,
                )
                .await
                .unwrap();
        }
        let page = database.connect_changes().await.unwrap();
        assert_eq!(page.len(), CONNECT_PAGE_SIZE);
        database.close().await.unwrap();
        let database = Database::open(&path).await.unwrap();
        let replay = database.connect_changes().await.unwrap();
        assert_eq!(page[0].record, replay[0].record);
        let destination = Database::open(directory.path().join("destination.sqlite"))
            .await
            .unwrap();
        destination.connect_capture_enabled(true).await.unwrap();
        let records = page.iter().map(|c| c.record.clone()).collect::<Vec<_>>();
        assert!(destination.connect_apply(&records).await.unwrap());
        assert!(!destination.connect_apply(&records).await.unwrap());
        assert!(destination.connect_changes().await.unwrap().is_empty());
        database.connect_acknowledge(&page).await.unwrap();
        assert_eq!(database.connect_changes().await.unwrap().len(), 72);
    }

    #[tokio::test]
    async fn imported_history_never_creates_service_submissions() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("profile.sqlite"))
            .await
            .unwrap();
        {
            let mut writer = database.writer().await.unwrap();
            sqlx::raw_sql("INSERT INTO listens(source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
                VALUES ('removed','https://example.org/retained','Old title','Old artist','Old album',1,'2026-10',1000,900,0),
                       ('removed','https://example.org/retained','Retained song','Retained artist','Retained album',2,'2026-10',2000,1900,0);
                PRAGMA user_version=56;")
                .execute(writer.as_mut().unwrap()).await.unwrap();
        }
        database.close().await.unwrap();
        let database = Database::open(directory.path().join("profile.sqlite"))
            .await
            .unwrap();
        let recovered = database
            .track_row_by_uri(
                "https://example.org/retained",
                &crate::ReadCancellation::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.title, "Retained song");
        assert_eq!(recovered.artist, "Retained artist");
        assert_eq!(recovered.album, "Retained album");
        {
            let mut reader = database.acquire_reader().await.unwrap();
            assert_eq!(
                crate::schema::pragma(&mut reader, "user_version")
                    .await
                    .unwrap(),
                58
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT local_play_count FROM tracks WHERE track_key=?1"
                )
                .bind(recovered.track_key)
                .fetch_one(&mut *reader)
                .await
                .unwrap(),
                2
            );
        }
        let selection = crate::SongQuery::new(crate::SongScope::History {
            source: Some(crate::SourceId::new("removed")),
        })
        .selection();
        assert_eq!(
            database
                .selected_track_keys(&selection, &crate::ReadCancellation::new())
                .await
                .unwrap(),
            vec![recovered.track_key]
        );
        let record = ConnectRecord {
            kind: "listen".into(),
            key: "shared-listen".into(),
            value: Some(serde_json::json!({
                "external_id":"shared-listen","source_id":null,"media_uri":"https://example.org/song.flac","track_title":"Song","artist_name":"Artist","album_title":"Album","started_at":1700000000,"local_period":"2023-11","duration_millis":120000,"listened_millis":60000,"skipped":0
            })),
        };
        database
            .connect_apply(std::slice::from_ref(&record))
            .await
            .unwrap();
        assert!(!database.connect_apply(&[record]).await.unwrap());
        let history = database
            .song_history_rows(
                &crate::SongQuery::new(crate::SongScope::History { source: None }),
                0,
                100,
                &crate::ReadCancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].title, "Retained song");
        assert_eq!(history[1].title, "Song");
        assert_eq!(history[1].artist, "Artist");
        assert_eq!(history[1].album, "Album");
        let mut connection = database.acquire_reader().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn captured_playlist_entries_replace_a_profile_and_keep_duplicate_occurrences() {
        let directory = tempfile::tempdir().unwrap();
        let source = Database::open(directory.path().join("source.sqlite"))
            .await
            .unwrap();
        let destination = Database::open(directory.path().join("destination.sqlite"))
            .await
            .unwrap();
        source.connect_capture_enabled(true).await.unwrap();
        destination.connect_capture_enabled(true).await.unwrap();
        let media = "https://example.org/same.flac".to_owned();
        let (playlist, _) = source
            .create_playlist(None, "Shared", &[media.clone(), media.clone()])
            .await
            .unwrap()
            .unwrap();
        destination
            .create_playlist(None, "Old local playlist", std::slice::from_ref(&media))
            .await
            .unwrap()
            .unwrap();
        let changes = source.connect_changes().await.unwrap();
        let records: Vec<_> = changes.iter().map(|change| change.record.clone()).collect();
        let entries: Vec<_> = records
            .iter()
            .filter(|record| record.kind == "entry")
            .collect();
        assert_eq!(entries.len(), 2);
        assert_ne!(entries[0].key, entries[1].key);
        assert!(
            entries
                .iter()
                .all(|record| record.value.as_ref().unwrap()["playlist"].is_array())
        );
        destination.connect_replace_profile().await.unwrap();
        assert!(destination.connect_apply(&records).await.unwrap());
        assert!(!destination.connect_apply(&records).await.unwrap());
        assert!(destination.connect_changes().await.unwrap().is_empty());
        source.connect_acknowledge(&changes).await.unwrap();
        source
            .add_playlist_media(None, playlist, std::slice::from_ref(&media), false)
            .await
            .unwrap();
        let last = sqlx::query_scalar::<_, crate::PlaylistEntryKey>(
            "SELECT playlist_entry_key FROM main.playlist_entries WHERE playlist_key=?1 ORDER BY position DESC LIMIT 1")
            .bind(playlist).fetch_one(&mut *source.acquire_reader().await.unwrap()).await.unwrap();
        source
            .move_playlist_entry(None, playlist, last, 0)
            .await
            .unwrap();
        let mut edits: Vec<_> = source
            .connect_changes()
            .await
            .unwrap()
            .into_iter()
            .map(|change| change.record)
            .collect();
        edits.sort_by_key(|record| {
            record
                .value
                .as_ref()
                .and_then(|value| value["position"].as_i64())
        });
        destination.connect_apply(&edits).await.unwrap();
        let query = "SELECT entry.object_id,track.media_uri,entry.position FROM playlist_entries entry JOIN tracks track USING(track_key) ORDER BY entry.position";
        let expected: Vec<(String, String, i64)> = sqlx::query_as(query)
            .fetch_all(&mut *source.acquire_reader().await.unwrap())
            .await
            .unwrap();
        let actual: Vec<(String, String, i64)> = sqlx::query_as(query)
            .fetch_all(&mut *destination.acquire_reader().await.unwrap())
            .await
            .unwrap();
        assert_eq!(expected.len(), 3);
        assert_eq!(actual, expected);
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM main.playlists")
            .fetch_all(&mut *destination.acquire_reader().await.unwrap())
            .await
            .unwrap();
        assert_eq!(names, ["Shared"]);
    }

    #[tokio::test]
    async fn playlist_projection_moves_duplicate_tracks_by_occurrence() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("profile.sqlite"))
            .await
            .unwrap();
        let playlist = "[null,\"playlist\"]";
        let make = |id: &str, position: i64| ConnectRecord {
            kind: "entry".into(),
            key: serde_json::json!([null, "playlist", id]).to_string(),
            value: Some(
                serde_json::json!({"playlist":playlist,"object_id":id,"media_uri":"https://example.org/same.flac","position":position,"snapshot_at":1}),
            ),
        };
        database
            .connect_apply(&[make("a", 0), make("b", 1), make("c", 2)])
            .await
            .unwrap();
        database
            .connect_apply(&[make("c", 0), make("a", 1), make("b", 2)])
            .await
            .unwrap();
        let mut connection = database.acquire_reader().await.unwrap();
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT object_id FROM main.playlist_entries ORDER BY position")
                .fetch_all(&mut *connection)
                .await
                .unwrap();
        assert_eq!(ids, ["c", "a", "b"]);
    }
}
