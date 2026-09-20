//! Durable Connect capture and replay at the existing Store writer boundary.
//! Journal writes share the user edit's transaction; replay never enters service outboxes.
use crate::{Database, LibraryError, LibraryResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Connection, Row, SqliteConnection};
use std::collections::BTreeMap;

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
const PROJECTIONS: &[Projection] = &[
    Projection {
        kind: "native_playlist",
        table: "catalog.native_playlists",
        key: "json_array((SELECT object_id FROM catalog.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text artwork_binding writable",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "native_entry",
        table: "catalog.native_playlist_entries",
        key: "json_array((SELECT object_id FROM catalog.sources WHERE source_key=(SELECT source_key FROM catalog.native_playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM catalog.native_playlists WHERE playlist_key=r.playlist_key),r.object_id)",
        fields: "object_id media_uri title artist album album_display_artist snapshot_at duration_millis disc_number track_number year release_date source_format musicbrainz_recording_id musicbrainz_release_track_id position",
        extra: "'playlist',json_array((SELECT object_id FROM catalog.sources WHERE source_key=(SELECT source_key FROM catalog.native_playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM catalog.native_playlists WHERE playlist_key=r.playlist_key))",
    },
    Projection {
        kind: "playlist",
        table: "main.playlists",
        key: "json_array((SELECT object_id FROM main.source_ids WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text position",
        extra: "'source_id',(SELECT object_id FROM main.source_ids WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "entry",
        table: "main.playlist_entries",
        key: "json_array((SELECT object_id FROM main.source_ids WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key),r.object_id)",
        fields: "object_id media_uri title artist album album_display_artist snapshot_at duration_millis disc_number track_number year release_date source_format musicbrainz_recording_id musicbrainz_release_track_id position",
        extra: "'playlist',json_array((SELECT object_id FROM main.source_ids WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key))",
    },
    Projection {
        kind: "smart",
        table: "main.smart_playlists",
        key: "r.object_id",
        fields: "object_id name normalized_name definition_json position",
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
        table: "catalog.tracks",
        key: "r.media_uri",
        fields: "object_id media_uri title normalized_search display_album display_artist sort_text duration_millis disc_number track_number year release_date date_added source_format comment bpm musicbrainz_recording_id musicbrainz_release_track_id source_favorite source_rating first_seen_at artwork_binding",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key),'album_id',(SELECT object_id FROM catalog.albums WHERE album_key=r.album_key),'_local_root',coalesce((SELECT root FROM catalog.local_files WHERE source_key=r.source_key AND path=r.source_path),(SELECT root FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),'relative_path',coalesce((SELECT relative_path FROM catalog.local_files WHERE source_key=r.source_key AND path=r.source_path),(SELECT relative_path FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),'revision',(SELECT revision FROM catalog.local_files WHERE source_key=r.source_key AND path=coalesce(r.source_path,(SELECT path FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)) LIMIT 1)",
    },
    Projection {
        kind: "album",
        table: "catalog.albums",
        key: "r.media_uri",
        fields: "object_id media_uri title normalized_title display_artist sort_text year release_date date_added musicbrainz_release_id musicbrainz_release_group_id is_compilation release_lookup_identity source_favorite source_rating first_seen_at artwork_binding",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "artist",
        table: "catalog.artists",
        key: "r.media_uri",
        fields: "object_id media_uri name normalized_name sort_text musicbrainz_artist_id source_favorite source_rating artwork_binding",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "genre",
        table: "catalog.genres",
        key: "json_array((SELECT object_id FROM catalog.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text artwork_binding",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "mood",
        table: "catalog.moods",
        key: "json_array((SELECT object_id FROM catalog.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key)",
    },
    Projection {
        kind: "folder",
        table: "catalog.folders",
        key: "json_array((SELECT object_id FROM catalog.sources WHERE source_key=r.source_key),r.object_id)",
        fields: "object_id name normalized_name sort_text artwork_binding",
        extra: "'source_id',(SELECT object_id FROM catalog.sources WHERE source_key=r.source_key)",
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

fn link_projection(left: &str, right: &str, table: &str, alias: &str) -> (String, String) {
    let left_table = format!("catalog.{left}s");
    let right_table = format!("catalog.{right}s");
    let source = format!(
        "(SELECT object_id FROM catalog.sources WHERE source_key=(SELECT source_key FROM {left_table} WHERE {left}_key={alias}.{left}_key))"
    );
    let left_id =
        format!("(SELECT object_id FROM {left_table} WHERE {left}_key={alias}.{left}_key)");
    let right_id =
        format!("(SELECT object_id FROM {right_table} WHERE {right}_key={alias}.{right}_key)");
    let _ = table;
    (
        format!("json_array({source},{left_id},{right_id})"),
        format!(
            "json_object('source_id',{source},'left',{left_id},'right',{right_id},'position',{alias}.position)"
        ),
    )
}

fn portable_relative(value: &mut Value) {
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
        .map(|field| if field == "artwork_binding" {
            format!("'{field}',CASE WHEN {alias}.{field} IS NULL THEN NULL ELSE hex({alias}.{field}) END")
        } else { format!("'{field}',{alias}.{field}") })
        .collect::<Vec<_>>();
    if !projection.extra.is_empty() {
        pairs.push(projection.extra.replace("r.", &format!("{alias}.")));
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
        CREATE TABLE IF NOT EXISTS connect_seed(kind TEXT PRIMARY KEY,cursor INTEGER NOT NULL DEFAULT 0,complete INTEGER NOT NULL DEFAULT 0,artwork_only INTEGER NOT NULL DEFAULT 0) STRICT;
        CREATE TABLE IF NOT EXISTS connect_playlist_seed(singleton INTEGER PRIMARY KEY CHECK(singleton=1),playlist INTEGER NOT NULL,position INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO connect_playlist_seed VALUES(1,0,-1);
        CREATE TABLE IF NOT EXISTS connect_native_playlist_seed(singleton INTEGER PRIMARY KEY CHECK(singleton=1),playlist INTEGER NOT NULL,position INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO connect_native_playlist_seed VALUES(1,0,-1);")
        .execute(&mut *connection).await?;
    for projection in PROJECTIONS {
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
            if operation == "DELETE" && projection.kind == "entry" {
                predicate.push_str(
                    " AND EXISTS(SELECT 1 FROM main.playlists WHERE playlist_key=OLD.playlist_key)",
                );
            }
            if operation == "DELETE" && projection.kind == "native_entry" {
                predicate.push_str(" AND EXISTS(SELECT 1 FROM catalog.native_playlists WHERE playlist_key=OLD.playlist_key)");
            } else if operation == "DELETE" && projection.table.starts_with("catalog.") {
                predicate.push_str(
                    " AND EXISTS(SELECT 1 FROM catalog.sources WHERE source_key=OLD.source_key)",
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
        sqlx::query("INSERT OR IGNORE INTO connect_seed(kind) VALUES(?1)")
            .bind(projection.kind)
            .execute(&mut *connection)
            .await?;
    }
    for (left, right, table) in LINKS {
        for (operation, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
            let (key, value) = link_projection(left, right, table, row);
            let value = if operation == "DELETE" {
                "NULL".into()
            } else {
                value
            };
            let timing = if operation == "DELETE" {
                "BEFORE"
            } else {
                "AFTER"
            };
            let present = if operation == "DELETE" {
                format!(
                    " AND EXISTS(SELECT 1 FROM catalog.{left}s WHERE {left}_key=OLD.{left}_key) AND EXISTS(SELECT 1 FROM catalog.{right}s WHERE {right}_key=OLD.{right}_key)"
                )
            } else {
                String::new()
            };
            let statement = format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS connect_{table}_{operation} {timing} {operation} ON catalog.{table} WHEN (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1){present} BEGIN INSERT INTO connect_changes(kind,object_key,payload) VALUES('{table}',{key},{value}) ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END"
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(&mut *connection)
                .await?;
        }
        sqlx::query("INSERT OR IGNORE INTO connect_seed(kind) VALUES(?1)")
            .bind(table)
            .execute(&mut *connection)
            .await?;
    }
    // Capture identities before a source/metadata cascade removes the lookup rows.
    let mut deletions = String::new();
    for projection in PROJECTIONS
        .iter()
        .filter(|p| p.table.starts_with("catalog.") && p.kind != "native_entry")
    {
        deletions.push_str(&format!("INSERT INTO connect_changes(kind,object_key,payload) SELECT '{}',{},NULL FROM {} r WHERE r.source_key=OLD.source_key ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload;",projection.kind,projection.key,projection.table));
    }
    for (left, right, table) in LINKS {
        let (key, _) = link_projection(left, right, table, "r");
        deletions.push_str(&format!("INSERT INTO connect_changes(kind,object_key,payload) SELECT '{table}',{key},NULL FROM catalog.{table} r WHERE r.{left}_key IN (SELECT {left}_key FROM catalog.{left}s WHERE source_key=OLD.source_key) ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload;"));
        for (entity, other) in [(left, right), (right, left)] {
            let statement = format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS connect_{table}_{entity}_cascade BEFORE DELETE ON catalog.{entity}s WHEN (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1) AND EXISTS(SELECT 1 FROM catalog.sources WHERE source_key=OLD.source_key) BEGIN INSERT INTO connect_changes(kind,object_key,payload) SELECT '{table}',{key},NULL FROM catalog.{table} r WHERE r.{entity}_key=OLD.{entity}_key AND EXISTS(SELECT 1 FROM catalog.{other}s WHERE {other}_key=r.{other}_key) ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END"
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(&mut *connection)
                .await?;
        }
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TEMP TRIGGER IF NOT EXISTS connect_source_cascade BEFORE DELETE ON catalog.sources WHEN (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1) BEGIN {deletions} END"))).execute(&mut *connection).await?;
    // Locators arrive after metadata during local ingestion. Share the relative reference,
    // never the sender's filesystem root or an absolute access URI.
    let tracks = PROJECTIONS
        .iter()
        .find(|p| p.kind == "track")
        .expect("track projection");
    for operation in ["INSERT", "UPDATE"] {
        let statement = format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS connect_locator_{operation} AFTER {operation} ON main.local_locators WHEN NEW.origin IN ('local','import') AND (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1) BEGIN INSERT INTO connect_changes(kind,object_key,payload) SELECT 'track',r.media_uri,{} FROM catalog.tracks r WHERE r.media_uri=NEW.media_uri ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END",
            object(tracks, "r")
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await?;
        let statement = format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS connect_local_file_{operation} AFTER {operation} ON catalog.local_files WHEN (SELECT enabled AND NOT applying FROM main.connect_capture WHERE singleton=1) BEGIN INSERT INTO connect_changes(kind,object_key,payload) SELECT 'track',r.media_uri,{} FROM catalog.tracks r WHERE r.source_key=NEW.source_key AND r.source_path=NEW.path ON CONFLICT(kind,object_key) DO UPDATE SET sequence=excluded.sequence,payload=excluded.payload; END",
            object(tracks, "r")
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&mut *connection)
            .await?;
    }
    let mut transaction = connection.begin().await?;
    let has_artwork_seed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('connect_seed') WHERE name='artwork_only')",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if !has_artwork_seed {
        sqlx::raw_sql("ALTER TABLE connect_seed ADD COLUMN artwork_only INTEGER NOT NULL DEFAULT 0;
            UPDATE connect_seed SET artwork_only=CASE WHEN complete=1 THEN 1 ELSE 0 END,cursor=0,complete=0 WHERE kind IN ('track','album','artist','genre','folder');")
            .execute(&mut *transaction).await?;
    }
    // Older Connect profiles omitted references held by the normal local scan index.
    sqlx::query("INSERT OR IGNORE INTO connect_seed(kind,cursor,complete,artwork_only) VALUES('local_reference',0,0,0)")
        .execute(&mut *transaction).await?;
    sqlx::query("UPDATE catalog.sources SET catalog_revision=1 WHERE catalog_revision=0 AND (EXISTS(SELECT 1 FROM catalog.tracks WHERE source_key=sources.source_key) OR EXISTS(SELECT 1 FROM catalog.albums WHERE source_key=sources.source_key) OR EXISTS(SELECT 1 FROM catalog.artists WHERE source_key=sources.source_key) OR EXISTS(SELECT 1 FROM catalog.genres WHERE source_key=sources.source_key) OR EXISTS(SELECT 1 FROM catalog.moods WHERE source_key=sources.source_key) OR EXISTS(SELECT 1 FROM catalog.folders WHERE source_key=sources.source_key))")
        .execute(&mut *transaction).await?;
    transaction.commit().await?;
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
        sqlx::raw_sql("DELETE FROM connect_changes; UPDATE connect_seed SET cursor=0,complete=0,artwork_only=0; UPDATE connect_playlist_seed SET playlist=0,position=-1; UPDATE connect_native_playlist_seed SET playlist=0,position=-1; UPDATE connect_capture SET enabled=1,applying=0;").execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn connect_catalog_sources(&self) -> LibraryResult<Vec<crate::SourceId>> {
        let mut connection = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar::<_, String>("SELECT object_id FROM catalog.sources")
                .fetch_all(&mut *connection)
                .await?
                .into_iter()
                .map(crate::SourceId::new)
                .collect(),
        )
    }

    pub async fn connect_remove_source(&self, source: &crate::SourceId) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::query("UPDATE connect_capture SET applying=1")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM user_media_state WHERE media_uri IN (SELECT media_uri FROM catalog.tracks WHERE source_key=(SELECT source_key FROM catalog.sources WHERE object_id=?1))").bind(source.as_str()).execute(&mut *transaction).await?;
        sqlx::query("DELETE FROM connect_collection WHERE json_extract(payload,'$.source_id')=?1")
            .bind(source.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM connect_roots WHERE source_id=?1")
            .bind(source.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM catalog.sources WHERE object_id=?1")
            .bind(source.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM main.source_ids WHERE object_id=?1")
            .bind(source.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE connect_capture SET applying=0")
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn connect_capture_enabled(&self, enabled: bool) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("UPDATE connect_capture SET enabled=?1 WHERE singleton=1")
            .bind(enabled)
            .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
            .await?;
        Ok(())
    }

    /// Seeds one bounded page. Subsequent calls resume after interruption.
    pub async fn connect_seed_page(&self) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let row = sqlx::query(
            "SELECT kind,cursor,artwork_only FROM connect_seed WHERE complete=0 ORDER BY rowid LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let kind: String = row.get(0);
        let cursor: i64 = row.get(1);
        let artwork_only: bool = row.get(2);
        let statement = if kind == "local_reference" {
            let projection = PROJECTIONS.iter().find(|p| p.kind == "track").unwrap();
            format!(
                "SELECT r.rowid,r.media_uri,{} FROM catalog.tracks r WHERE r.rowid>?1 AND EXISTS(SELECT 1 FROM catalog.local_files f WHERE f.source_key=r.source_key AND f.path=r.source_path) ORDER BY r.rowid LIMIT ?2",
                object(projection, "r")
            )
        } else if let Some(projection) = PROJECTIONS.iter().find(|p| p.kind == kind) {
            if matches!(kind.as_str(), "playlist" | "smart") {
                format!(
                    "SELECT r.position+1,{} AS object_key,{} AS payload FROM {} r WHERE r.position>=?1 ORDER BY r.position LIMIT ?2",
                    projection.key,
                    object(projection, "r"),
                    projection.table
                )
            } else if matches!(kind.as_str(), "entry" | "native_entry") {
                let seed = if kind == "entry" {
                    "connect_playlist_seed"
                } else {
                    "connect_native_playlist_seed"
                };
                format!(
                    "SELECT r.rowid,{} AS object_key,{} AS payload FROM {} r WHERE (r.playlist_key,r.position)>(SELECT playlist,position FROM {seed} WHERE singleton=1) ORDER BY r.playlist_key,r.position LIMIT ?2",
                    projection.key,
                    object(projection, "r"),
                    projection.table
                )
            } else {
                format!(
                    "SELECT r.rowid,{} AS object_key,{} AS payload FROM {} r WHERE r.rowid>?1{} ORDER BY r.rowid LIMIT ?2",
                    projection.key,
                    object(projection, "r"),
                    projection.table,
                    if artwork_only {
                        " AND r.artwork_binding IS NOT NULL"
                    } else {
                        ""
                    },
                )
            }
        } else {
            let (left, right, table) = LINKS
                .iter()
                .find(|(_, _, table)| *table == kind)
                .expect("fixed relation kind");
            let (key, value) = link_projection(left, right, table, "r");
            format!(
                "SELECT r.rowid,{key},{value} FROM catalog.{table} r WHERE r.rowid>?1 ORDER BY r.rowid LIMIT ?2"
            )
        };
        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(cursor)
            .bind(CONNECT_PAGE_SIZE as i64)
            .fetch_all(&mut *transaction)
            .await?;
        let mut last = cursor;
        for row in &rows {
            last = row.get(0);
            sqlx::query(
                "INSERT OR IGNORE INTO connect_changes(kind,object_key,payload) VALUES(?1,?2,?3)",
            )
            .bind(if kind == "local_reference" {
                "track"
            } else {
                &kind
            })
            .bind(row.get::<String, _>(1))
            .bind(row.get::<String, _>(2))
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query("UPDATE connect_seed SET cursor=?2,complete=?3 WHERE kind=?1")
            .bind(&kind)
            .bind(last)
            .bind(rows.len() < CONNECT_PAGE_SIZE)
            .execute(&mut *transaction)
            .await?;
        if kind == "entry" && !rows.is_empty() {
            sqlx::query("UPDATE connect_playlist_seed SET (playlist,position)=(SELECT playlist_key,position FROM main.playlist_entries WHERE rowid=?1) WHERE singleton=1").bind(last).execute(&mut *transaction).await?;
        } else if kind == "native_entry" && !rows.is_empty() {
            sqlx::query("UPDATE connect_native_playlist_seed SET (playlist,position)=(SELECT playlist_key,position FROM catalog.native_playlist_entries WHERE rowid=?1) WHERE singleton=1").bind(last).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn connect_changes(&self) -> LibraryResult<Vec<ConnectChange>> {
        let mut connection = self.acquire_reader().await?;
        let rows=sqlx::query("SELECT sequence,kind,object_key,payload FROM connect_changes ORDER BY sequence LIMIT ?1").bind(CONNECT_PAGE_SIZE as i64).fetch_all(&mut *connection).await?;
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
        for change in changes {
            if change.record.kind == "track" {
                if let Some(value) = &change.record.value {
                    remember_root(&mut transaction, value).await?;
                }
            }
            sqlx::query("DELETE FROM connect_changes WHERE sequence=?1")
                .bind(change.sequence)
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

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
        let mut sources = BTreeMap::new();
        for record in records {
            changed |= apply_record(&mut transaction, record, &mut sources).await?;
        }
        for (source, artwork_changed) in sources {
            sqlx::query("UPDATE catalog.sources SET catalog_revision=catalog_revision+1,artwork_digest=CASE WHEN ?2 THEN randomblob(32) ELSE artwork_digest END WHERE source_key=?1")
                .bind(source).bind(artwork_changed).execute(&mut *transaction).await?;
        }
        sqlx::query("UPDATE connect_capture SET applying=0 WHERE singleton=1")
            .execute(&mut *transaction)
            .await?;
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
        sqlx::raw_sql("UPDATE connect_capture SET applying=1; DELETE FROM main.playlists; DELETE FROM smart_playlists; DELETE FROM user_media_state; DELETE FROM favorite_outbox; DELETE FROM listens; DELETE FROM legacy_activity; DELETE FROM local_locators; DELETE FROM connect_media_files; DELETE FROM catalog.sources; DELETE FROM main.source_ids; DELETE FROM queue_occurrences; DELETE FROM queue_state; DELETE FROM queue_order; DELETE FROM queue_saved; DELETE FROM queue_transfer_pages; DELETE FROM connect_collection; DELETE FROM connect_roots; DELETE FROM connect_changes; UPDATE connect_seed SET complete=1; UPDATE connect_capture SET applying=0;").execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn connect_track_reference(&self, media_uri: &str) -> LibraryResult<Option<Value>> {
        let mut connection = self.acquire_reader().await?;
        let mut payload: Option<String> =
            sqlx::query_scalar("SELECT payload FROM connect_collection WHERE media_uri=?1")
                .bind(media_uri)
                .fetch_optional(&mut *connection)
                .await?;
        if payload.is_none() {
            let projection = PROJECTIONS
                .iter()
                .find(|p| p.kind == "track")
                .expect("track projection");
            payload = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT {} FROM catalog.tracks r WHERE media_uri=?1",
                object(projection, "r")
            )))
            .bind(media_uri)
            .fetch_optional(&mut *connection)
            .await?;
        }
        let Some(payload) = payload else {
            return Ok(None);
        };
        let mut value: Value = serde_json::from_str(&payload)?;
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
    ) -> LibraryResult<Option<std::path::PathBuf>> {
        let mut connection = self.acquire_reader().await?;
        let paths:Vec<String>=sqlx::query_scalar("SELECT path FROM main.local_locators WHERE media_uri=?1 ORDER BY CASE origin WHEN 'local' THEN 0 WHEN 'import' THEN 1 WHEN 'mapping' THEN 2 ELSE 3 END LIMIT 4").bind(media_uri).fetch_all(&mut *connection).await?;
        Ok(paths
            .into_iter()
            .map(std::path::PathBuf::from)
            .find(|path| path.is_file()))
    }

    /// Register a completed transferred file or a trusted corresponding-folder file
    /// through the existing local-access owner. Mapping never enrolls a scan root.
    pub async fn connect_set_local_file(
        &self,
        media_uri: &str,
        path: &std::path::Path,
        managed: bool,
    ) -> LibraryResult<()> {
        let reference = self
            .connect_track_reference(media_uri)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest("Connect track is not in the collection".into())
            })?;
        let metadata = std::fs::metadata(path)?;
        let source = match reference["source_id"].as_str() {
            Some(id) => self.source_identity_key(&crate::SourceId::new(id)).await?,
            None => None,
        };
        let access_uri = url::Url::from_file_path(path)
            .map_err(|()| {
                LibraryError::InvalidRequest("Connect media path must be absolute".into())
            })?
            .to_string();
        self.upsert_local_access(
            source,
            &crate::LocalAccessWrite {
                media_uri: media_uri.to_owned(),
                origin: if managed {
                    crate::LocalAccessOrigin::Download
                } else {
                    crate::LocalAccessOrigin::Mapping
                },
                path: path.to_string_lossy().into_owned(),
                root: path.parent().unwrap_or(path).to_string_lossy().into_owned(),
                relative_path: reference["relative_path"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                mtime_ns: metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
                    .unwrap_or(0),
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

async fn source_key(connection: &mut SqliteConnection, source: &str) -> LibraryResult<i64> {
    sqlx::query("INSERT OR IGNORE INTO main.source_ids(object_id) VALUES(?1)")
        .bind(source)
        .execute(&mut *connection)
        .await?;
    Ok(
        sqlx::query_scalar("SELECT source_key FROM main.source_ids WHERE object_id=?1")
            .bind(source)
            .fetch_one(connection)
            .await?,
    )
}

async fn playlist_key(connection: &mut SqliteConnection, identity: &str) -> LibraryResult<i64> {
    let identity: (Option<String>, String) = serde_json::from_str(identity)?;
    let source = match identity.0 {
        Some(source) => Some(source_key(connection, &source).await?),
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

async fn remember_root(connection: &mut SqliteConnection, value: &Value) -> LibraryResult<()> {
    if let (Some(source), Some(root)) = (value["source_id"].as_str(), value["root_id"].as_str()) {
        sqlx::query("INSERT INTO connect_roots(source_id,id,label) VALUES(?1,?2,?3) ON CONFLICT(source_id,id) DO UPDATE SET label=excluded.label").bind(source).bind(root).bind(value["root_label"].as_str().unwrap_or("")).execute(connection).await?;
    }
    Ok(())
}

async fn apply_record(
    connection: &mut SqliteConnection,
    record: &ConnectRecord,
    sources: &mut BTreeMap<i64, bool>,
) -> LibraryResult<bool> {
    if record.kind == "root" {
        if let Some(value) = &record.value {
            remember_root(connection, value).await?;
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
                playlist_key(connection, &record.key).await?,
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
    if let Some((left, right, table)) = LINKS.iter().find(|(_, _, table)| *table == record.kind) {
        let (key, _) = link_projection(left, right, table, "r");
        let Some(value) = &record.value else {
            let source: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT source_key FROM catalog.{left}s WHERE {left}_key=(SELECT {left}_key FROM catalog.{table} r WHERE {key}=?1)"
            )))
            .bind(&record.key)
            .fetch_optional(&mut *connection).await?;
            let Some(source) = source else {
                return Ok(false);
            };
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM catalog.{table} AS r WHERE {key}=?1"
            )))
            .bind(&record.key)
            .execute(connection)
            .await?;
            sources.entry(source).or_insert(false);
            return Ok(true);
        };
        let source = value["source_id"].as_str();
        let left_key:Option<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {left}_key FROM catalog.{left}s WHERE source_key=(SELECT source_key FROM catalog.sources WHERE object_id=?1) AND object_id=?2"))).bind(source).bind(value["left"].as_str()).fetch_optional(&mut *connection).await?;
        let right_key:Option<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {right}_key FROM catalog.{right}s WHERE source_key=(SELECT source_key FROM catalog.sources WHERE object_id=?1) AND object_id=?2"))).bind(source).bind(value["right"].as_str()).fetch_optional(&mut *connection).await?;
        let (Some(left_key), Some(right_key)) = (left_key, right_key) else {
            return Err(LibraryError::ConnectPending);
        };
        let position = value["position"].as_i64().unwrap_or(0);
        let old: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT position FROM catalog.{table} WHERE {left}_key=?1 AND {right}_key=?2"
        )))
        .bind(left_key)
        .bind(right_key)
        .fetch_optional(&mut *connection)
        .await?;
        if old == Some(position) {
            return Ok(false);
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR REPLACE INTO catalog.{table}({left}_key,{right}_key,position) VALUES(?1,?2,?3)"))).bind(left_key).bind(right_key).bind(position).execute(&mut *connection).await?;
        let source: i64 =
            sqlx::query_scalar("SELECT source_key FROM catalog.sources WHERE object_id=?1")
                .bind(source)
                .fetch_one(connection)
                .await?;
        sources.entry(source).or_insert(false);
        return Ok(true);
    }
    let Some(projection) = PROJECTIONS.iter().find(|p| p.kind == record.kind) else {
        return Ok(false);
    };
    let identity = match record.kind.as_str() {
        "native_playlist" => "r.source_key=(SELECT source_key FROM catalog.sources WHERE object_id=json_extract(?1,'$[0]')) AND r.object_id=json_extract(?1,'$[1]')".to_owned(),
        "native_entry" => "r.playlist_key=(SELECT playlist_key FROM catalog.native_playlists WHERE source_key=(SELECT source_key FROM catalog.sources WHERE object_id=json_extract(?1,'$[0]')) AND object_id=json_extract(?1,'$[1]')) AND r.object_id=json_extract(?1,'$[2]')".to_owned(),
        _ => format!("{}=?1", projection.key),
    };
    let Some(value) = &record.value else {
        if record.kind == "track" {
            sqlx::query("DELETE FROM connect_collection WHERE media_uri=?1")
                .bind(&record.key)
                .execute(&mut *connection)
                .await?;
        }
        let statement = format!("DELETE FROM {} AS r WHERE {identity}", projection.table);
        if record.kind == "native_entry" {
            let source: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT p.source_key FROM catalog.native_playlist_entries r JOIN catalog.native_playlists p USING(playlist_key) WHERE {identity}"
            )))
            .bind(&record.key).fetch_optional(&mut *connection).await?;
            let deleted = sqlx::query(sqlx::AssertSqlSafe(statement))
                .bind(&record.key)
                .execute(&mut *connection)
                .await?
                .rows_affected()
                > 0;
            if let Some(source) = source {
                sources.entry(source).or_insert(false);
            }
            return Ok(deleted);
        }
        if projection.table.starts_with("catalog.") {
            let artwork = if projection.fields.contains("artwork_binding") {
                "artwork_binding IS NOT NULL"
            } else {
                "0"
            };
            let row = sqlx::query(sqlx::AssertSqlSafe(format!(
                "{statement} RETURNING source_key,{artwork}"
            )))
            .bind(&record.key)
            .fetch_optional(connection)
            .await?;
            if let Some(row) = row {
                let artwork: bool = row.get(1);
                sources
                    .entry(row.get(0))
                    .and_modify(|changed| *changed |= artwork)
                    .or_insert(artwork);
                return Ok(true);
            }
            return Ok(false);
        }
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
    if record.kind == "track" {
        if let Some(old) = &mut old {
            portable_relative(old);
            if old.get("root_id").is_none() {
                let saved: Option<String> =
                    sqlx::query_scalar("SELECT payload FROM connect_collection WHERE media_uri=?1")
                        .bind(&record.key)
                        .fetch_optional(&mut *connection)
                        .await?;
                if let Some(saved) = saved {
                    let saved: Value = serde_json::from_str(&saved)?;
                    for field in ["root_id", "root_label", "relative_path", "revision"] {
                        if let Some(value) = saved.get(field) {
                            old[field] = value.clone();
                        }
                    }
                }
            }
        }
    }
    if value.get("artwork_binding").is_none() {
        if let Some(Value::Object(old)) = &mut old {
            old.remove("artwork_binding");
        }
    }
    if old.as_ref().and_then(Value::as_object).is_some_and(|old| {
        old.iter().all(|(field, old)| {
            let incoming = value.get(field).unwrap_or(&Value::Null);
            match (old, incoming) {
                (Value::Number(old), Value::Bool(incoming)) => {
                    old.as_i64() == Some(i64::from(*incoming))
                }
                _ => incoming == old,
            }
        })
    }) {
        return Ok(false);
    }
    match record.kind.as_str() {
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
            sqlx::query("UPDATE main.playlists SET name=json_extract(?2,'$.name'),normalized_name=json_extract(?2,'$.normalized_name'),sort_text=json_extract(?2,'$.sort_text') WHERE playlist_key=?1").bind(key).bind(payload).execute(connection).await?;
        }
        "entry" | "native_entry" => {
            let (source, playlist, _): (Option<String>, String, String) =
                serde_json::from_str(&record.key)?;
            let table = projection.table;
            let key = if record.kind == "native_entry" {
                let row = sqlx::query("SELECT playlist_key,source_key FROM catalog.native_playlists WHERE source_key=(SELECT source_key FROM catalog.sources WHERE object_id=?1) AND object_id=?2")
                    .bind(source).bind(playlist).fetch_optional(&mut *connection).await?;
                let Some(row) = row else {
                    return Err(LibraryError::ConnectPending);
                };
                sources.entry(row.get(1)).or_insert(false);
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
            let columns = projection
                .fields
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(",");
            let values = projection
                .fields
                .split_whitespace()
                .map(|f| {
                    if f == "position" {
                        "?3".into()
                    } else {
                        format!("json_extract(?2,'$.{f}')")
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO {table}(playlist_key,{columns}) VALUES(?1,{values})"
            )))
            .bind(key)
            .bind(payload)
            .bind(desired)
            .execute(connection)
            .await?;
        }
        "smart" => {
            sqlx::query("INSERT INTO smart_playlists(object_id,name,normalized_name,definition_json,position) VALUES(?1,json_extract(?2,'$.name'),json_extract(?2,'$.normalized_name'),json_extract(?2,'$.definition_json'),(SELECT coalesce(max(position),-1)+1 FROM smart_playlists)) ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,definition_json=excluded.definition_json").bind(&record.key).bind(payload).execute(connection).await?;
        }
        "track" | "album" | "artist" | "genre" | "mood" | "folder" | "native_playlist" => {
            let source = value["source_id"].as_str().ok_or_else(|| {
                LibraryError::InvalidRequest("Connect track source is missing".into())
            })?;
            source_key(connection, source).await?;
            sqlx::query("INSERT OR IGNORE INTO catalog.sources(object_id,display_name,normalized_name,artwork_digest) VALUES(?1,?1,?1,zeroblob(32))").bind(source).execute(&mut *connection).await?;
            let key: i64 =
                sqlx::query_scalar("SELECT source_key FROM catalog.sources WHERE object_id=?1")
                    .bind(source)
                    .fetch_one(&mut *connection)
                    .await?;
            let artwork_changed = value.get("artwork_binding").is_some()
                && old.as_ref().and_then(|old| old.get("artwork_binding"))
                    != value.get("artwork_binding");
            sources
                .entry(key)
                .and_modify(|changed| *changed |= artwork_changed)
                .or_insert(artwork_changed);
            let columns = projection
                .fields
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(",");
            let values = projection
                .fields
                .split_whitespace()
                .map(|f| {
                    if f == "artwork_binding" {
                        "unhex(json_extract(?2,'$.artwork_binding'))".into()
                    } else {
                        format!("json_extract(?2,'$.{f}')")
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            let updates = projection
                .fields
                .split_whitespace()
                .filter(|f| *f != "media_uri")
                .map(|f| if f == "artwork_binding" { "artwork_binding=CASE WHEN json_type(?2,'$.artwork_binding') IS NULL THEN artwork_binding ELSE excluded.artwork_binding END".into() } else { format!("{f}=excluded.{f}") })
                .collect::<Vec<_>>()
                .join(",");
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO {}(source_key,{columns}) VALUES(?1,{values}) ON CONFLICT(source_key,object_id) DO UPDATE SET {updates}",projection.table))).bind(key).bind(&payload).execute(&mut *connection).await?;
            if record.kind == "track" {
                remember_root(connection, value).await?;
                sqlx::query("UPDATE catalog.tracks SET album_key=(SELECT album_key FROM catalog.albums WHERE source_key=?1 AND object_id=json_extract(?2,'$.album_id')) WHERE media_uri=?3").bind(key).bind(&payload).bind(&record.key).execute(&mut *connection).await?;
                sqlx::query("INSERT INTO connect_collection(media_uri,payload) VALUES(?1,?2) ON CONFLICT(media_uri) DO UPDATE SET payload=excluded.payload").bind(&record.key).bind(payload).execute(connection).await?;
            } else if record.kind == "album" {
                sqlx::query("UPDATE catalog.tracks SET album_key=(SELECT album_key FROM catalog.albums WHERE media_uri=?1) WHERE source_key=?2 AND media_uri IN (SELECT media_uri FROM connect_collection WHERE json_extract(payload,'$.album_id')=json_extract(?3,'$.object_id') AND json_extract(payload,'$.source_id')=json_extract(?3,'$.source_id'))").bind(&record.key).bind(key).bind(payload).execute(connection).await?;
            }
        }
        _ => unreachable!(),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AlbumSort, ArtistSort, FavoriteTarget, ReadCancellation, Scan, TrackSort};

    #[tokio::test]
    async fn scanned_catalog_replay_publishes_pages_and_artwork_without_noop_revisions() {
        let directory = tempfile::tempdir().unwrap();
        let source = Database::open(directory.path().join("source.sqlite"))
            .await
            .unwrap();
        let destination_path = directory.path().join("destination.sqlite");
        let destination = Database::open(&destination_path).await.unwrap();
        // Durable playlist sources and catalog sources have independent numeric keys.
        source_key(
            destination.writer().await.unwrap().as_mut().unwrap(),
            "other",
        )
        .await
        .unwrap();
        let art = b"{\"source_id\":\"server\",\"image\":\"cover\"}";
        let mut scan = Scan::begin(&source, "server", "Server", "server", None)
            .await
            .unwrap();
        scan.write_album(
            "album",
            "Album",
            "album",
            "Artist",
            "album",
            None,
            None,
            None,
            None,
            None,
            None,
            Some(art),
            false,
            None,
            None,
        )
        .await
        .unwrap();
        scan.write_artist(
            "artist",
            "Artist",
            "artist",
            Some("artist"),
            None,
            Some(art),
            None,
            None,
        )
        .await
        .unwrap();
        scan.write_track(
            "track",
            Some("album"),
            "Track",
            "track",
            "Album",
            "Artist",
            "track",
            180000,
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(art),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            [1; 32],
        )
        .await
        .unwrap();
        scan.write_track_relations(&[("track", "artist")], &[], &[])
            .await
            .unwrap();
        scan.write_album_relations(&[("album", "artist")], &[], &[])
            .await
            .unwrap();
        scan.finish().await.unwrap();
        source.connect_initialize_profile().await.unwrap();
        while source.connect_seed_page().await.unwrap() {}
        let changes = source.connect_changes().await.unwrap();
        let records: Vec<_> = changes.iter().map(|change| change.record.clone()).collect();
        assert!(destination.connect_apply(&records).await.unwrap());
        let cancellation = ReadCancellation::new();
        let cached = destination
            .cached_source("server", &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.catalog_revision, 1);
        assert_ne!(cached.artwork_digest, vec![0; 32]);
        let keys: (i64, i64) = sqlx::query_as("SELECT catalog.sources.source_key,main.source_ids.source_key FROM catalog.sources JOIN main.source_ids USING(object_id) WHERE object_id='server'")
            .fetch_one(&mut *destination.acquire_reader().await.unwrap()).await.unwrap();
        assert_ne!(keys.0, keys.1);
        assert_eq!(
            destination
                .track_page(
                    cached.source,
                    None,
                    false,
                    "",
                    TrackSort::Title,
                    false,
                    0,
                    20,
                    &cancellation
                )
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            destination
                .album_page(
                    cached.source,
                    None,
                    false,
                    "",
                    AlbumSort::Title,
                    false,
                    0,
                    20,
                    &cancellation
                )
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            destination
                .artist_page(
                    cached.source,
                    None,
                    false,
                    false,
                    "",
                    ArtistSort::Title,
                    false,
                    0,
                    20,
                    &cancellation
                )
                .await
                .unwrap()
                .len(),
            1
        );
        let bindings: Vec<Vec<u8>> = sqlx::query_scalar("SELECT artwork_binding FROM catalog.tracks UNION ALL SELECT artwork_binding FROM catalog.albums UNION ALL SELECT artwork_binding FROM catalog.artists")
            .fetch_all(&mut *destination.acquire_reader().await.unwrap()).await.unwrap();
        assert_eq!(bindings, vec![art.to_vec(); 3]);
        let track = records
            .iter()
            .find(|record| record.kind == "track")
            .unwrap();
        assert_eq!(
            destination
                .track_artwork_bindings(std::slice::from_ref(&track.key), &cancellation)
                .await
                .unwrap(),
            vec![art.to_vec()]
        );
        assert!(!destination.connect_apply(&records).await.unwrap());
        let unchanged = destination
            .cached_source("server", &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.catalog_revision, cached.catalog_revision);
        assert_eq!(unchanged.artwork_digest, cached.artwork_digest);
        let mut older = records.clone();
        for record in &mut older {
            if let Some(Value::Object(value)) = &mut record.value {
                value.remove("artwork_binding");
            }
        }
        assert!(!destination.connect_apply(&older).await.unwrap());
        let mut future = records.clone();
        for record in &mut future {
            if let Some(value) = &mut record.value {
                value["future_field"] = Value::from(42);
            }
        }
        assert!(!destination.connect_apply(&future).await.unwrap());
        let mut relation = records
            .iter()
            .find(|record| record.kind == "track_artists")
            .unwrap()
            .clone();
        relation.value.as_mut().unwrap()["position"] = Value::from(7);
        assert!(
            destination
                .connect_apply(std::slice::from_ref(&relation))
                .await
                .unwrap()
        );
        let related = destination
            .cached_source("server", &cancellation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(related.catalog_revision, 2);
        assert_eq!(related.artwork_digest, cached.artwork_digest);
        relation.value = None;
        assert!(destination.connect_apply(&[relation]).await.unwrap());
        assert_eq!(
            destination
                .cached_source("server", &cancellation)
                .await
                .unwrap()
                .unwrap()
                .catalog_revision,
            3
        );
        // Existing received catalogs with zero revisions become visible on reopen.
        sqlx::query("UPDATE catalog.sources SET catalog_revision=0")
            .execute(destination.writer().await.unwrap().as_mut().unwrap())
            .await
            .unwrap();
        destination.close().await.unwrap();
        let reopened = Database::open(&destination_path).await.unwrap();
        assert_eq!(
            reopened
                .cached_source("server", &cancellation)
                .await
                .unwrap()
                .unwrap()
                .catalog_revision,
            1
        );
    }

    #[tokio::test]
    async fn old_profiles_backfill_artwork_in_restartable_pages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.sqlite");
        let database = Database::open(&path).await.unwrap();
        let art = b"existing artwork binding";
        let mut scan = Scan::begin(&database, "server", "Server", "server", None)
            .await
            .unwrap();
        for index in 0..258 {
            scan.write_artist(
                &format!("artist-{index}"),
                "Artist",
                "artist",
                Some("artist"),
                None,
                (index != 257).then_some(art.as_slice()),
                None,
                None,
            )
            .await
            .unwrap();
        }
        scan.write_folder("with-art", "Folder", "folder", "folder", Some(art))
            .await
            .unwrap();
        scan.write_folder("without-art", "Folder", "folder", "folder", None)
            .await
            .unwrap();
        scan.finish().await.unwrap();
        database.close().await.unwrap();
        // This is the persisted schema before artwork capture, with folder seeding unfinished.
        let mut raw = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(&path),
        )
        .await
        .unwrap();
        sqlx::raw_sql("UPDATE connect_seed SET complete=(kind!='folder'); ALTER TABLE connect_seed DROP COLUMN artwork_only;")
            .execute(&mut raw).await.unwrap();
        raw.close().await.unwrap();
        let database = Database::open(&path).await.unwrap();
        while database.connect_changes().await.unwrap().is_empty() {
            assert!(database.connect_seed_page().await.unwrap());
        }
        let first = database.connect_changes().await.unwrap();
        assert_eq!(first.len(), CONNECT_PAGE_SIZE);
        assert!(first.iter().all(|change| change.record.kind == "artist"
            && change.record.value.as_ref().unwrap()["artwork_binding"].is_string()));
        database.connect_acknowledge(&first).await.unwrap();
        database.close().await.unwrap();
        let database = Database::open(&path).await.unwrap();
        let mut count = first.len();
        while database.connect_seed_page().await.unwrap() {
            let page = database.connect_changes().await.unwrap();
            assert!(page.len() <= CONNECT_PAGE_SIZE);
            assert!(page.iter().all(|change| {
                !first
                    .iter()
                    .any(|seen| seen.record.key == change.record.key)
            }));
            count += page.len();
            database.connect_acknowledge(&page).await.unwrap();
        }
        assert_eq!(count, 259);
        database.close().await.unwrap();
        let database = Database::open(&path).await.unwrap();
        assert!(!database.connect_seed_page().await.unwrap());
        assert!(database.connect_changes().await.unwrap().is_empty());
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
        database.connect_apply(&[record]).await.unwrap();
        let mut connection = database.acquire_reader().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens")
                .fetch_one(&mut *connection)
                .await
                .unwrap(),
            1
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
        let query =
            "SELECT object_id,media_uri,position FROM main.playlist_entries ORDER BY position";
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
