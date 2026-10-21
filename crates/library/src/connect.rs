//! Shared values at the catalog writer boundary. Changes commit with their
//! versions; received values never enter service outboxes.
use crate::{Database, LibraryError, LibraryResult};
pub(crate) mod catalog;
mod database;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Connection, QueryBuilder, Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

pub const CONNECT_PAGE_SIZE: usize = 128;

pub fn connect_current_kind(kind: &str) -> bool {
    matches!(kind, "catalog:source" | "catalog:file" | "catalog:artwork")
        || kind.starts_with("analysis:")
        || matches!(
            kind,
            "listen" | "legacy_activity" | "state" | "smart" | "reference"
        )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConnectRecord {
    pub kind: String,
    pub key: String,
    pub value: Option<Value>,
}

/// The logical order of one saved value, independent of wall-clock time.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RecordStamp {
    pub lamport: u64,
    pub actor: u64,
}

impl RecordStamp {
    pub fn next(self, actor: u64) -> Self {
        Self {
            lamport: self.lamport.saturating_add(1),
            actor,
        }
    }

    pub fn encode(self) -> Vec<u8> {
        [self.lamport.to_be_bytes(), self.actor.to_be_bytes()].concat()
    }

    pub fn decode(bytes: &[u8]) -> LibraryResult<Self> {
        let bytes: &[u8; 16] = bytes
            .try_into()
            .map_err(|_| LibraryError::InvalidStore("invalid shared record version".into()))?;
        Ok(Self {
            lamport: u64::from_be_bytes(bytes[..8].try_into().unwrap()),
            actor: u64::from_be_bytes(bytes[8..].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StampedRecord {
    pub record: ConnectRecord,
    pub stamp: RecordStamp,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
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

fn projection_filter(projection: &Projection, alias: &str) -> String {
    match projection.kind {
        "playlist_order" => format!("{alias}.owner='source'"),
        "playlist" | "playlist_artwork" => format!("{alias}.owner='rufin'"),
        "entry" => format!(
            "EXISTS(SELECT 1 FROM playlists WHERE playlist_key={alias}.playlist_key AND owner='rufin')"
        ),
        "track" => format!(
            "({}) AND NOT EXISTS(SELECT 1 FROM sources source WHERE source.source_key={alias}.source_key AND source.kind IS NOT NULL)",
            needed_media_sql(alias)
        ),
        _ => "1".into(),
    }
}
pub(crate) const NEEDED_TRACK_KEYS: &str = "SELECT entry.track_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.owner='rufin'
    UNION SELECT track_key FROM queue_entries
    UNION SELECT track.track_key FROM local_locators locator JOIN tracks track USING(media_uri) WHERE locator.origin='download'
    UNION SELECT track.track_key FROM (SELECT artwork_binding FROM playlists UNION ALL SELECT artwork_binding FROM smart_playlists) artwork
    CROSS JOIN json_each(artwork.artwork_binding) candidate
    CROSS JOIN json_each(candidate.value,'$.image.automatic.chosen') chosen
    JOIN tracks track ON track.media_uri=chosen.value";

pub(crate) fn needed_media_sql(alias: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE entry.track_key={alias}.track_key AND playlist.owner='rufin') OR EXISTS(SELECT 1 FROM queue_entries WHERE track_key={alias}.track_key) OR EXISTS(SELECT 1 FROM local_locators WHERE media_uri={alias}.media_uri AND origin='download') OR EXISTS(SELECT 1 FROM(SELECT artwork_binding FROM playlists UNION ALL SELECT artwork_binding FROM smart_playlists) artwork CROSS JOIN json_each(artwork.artwork_binding) candidate CROSS JOIN json_each(candidate.value,'$.image.automatic.chosen') chosen WHERE chosen.value={alias}.media_uri)"
    )
}

const PROJECTIONS: &[Projection] = &[
    Projection {
        kind: "playlist_order",
        table: "main.playlists",
        key: "json_array((SELECT object_id FROM sources WHERE source_key=r.source_key),r.object_id)",
        fields: "position artwork_binding",
        extra: "'source_owned',json('true')",
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
        fields: "artwork_bytes artwork_mime artwork_revision artwork_binding",
        extra: "",
    },
    Projection {
        kind: "entry",
        table: "main.playlist_entries",
        key: "json_array((SELECT object_id FROM main.sources WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key),r.object_id)",
        fields: "object_id position",
        extra: "'playlist',json_array((SELECT object_id FROM main.sources WHERE source_key=(SELECT source_key FROM main.playlists WHERE playlist_key=r.playlist_key)),(SELECT object_id FROM main.playlists WHERE playlist_key=r.playlist_key))",
    },
    Projection {
        kind: "smart",
        table: "main.smart_playlists",
        key: "r.object_id",
        fields: "object_id name normalized_name definition_json position artwork_bytes artwork_revision artwork_binding",
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
        table: "main.tracks",
        key: "r.media_uri",
        fields: "object_id media_uri first_seen_at artwork_binding",
        extra: "'source_id',(SELECT object_id FROM main.sources WHERE source_key=r.source_key),'album_id',(SELECT object_id FROM main.albums WHERE album_key=r.album_key),'index_source_path',r.source_path,'index_cue_path',r.cue_path,'root_id',(SELECT root_id FROM local_files file WHERE file.source_key=r.source_key AND file.path=r.source_path LIMIT 1),'_local_root',coalesce((SELECT root FROM main.local_files WHERE source_key=r.source_key AND path=r.source_path),(SELECT root FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),'relative_path',coalesce((SELECT relative_path FROM main.local_files WHERE source_key=r.source_key AND path=r.source_path),(SELECT relative_path FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),'revision',(SELECT revision FROM main.local_files WHERE source_key=r.source_key AND path=coalesce(r.source_path,(SELECT path FROM main.local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)) LIMIT 1)",
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
        .or_else(|| {
            value["index_source_path"]
                .as_str()
                .or_else(|| value["source_path"].as_str())
                .and_then(|path| {
                    crate::document_locator_parts(path)
                        .map(|(id, _)| id)
                        .or_else(|| crate::document_media_id(path))
                })
        })
    {
        value["backing_id"] = crate::document_media_uri(&backing).into();
    }
    if let Some(root) = value
        .as_object_mut()
        .and_then(|v| v.remove("_local_root"))
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        if value["root_id"].is_null() {
            value["root_id"] = blake3::hash(root.as_bytes()).to_hex().to_string().into();
        }
        value["root_label"] = root
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or_default()
            .into();
    }
    if let Some(relative) = value["relative_path"].as_str() {
        value["relative_path"] = relative.replace('\\', "/").into();
    }
}

fn object(projection: &Projection, alias: &str) -> String {
    if projection.kind == "state" {
        return format!(
            "json_set(coalesce(json({alias}.extra),'{{}}'),'$.media_uri',{alias}.media_uri,'$.favorite',json(CASE {alias}.favorite WHEN 1 THEN 'true' WHEN 0 THEN 'false' ELSE 'null' END),'$.rating',{alias}.rating)"
        );
    }
    let mut pairs = projection.fields.split_whitespace().map(|field| {
        if matches!(field,"artwork_binding"|"artwork_bytes") {
            let column=if field=="artwork_binding" && projection.kind=="track" {"native_artwork"} else {field};
            format!("'{field}',CASE WHEN {alias}.{column} IS NULL THEN NULL ELSE hex({alias}.{column}) END")
        } else {format!("'{field}',{alias}.{field}")}
    }).collect::<Vec<_>>();
    if !projection.extra.is_empty() {
        pairs.push(projection.extra.replace("r.", &format!("{alias}.")));
    }
    let base = match projection.kind {
        "track" => {
            pairs.push(format!("'display_artist',json_extract({alias}.metadata,'$.artist'),'display_album',json_extract({alias}.metadata,'$.album'),'source_favorite',(SELECT favorite FROM native_media_state WHERE media_uri={alias}.media_uri),'source_rating',(SELECT rating FROM native_media_state WHERE media_uri={alias}.media_uri),'local_media',json(CASE WHEN EXISTS(SELECT 1 FROM main.local_files f WHERE f.source_key={alias}.source_key AND f.path={alias}.source_path AND f.path LIKE 'rufin-document:%') OR EXISTS(SELECT 1 FROM main.local_locators l WHERE l.media_uri={alias}.media_uri AND l.origin IN ('local','import') AND l.access_uri LIKE 'content://%') THEN 'true' ELSE 'false' END),'cue_start_millis',{alias}.cue_start_millis,'cue_end_millis',{alias}.cue_end_millis"));
            format!("json({alias}.metadata)")
        }
        "entry" => {
            pairs.push(format!(
                "'media_uri',(SELECT media_uri FROM tracks WHERE track_key={alias}.track_key)"
            ));
            "'{}'".into()
        }
        _ => "'{}'".into(),
    };
    format!(
        "json_patch(coalesce({base},'{{}}'),json_object({}))",
        pairs.join(",")
    )
}

pub(crate) async fn set_receiving_on(
    connection: &mut SqliteConnection,
    receiving: bool,
) -> LibraryResult<()> {
    sqlx::query("UPDATE connect_profile SET receiving=?1 WHERE singleton=1")
        .bind(receiving)
        .execute(connection)
        .await?;
    Ok(())
}

pub(crate) async fn mark_current_on(
    connection: &mut SqliteConnection,
    kind: &str,
    key: &str,
    deleted: bool,
) -> LibraryResult<()> {
    mark_current_many_on(connection, kind, &[key], deleted).await
}

pub(crate) async fn mark_current_many_on(
    connection: &mut SqliteConnection,
    kind: &str,
    keys: &[&str],
    deleted: bool,
) -> LibraryResult<()> {
    for keys in keys.chunks(CONNECT_PAGE_SIZE) {
        let profile:Option<(String,i64)>=sqlx::query_as("UPDATE connect_profile SET clock=clock+?1 WHERE singleton=1 AND profile IS NOT NULL AND enabled=1 AND receiving=0 RETURNING actor,clock")
            .bind(keys.len() as i64).fetch_optional(&mut *connection).await?;
        let Some((actor, clock)) = profile else {
            return Ok(());
        };
        let actor = actor.parse().unwrap_or_default();
        let revision: i64 = sqlx::query_scalar(
            "UPDATE file_revision SET revision=revision+?1 WHERE id=1 RETURNING revision",
        )
        .bind(keys.len() as i64)
        .fetch_one(&mut *connection)
        .await?;
        let rows = keys
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let offset = index as i64 + 1 - keys.len() as i64;
                Ok((
                    format!("{kind}:{key}"),
                    *key,
                    serde_json::to_string(&RecordStamp {
                        lamport: (clock + offset) as u64,
                        actor,
                    })?,
                    revision + offset,
                ))
            })
            .collect::<LibraryResult<Vec<_>>>()?;
        let mut insert = QueryBuilder::new(
            "INSERT INTO connect_versions(name,kind,object_key,version,revision,deleted) ",
        );
        insert.push_values(rows, |mut row, (name, key, version, revision)| {
            row.push_bind(name)
                .push_bind(kind)
                .push_bind(key)
                .push_bind(version)
                .push_bind(revision)
                .push_bind(deleted);
        });
        insert.push(" ON CONFLICT(name) DO UPDATE SET version=excluded.version,revision=excluded.revision,deleted=excluded.deleted");
        insert.build().execute(&mut *connection).await?;
    }
    Ok(())
}

pub(crate) async fn mark_saved_references_on(
    connection: &mut SqliteConnection,
    source: i64,
    ids: &[&str],
) -> LibraryResult<()> {
    for page in ids.chunks(CONNECT_PAGE_SIZE) {
        let uris:Vec<String>=sqlx::query_scalar("SELECT track.media_uri FROM json_each(?2) input JOIN tracks track ON track.source_key=?1 AND track.object_id=input.value JOIN connect_versions saved ON saved.kind='reference' AND saved.object_key=track.media_uri AND saved.deleted=0")
            .bind(source).bind(serde_json::to_string(page)?).fetch_all(&mut *connection).await?;
        mark_current_many_on(
            connection,
            "reference",
            &uris.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn initialize(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql("DROP TABLE IF EXISTS file_exchange; DROP TABLE IF EXISTS cache_file_exchange; DROP TABLE IF EXISTS cache_assets;")
        .execute(&mut *connection).await?;
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS connect_profile(singleton INTEGER PRIMARY KEY CHECK(singleton=1),profile TEXT,publisher TEXT,schema_version INTEGER NOT NULL DEFAULT 0,actor TEXT NOT NULL DEFAULT '0',clock INTEGER NOT NULL DEFAULT 0,receiving INTEGER NOT NULL DEFAULT 0,enabled INTEGER NOT NULL DEFAULT 0) STRICT;
        INSERT OR IGNORE INTO connect_profile(singleton) VALUES(1);")
        .execute(&mut *connection).await?;
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS connect_roots(source_id TEXT NOT NULL,id TEXT NOT NULL,label TEXT NOT NULL,root TEXT,PRIMARY KEY(source_id,id)) STRICT;
        CREATE TABLE IF NOT EXISTS connect_seed(kind TEXT PRIMARY KEY,cursor INTEGER NOT NULL DEFAULT 0,complete INTEGER NOT NULL DEFAULT 0) STRICT;
        CREATE TABLE IF NOT EXISTS connect_playlist_seed(singleton INTEGER PRIMARY KEY CHECK(singleton=1),playlist INTEGER NOT NULL,position INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO connect_playlist_seed VALUES(1,0,-1);")
        .execute(&mut *connection).await?;
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS file_revision(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL) STRICT;
        INSERT OR IGNORE INTO file_revision(id,revision) VALUES(1,0);
        CREATE TABLE IF NOT EXISTS connect_versions(name TEXT PRIMARY KEY,kind TEXT NOT NULL,object_key TEXT NOT NULL,version TEXT NOT NULL,revision INTEGER NOT NULL,deleted INTEGER NOT NULL,UNIQUE(kind,object_key)) STRICT;
        CREATE INDEX IF NOT EXISTS connect_versions_revision ON connect_versions(revision,name);
        CREATE TABLE IF NOT EXISTS documents(name TEXT PRIMARY KEY,snapshot BLOB NOT NULL,revision INTEGER NOT NULL DEFAULT 0,version BLOB NOT NULL DEFAULT X'') STRICT;
        CREATE INDEX IF NOT EXISTS documents_kind ON documents(substr(name,1,instr(name,':')-1),name);
        CREATE TRIGGER IF NOT EXISTS file_revision_delete AFTER DELETE ON documents BEGIN UPDATE file_revision SET revision=revision+1; END;
        CREATE TABLE IF NOT EXISTS sync_cursors(peer TEXT NOT NULL,setup INTEGER NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(peer,setup)) STRICT;
        CREATE TABLE IF NOT EXISTS history_peers(peer TEXT PRIMARY KEY) STRICT;
        CREATE TABLE IF NOT EXISTS history_acknowledgements(peer TEXT NOT NULL,name TEXT NOT NULL,version BLOB NOT NULL,PRIMARY KEY(peer,name)) STRICT;
        CREATE TABLE IF NOT EXISTS history_pending(name TEXT PRIMARY KEY) STRICT;
")
        .execute(&mut *connection).await?;
    if !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('connect_roots') WHERE name='root')",
    )
    .fetch_one(&mut *connection)
    .await?
    {
        sqlx::query("ALTER TABLE connect_roots ADD COLUMN root TEXT")
            .execute(&mut *connection)
            .await?;
    }
    migrate_collection_on(connection, "main").await?;
    for kind in [
        "smart_order",
        "playlist_order",
        "playlist",
        "playlist_artwork",
        "entry",
    ] {
        sqlx::query("INSERT OR IGNORE INTO connect_seed(kind) VALUES(?1)")
            .bind(kind)
            .execute(&mut *connection)
            .await?;
    }
    sqlx::query("DELETE FROM connect_seed WHERE kind NOT IN ('smart_order','playlist_order','playlist','playlist_artwork','entry')")
        .execute(&mut *connection).await?;
    Ok(())
}

impl Database {
    pub async fn connect_record_on(
        connection: &mut SqliteConnection,
        kind: &str,
        key: &str,
    ) -> LibraryResult<ConnectRecord> {
        if kind.starts_with("analysis:") {
            let value = match kind {
                "analysis:loudness" => crate::loudness::current_record_on(connection, key).await?,
                "analysis:waveform" => crate::waveform::current_record_on(connection, key).await?,
                _ => None,
            };
            return Ok(ConnectRecord {
                kind: kind.into(),
                key: key.into(),
                value,
            });
        }
        if matches!(kind, "catalog:source" | "catalog:file" | "catalog:artwork") {
            return Ok(ConnectRecord {
                kind: kind.into(),
                key: key.into(),
                value: catalog::current_on(connection, kind, key).await?,
            });
        }
        let payload: Option<String> = if let Some(projection) = PROJECTIONS
            .iter()
            .find(|projection| projection.kind == if kind == "reference" { "track" } else { kind })
        {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT {} FROM {} r WHERE {}=?1 AND ({}) LIMIT 1",
                object(projection, "r"),
                projection.table,
                projection.key,
                if kind == "reference" {
                    "1".into()
                } else {
                    projection_filter(projection, "r")
                }
            )))
            .bind(key)
            .fetch_optional(&mut *connection)
            .await?
        } else {
            None
        };
        let mut value = payload
            .map(|payload| serde_json::from_str::<Value>(&payload))
            .transpose()?;
        if kind == "reference"
            && let Some(value) = &mut value
        {
            portable_relative(value);
        }
        if let Some(value) = &mut value {
            restore_root_label(connection, value, None).await?;
        }
        Ok(ConnectRecord {
            kind: kind.into(),
            key: key.into(),
            value,
        })
    }
    pub async fn connect_initialize_profile(&self) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::raw_sql("DELETE FROM connect_versions; UPDATE connect_seed SET cursor=0,complete=0; UPDATE connect_playlist_seed SET playlist=0,position=-1; UPDATE connect_profile SET enabled=1,receiving=0;").execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn connect_set_active(&self, enabled: bool) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("UPDATE connect_profile SET enabled=?1 WHERE singleton=1")
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
            "SELECT kind,cursor FROM connect_seed WHERE complete=0 ORDER BY kind<>'smart_order',rowid LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let kind: String = row.get(0);
        let cursor: i64 = row.get(1);
        let statement = if kind == "smart_order" {
            "SELECT position+1,object_id,json_object('position',position) FROM smart_playlists WHERE position>=?1 ORDER BY position LIMIT ?2".into()
        } else if let Some(projection) = PROJECTIONS.iter().find(|p| p.kind == kind) {
            if kind == "playlist" {
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
                    "SELECT r.rowid,r.playlist_key,r.position FROM {} r WHERE ({}) AND (r.playlist_key,r.position)>(SELECT playlist,position FROM {seed} WHERE singleton=1) ORDER BY r.playlist_key,r.position LIMIT ?2",
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
                        " AND r.artwork_binding IS NOT NULL"
                    } else {
                        ""
                    },
                )
            }
        } else {
            unreachable!("fixed saved-object seed kind")
        };
        let page_size = if kind == "playlist_artwork" {
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
        if kind == "entry" {
            for group in rows.chunk_by(|a, b| a.get::<i64, _>(1) == b.get::<i64, _>(1)) {
                let records = crate::playlists::playlist_entry_records_on(
                    &mut transaction,
                    group[0].get(1),
                    group[0].get(2),
                    group.last().unwrap().get(2),
                )
                .await?;
                crate::profile::write_local_on(&mut transaction, &records).await?;
            }
        } else {
            let mut records = Vec::with_capacity(rows.len());
            for row in &rows {
                records.push(ConnectRecord {
                    kind: kind.clone(),
                    key: row.get(1),
                    value: Some(serde_json::from_str(row.get::<&str, _>(2))?),
                });
            }
            crate::profile::write_local_on(&mut transaction, &records).await?;
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

    /// Apply received values through the same canonical writers as source observations.
    pub async fn connect_apply(&self, records: &[ConnectRecord]) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let changed = apply_records_on(&mut transaction, records).await?;
        transaction.commit().await?;
        Ok(changed)
    }

    pub async fn connect_track_reference(&self, media_uri: &str) -> LibraryResult<Option<Value>> {
        let mut connection = self.acquire_reader().await?;
        let projection = PROJECTIONS.iter().find(|p| p.kind == "track").unwrap();
        let whole_file = crate::local::whole_file_cue_sql("r");
        let payload: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT json_set({},'$.index_source_path',r.source_path,'$.index_cue_path',r.cue_path,'$.cue_end_millis',CASE WHEN {whole_file} THEN NULL ELSE r.cue_end_millis END) FROM tracks r WHERE media_uri=?1 AND (r.indexed=1 OR (r.track_key IN ({NEEDED_TRACK_KEYS}) AND (r.source_path IS NOT NULL OR EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=r.media_uri AND locator.origin IN ('local','import')))))",
            object(projection, "r")
        )))
        .bind(media_uri).fetch_optional(&mut *connection).await?;
        let Some(payload) = payload else {
            return Ok(None);
        };
        let mut value: Value = serde_json::from_str(&payload)?;
        portable_relative(&mut value);
        restore_root_label(&mut connection, &mut value, None).await?;
        value["root_count"] = match value["source_id"].as_str() {
            Some(source) => read_roots(&mut connection, source).await?.len(),
            None => 0,
        }
        .into();
        Ok(Some(value))
    }

    pub async fn connect_roots(&self, source_id: &str) -> LibraryResult<Vec<ConnectRoot>> {
        let mut connection = self.acquire_reader().await?;
        read_roots(&mut connection, source_id).await
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
}

pub(crate) async fn apply_records_on(
    connection: &mut SqliteConnection,
    records: &[ConnectRecord],
) -> LibraryResult<bool> {
    let (analysis, records): (Vec<_>, Vec<_>) = records
        .iter()
        .cloned()
        .partition(|record| record.kind.starts_with("analysis:"));
    let (current, legacy): (Vec<_>, Vec<_>) = records
        .into_iter()
        .partition(|record| record.kind.starts_with("catalog:"));
    set_receiving_on(connection, true).await?;
    let mut changed = Database::apply_current_catalog_on(connection, &current).await?;
    let mut legacy = legacy;
    legacy.sort_by_key(|record| {
        let parent = match record.kind.as_str() {
            "playlist" | "native_playlist" => 0,
            "album" | "artist" | "genre" | "mood" | "folder" | "root" => 1,
            "track" | "reference" => 2,
            "entry" | "native_entry" => 3,
            "playlist_order" => 5,
            "smart_order" => 6,
            _ => 4,
        };
        let position = matches!(
            record.kind.as_str(),
            "entry" | "native_entry" | "playlist_order" | "smart_order"
        )
        .then(|| {
            record
                .value
                .as_ref()
                .and_then(|value| value["position"].as_i64())
        })
        .flatten();
        (parent, position)
    });
    for record in analysis {
        changed |= match record.kind.as_str() {
            "analysis:loudness" => {
                crate::loudness::apply_record_on(connection, &record.key, record.value.as_ref())
                    .await?
            }
            "analysis:waveform" => {
                crate::waveform::apply_record_on(connection, &record.key, record.value.as_ref())
                    .await?
            }
            _ => false,
        };
    }
    let records = legacy.as_slice();
    let mut remaining = records;
    while let Some(record) = remaining.first() {
        if matches!(record.kind.as_str(), "playlist_order" | "smart_order") {
            let count = remaining
                .iter()
                .take(CONNECT_PAGE_SIZE)
                .take_while(|next| next.kind == record.kind)
                .count();
            changed |= apply_order_page(connection, &remaining[..count]).await?;
            remaining = &remaining[count..];
        } else if matches!(record.kind.as_str(), "entry" | "native_entry") {
            let count = remaining
                .iter()
                .take(CONNECT_PAGE_SIZE)
                .take_while(|record| matches!(record.kind.as_str(), "entry" | "native_entry"))
                .count();
            changed |= apply_entry_page(connection, &remaining[..count]).await?;
            remaining = &remaining[count..];
        } else if record.value.is_some()
            && (matches!(
                record.kind.as_str(),
                "track"
                    | "reference"
                    | "album"
                    | "artist"
                    | "genre"
                    | "mood"
                    | "folder"
                    | "native_playlist"
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
                apply_link_page(connection, &remaining[..count], left, right, table).await?
            } else {
                apply_catalog_page(connection, &remaining[..count]).await?
            };
            remaining = &remaining[count..];
        } else {
            let applied = apply_record(connection, record).await?;
            changed |= applied;
            remaining = &remaining[1..];
        }
    }
    set_receiving_on(connection, false).await?;
    Ok(changed)
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

async fn read_roots(
    connection: &mut SqliteConnection,
    source: &str,
) -> LibraryResult<Vec<ConnectRoot>> {
    let native = native_roots(connection, source).await?;
    roots_with_native(connection, source, &native).await
}

pub(crate) async fn roots_with_native(
    connection: &mut SqliteConnection,
    source: &str,
    native: &[(String, Option<String>)],
) -> LibraryResult<Vec<ConnectRoot>> {
    let mut roots: BTreeMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT id,label FROM connect_roots WHERE source_id=?1",
    )
    .bind(source)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .collect();
    for (root, shared) in native {
        let id = shared
            .clone()
            .unwrap_or_else(|| blake3::hash(root.as_bytes()).to_hex().to_string());
        let label = roots.entry(id).or_default();
        if label.is_empty() {
            *label = std::path::Path::new(root)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
    }
    let mut roots: Vec<_> = roots
        .into_iter()
        .map(|(id, label)| ConnectRoot { id, label })
        .collect();
    roots.sort_by(|left, right| (&left.label, &left.id).cmp(&(&right.label, &right.id)));
    Ok(roots)
}

pub(crate) async fn native_roots(
    connection: &mut SqliteConnection,
    source: &str,
) -> LibraryResult<Vec<(String, Option<String>)>> {
    let roots:Vec<(String,String)>=sqlx::query_as("SELECT DISTINCT access.root,access.root_id FROM local_file_access_roots access JOIN sources source USING(source_key) WHERE source.object_id=?1 ORDER BY access.root_id,access.root")
        .bind(source).fetch_all(connection).await?;
    Ok(roots
        .into_iter()
        .map(|(root, id)| (root, Some(id)))
        .collect())
}

async fn restore_root_label(
    connection: &mut SqliteConnection,
    value: &mut Value,
    source: Option<&str>,
) -> LibraryResult<()> {
    if let (Some(source), Some(root)) = (
        value["source_id"].as_str().or(source),
        value["root_id"].as_str(),
    ) {
        if let Some(label) = sqlx::query_scalar::<_, String>(
            "SELECT label FROM connect_roots WHERE source_id=?1 AND id=?2 AND label<>''",
        )
        .bind(source)
        .bind(root)
        .fetch_optional(connection)
        .await?
        {
            value["root_label"] = label.into();
        }
    }
    Ok(())
}

pub(crate) async fn remember_roots(
    connection: &mut SqliteConnection,
    values: &[&Value],
) -> LibraryResult<()> {
    let roots = values
        .iter()
        .filter_map(|value| {
            let source = value["source_id"].as_str()?;
            let id = value["root_id"].as_str()?;
            let path = value["_local_root"]
                .as_str()
                .or_else(|| value["file"]["root"].as_str())
                .map(str::to_owned)
                .or_else(|| {
                    let path = value["index_source_path"]
                        .as_str()
                        .or_else(|| value["source_path"].as_str())?;
                    let relative = value["relative_path"].as_str()?;
                    let offset = path
                        .replace('\\', "/")
                        .strip_suffix(&relative.replace('\\', "/"))?
                        .len();
                    let root = path.get(..offset)?;
                    Some(
                        if root.len() == 1 || (root.len() == 3 && root.as_bytes()[1] == b':') {
                            root.to_owned()
                        } else {
                            root.trim_end_matches(['/', '\\']).to_owned()
                        },
                    )
                });
            Some((source, id, value["root_label"].as_str(), path))
        })
        .collect::<Vec<_>>();
    if !roots.is_empty() {
        sqlx::query("INSERT INTO connect_roots(source_id,id,label,root) SELECT json_extract(value,'$[0]'),json_extract(value,'$[1]'),coalesce(json_extract(value,'$[2]'),(SELECT label FROM connect_roots known WHERE known.source_id=json_extract(value,'$[0]') AND known.id=json_extract(value,'$[1]')),''),json_extract(value,'$[3]') FROM json_each(?1) WHERE true ON CONFLICT(source_id,id) DO UPDATE SET label=excluded.label,root=coalesce(excluded.root,connect_roots.root) WHERE label IS NOT excluded.label OR(root IS NOT excluded.root AND excluded.root IS NOT NULL)")
            .bind(serde_json::to_string(&roots)?).execute(connection).await?;
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
        if matches!(record.kind.as_str(), "track" | "reference") {
            let uri = value["media_uri"].as_str().unwrap_or(&record.key);
            remember_media(connection, uri, value).await?;
            remember_reference_extras_on(connection, uri, value).await?;
            continue;
        }
        let Some(source) = value["source_id"].as_str() else {
            continue;
        };
        let source = crate::db::write_source_identity(connection, source).await?;
        let object_id = value["object_id"].as_str().unwrap_or_default();
        let mut metadata = value.clone();
        if record.kind == "album" {
            normalize_compilation(&mut metadata);
            if metadata["artist"].is_null() {
                metadata["artist"] = value["display_artist"].clone();
            }
        }
        if let Some(object) = metadata.as_object_mut() {
            object.retain(|_, value| !value.is_null());
        }
        crate::catalog::remember_collection(
            connection,
            if record.kind == "native_playlist" {
                "playlist"
            } else {
                &record.kind
            },
            crate::SourceKey::from_raw(source),
            object_id,
            value["media_uri"].as_str(),
            &metadata,
            imported_artwork(value)?,
        )
        .await?;
    }
    Ok(sqlx::query_scalar::<_, i64>("SELECT total_changes()")
        .fetch_one(connection)
        .await?
        != before)
}

fn hex_bytes(value: &Value) -> Option<Vec<u8>> {
    let text = value.as_str()?;
    if text.len() % 2 != 0 {
        return None;
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            pair.iter().try_fold(0u8, |byte, digit| {
                char::from(*digit)
                    .to_digit(16)
                    .map(|digit| byte * 16 + digit as u8)
            })
        })
        .collect()
}

fn imported_artwork(value: &Value) -> LibraryResult<Option<Vec<u8>>> {
    hex_bytes(&value["artwork_binding"])
        .as_deref()
        .map(crate::normalize_artwork_binding)
        .transpose()
}

async fn remember_media(
    connection: &mut SqliteConnection,
    uri: &str,
    value: &Value,
) -> LibraryResult<crate::TrackKey> {
    let artwork_binding = imported_artwork(value)?;
    let mut metadata = value.clone();
    if let Some(tags) = metadata.get_mut("album_tags") {
        normalize_compilation(tags);
    }
    for (field, aliases) in [
        ("title", &["track_title"][..]),
        ("artist", &["display_artist", "artist_name"][..]),
        ("album", &["display_album", "album_title"][..]),
    ] {
        if metadata[field].is_null() {
            if let Some(value) = aliases
                .iter()
                .find_map(|alias| value.get(*alias).filter(|value| !value.is_null()))
            {
                metadata[field] = value.clone();
            }
        }
    }
    if let Some(object) = metadata.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    let key = crate::catalog::remember_track(
        connection,
        &crate::TrackReference {
            media_uri: uri.into(),
            metadata: serde_json::from_value(metadata)?,
            artwork_binding,
        },
    )
    .await?;
    remember_reference_facts_on(connection, uri, value).await?;
    Ok(key)
}

async fn remember_reference_extras_on(
    connection: &mut SqliteConnection,
    uri: &str,
    value: &Value,
) -> LibraryResult<()> {
    let mut extra = value
        .get("metadata")
        .unwrap_or(value)
        .as_object()
        .cloned()
        .unwrap_or_default();
    let known = serde_json::to_value(crate::TrackMetadata::default())?;
    let transport = "object_id media_uri source_id album_id album_object_id artwork_binding first_seen_at indexed native_metadata root_id root_label root_count relative_path revision local_media cue_start_millis cue_end_millis backing_id index_source_path index_cue_path source_path cue_path file_index display_artist display_album artist_name album_title track_title source_favorite source_rating _local_root _document_backing snapshot_at metadata";
    extra.retain(|key, _| {
        known.get(key).is_none() && !transport.split_whitespace().any(|field| field == key)
    });
    if !extra.is_empty() {
        sqlx::query("UPDATE tracks SET metadata=jsonb_patch(metadata,?2) WHERE media_uri=?1 AND metadata IS NOT jsonb_patch(metadata,?2)")
            .bind(uri).bind(serde_json::to_string(&extra)?).execute(connection).await?;
    }
    Ok(())
}

/// Convert released transport records into the existing catalog and file facts.
pub(crate) async fn remember_reference_facts_on(
    connection: &mut SqliteConnection,
    uri: &str,
    value: &Value,
) -> LibraryResult<()> {
    let source = match value["source_id"].as_str() {
        Some(source) => Some(crate::db::write_source_identity(connection, source).await?),
        None => None,
    };
    let backing = crate::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
    let mut path = value["index_source_path"]
        .as_str()
        .or_else(|| value["source_path"].as_str())
        .map(str::to_owned);
    if path.is_none() {
        path = sqlx::query_scalar("SELECT source_path FROM tracks WHERE media_uri=?1")
            .bind(uri)
            .fetch_optional(&mut *connection)
            .await?
            .flatten();
    }
    let path = path.or(backing).or_else(|| {
        (uri.starts_with("file:") || crate::document_media_id(uri).is_some())
            .then(|| uri.to_owned())
    });
    let cue = value["index_cue_path"]
        .as_str()
        .or_else(|| value["cue_path"].as_str());
    sqlx::query("UPDATE tracks SET source_key=coalesce(source_key,?2),object_id=coalesce(object_id,?3),source_path=coalesce(?4,source_path),cue_path=coalesce(?5,cue_path),cue_start_millis=coalesce(?6,cue_start_millis),cue_end_millis=CASE WHEN ?6 IS NOT NULL THEN ?7 ELSE cue_end_millis END WHERE media_uri=?1 AND indexed=0 AND((source_key IS NULL AND ?2 IS NOT NULL) OR(object_id IS NULL AND ?3 IS NOT NULL) OR(?4 IS NOT NULL AND source_path IS NOT ?4) OR(?5 IS NOT NULL AND cue_path IS NOT ?5) OR(?6 IS NOT NULL AND(cue_start_millis,cue_end_millis) IS NOT(?6,?7)))")
        .bind(uri).bind(source).bind(value["object_id"].as_str()).bind(&path).bind(cue)
        .bind(value["cue_start_millis"].as_i64()).bind(value["cue_end_millis"].as_i64()).execute(&mut *connection).await?;
    remember_roots(connection, &[value]).await?;
    if let Some(source) = source {
        if let Some(path) = path {
            let root: Option<String> = sqlx::query_scalar("SELECT coalesce((SELECT root FROM local_files WHERE source_key=?1 AND path=?2),(SELECT root FROM connect_roots WHERE source_id=?3 AND id=?4))")
                .bind(source).bind(&path).bind(value["source_id"].as_str()).bind(value["root_id"].as_str()).fetch_one(&mut *connection).await?;
            let relative = value["relative_path"].as_str().unwrap_or(&path);
            let root = root
                .or_else(|| value["root_id"].as_str().map(str::to_owned))
                .unwrap_or_else(|| path.clone());
            sqlx::query(
                "UPDATE connect_roots SET root=?3 WHERE source_id=?1 AND id=?2 AND root IS NULL",
            )
            .bind(value["source_id"].as_str())
            .bind(value["root_id"].as_str())
            .bind(&root)
            .execute(&mut *connection)
            .await?;
            let root_id = value["root_id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| blake3::hash(root.as_bytes()).to_hex().to_string());
            sqlx::query("INSERT INTO local_files(source_key,path,root,root_id,relative_path,kind,size_bytes,mtime_ns,revision,state) VALUES(?1,?2,?3,?4,?5,'media',NULL,0,?6,'observed') ON CONFLICT(source_key,path) DO UPDATE SET root_id=coalesce(local_files.root_id,excluded.root_id),revision=excluded.revision,state=CASE WHEN revision IS NOT excluded.revision THEN 'observed' ELSE state END,parsed_stamp=CASE WHEN revision IS NOT excluded.revision THEN NULL ELSE parsed_stamp END WHERE observed_stamp=0 AND NOT EXISTS(SELECT 1 FROM local_file_observations access WHERE access.local_file_key=local_files.local_file_key) AND NOT EXISTS(SELECT 1 FROM tracks native WHERE native.source_key=local_files.source_key AND native.source_path=local_files.path AND native.indexed=1) AND(revision,root_id) IS NOT(excluded.revision,coalesce(local_files.root_id,excluded.root_id))")
                .bind(source).bind(&path).bind(root).bind(root_id).bind(relative).bind(value["revision"].as_str()).execute(&mut *connection).await?;
        }
    }
    Ok(())
}

pub(crate) async fn migrate_collection_on(
    connection: &mut SqliteConnection,
    schema: &str,
) -> LibraryResult<()> {
    if !sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!("SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_schema WHERE type='table' AND name='connect_collection')")))
        .fetch_one(&mut *connection).await? { return Ok(()) }
    let mut transaction = connection.begin().await?;
    let receiving: bool =
        sqlx::query_scalar("SELECT receiving FROM connect_profile WHERE singleton=1")
            .fetch_one(&mut *transaction)
            .await?;
    set_receiving_on(&mut transaction, true).await?;
    let mut after = String::new();
    loop {
        let rows: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT media_uri,payload FROM {schema}.connect_collection WHERE media_uri>?1 ORDER BY media_uri LIMIT 128")))
            .bind(&after).fetch_all(&mut *transaction).await?;
        if rows.is_empty() {
            break;
        }
        for (uri, payload) in rows {
            let value = serde_json::from_str(&payload)?;
            remember_media(&mut transaction, &uri, &value).await?;
            remember_reference_extras_on(&mut transaction, &uri, &value).await?;
            after = uri;
        }
    }
    if schema == "main" {
        sqlx::query("DROP TABLE connect_collection")
            .execute(&mut *transaction)
            .await?;
    }
    set_receiving_on(&mut transaction, receiving).await?;
    transaction.commit().await?;
    Ok(())
}

fn normalize_compilation(metadata: &mut Value) {
    if let Some(value) = metadata["is_compilation"].as_i64() {
        metadata["is_compilation"] = Value::Bool(value != 0);
    }
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
        .bind(serde_json::to_string(&changed)?).execute(&mut *connection).await?;
    if table != "track_folders" {
        let array = match table {
            "track_artists" => "artists",
            "album_artists" => "album_artists",
            "track_genres" | "album_genres" => "genres",
            "track_moods" => "moods",
            _ => unreachable!(),
        };
        let owner_keys = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT DISTINCT l.{left}_key {joins} WHERE l.indexed=0"
        )))
        .bind(serde_json::to_string(&changed)?)
        .fetch_all(&mut *connection)
        .await?;
        // Older peers sent credits separately. Fold them into the accepted snapshot.
        let input = if left == "album" {
            "input_metadata"
        } else {
            "metadata"
        };
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {left}s owner SET {input}=jsonb(json_set(coalesce({input},jsonb('{{}}')),'$.{array}',json((SELECT json_group_array(json_patch(target.metadata,json_object('object_id',target.object_id)) ORDER BY credit.position) FROM {table} credit JOIN {right}s target USING({right}_key) WHERE credit.{left}_key=owner.{left}_key)))) WHERE {left}_key IN(SELECT value FROM json_each(?1))"))).bind(serde_json::to_string(&owner_keys)?).execute(&mut *connection).await?;
        if left == "track" {
            crate::catalog::artwork::refresh_choices(connection, &owner_keys, &[], &[], false)
                .await?;
        }
        if left == "album" {
            let owners=sqlx::query_as::<_,(i64,String,String,String,Option<Vec<u8>>)>("SELECT source_key,object_id,media_uri,json(input_metadata),native_artwork FROM albums WHERE album_key IN(SELECT value FROM json_each(?1))").bind(serde_json::to_string(&owner_keys)?).fetch_all(&mut *connection).await?;
            for (source, id, uri, payload, artwork) in owners {
                crate::catalog::remember_collection(
                    connection,
                    "album",
                    crate::SourceKey::from_raw(source),
                    &id,
                    Some(&uri),
                    &serde_json::from_str(&payload)?,
                    artwork,
                )
                .await?;
            }
        }
    }

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

async fn apply_entry_page(
    connection: &mut SqliteConnection,
    records: &[ConnectRecord],
) -> LibraryResult<bool> {
    let before: i64 = sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(&mut *connection)
        .await?;
    let mut entries = Vec::with_capacity(records.len());
    for record in records {
        let (source, playlist, id): (Option<String>, String, String) =
            serde_json::from_str(&record.key)?;
        let identity = serde_json::to_string(&(source.as_ref(), &playlist))?;
        let playlist = if record.kind == "native_entry" {
            if record.value.is_none() {
                continue;
            }
            let playlist: Option<(crate::PlaylistKey, bool)> = sqlx::query_as("SELECT playlist_key,indexed FROM playlists WHERE owner='source' AND source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND object_id=?2")
                .bind(source).bind(playlist).fetch_optional(&mut *connection).await?;
            let Some((playlist, indexed)) = playlist else {
                return Err(LibraryError::ConnectPending);
            };
            if indexed {
                continue;
            }
            playlist
        } else if record.value.is_none() {
            let existing: Option<crate::PlaylistKey> = sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE owner='rufin' AND source_key IS(SELECT source_key FROM sources WHERE object_id=?1) AND object_id=?2")
                .bind(source).bind(playlist).fetch_optional(&mut *connection).await?;
            let Some(playlist) = existing else {
                continue;
            };
            playlist
        } else {
            crate::PlaylistKey::from_raw(playlist_key(connection, &identity).await?)
        };
        let (track, position) = if let Some(value) = &record.value {
            let uri = value["media_uri"].as_str().ok_or_else(|| {
                LibraryError::InvalidRequest("Connect playlist media is missing".into())
            })?;
            let position = match value["position"].as_i64() {
                Some(position) => position,
                None => sqlx::query_scalar("SELECT coalesce(max(position),-1)+1 FROM playlist_entries WHERE playlist_key=?1")
                    .bind(playlist).fetch_one(&mut *connection).await?,
            };
            (
                Some(remember_media(connection, uri, value).await?),
                position,
            )
        } else {
            (None, 0)
        };
        entries.push((playlist, id, track, position));
    }
    crate::playlists::write_entry_positions_on(connection, &entries).await?;
    Ok(sqlx::query_scalar::<_, i64>("SELECT total_changes()")
        .fetch_one(connection)
        .await?
        != before)
}

async fn apply_order_page(
    connection: &mut SqliteConnection,
    records: &[ConnectRecord],
) -> LibraryResult<bool> {
    let before: i64 = sqlx::query_scalar("SELECT total_changes()")
        .fetch_one(&mut *connection)
        .await?;
    let mut playlists = Vec::new();
    let mut smart = Vec::new();
    for record in records {
        let Some(value) = &record.value else { continue };
        let key = if record.kind == "playlist_order" {
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
            }
        } else {
            let key: Option<i64> = sqlx::query_scalar(
                "SELECT smart_playlist_key FROM smart_playlists WHERE object_id=?1",
            )
            .bind(&record.key)
            .fetch_optional(&mut *connection)
            .await?;
            let Some(key) = key else { continue };
            key
        };
        if record.kind == "playlist_order" && value.get("artwork_binding").is_some() {
            let saved = imported_artwork(value)?;
            let previous: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT artwork_binding FROM playlists WHERE playlist_key=?1")
                    .bind(key)
                    .fetch_one(&mut *connection)
                    .await?;
            let own = previous
                .as_deref()
                .map(crate::artwork::authored_playlist_artwork)
                .transpose()?
                .flatten();
            let binding = crate::artwork::merge_playlist_artwork(saved.as_deref(), own.as_deref())?;
            sqlx::query("UPDATE playlists SET artwork_binding=?2 WHERE playlist_key=?1 AND artwork_binding IS NOT ?2")
                .bind(key).bind(binding).execute(&mut *connection).await?;
        }

        let Some(position) = value["position"].as_i64() else {
            continue;
        };
        let position = position.max(0);
        if record.kind == "playlist_order" {
            playlists.push((crate::PlaylistKey::from_raw(key), position));
        } else {
            smart.push((crate::SmartPlaylistKey::from_raw(key), position));
        }
    }
    crate::playlists::write_positions_on(connection, &playlists).await?;
    crate::smart_playlists::write_positions_on(connection, &smart).await?;
    Ok(sqlx::query_scalar::<_, i64>("SELECT total_changes()")
        .fetch_one(connection)
        .await?
        != before)
}

async fn apply_record(
    connection: &mut SqliteConnection,
    record: &ConnectRecord,
) -> LibraryResult<bool> {
    if record.kind == "playlist_artwork" {
        let image = sqlx::query_as::<_, crate::playlists::PlaylistArtworkRecord>(
            "SELECT json_extract(?1,'$[0]') source_id,json_extract(?1,'$[1]') object_id,
             unhex(json_extract(?2,'$.artwork_bytes')) bytes,json_extract(?2,'$.artwork_mime') content_type,
             unhex(json_extract(?2,'$.artwork_binding')) artwork_binding",
        ).bind(&record.key).bind(record.value.as_ref().map(serde_json::to_string).transpose()?)
            .fetch_one(&mut *connection).await?;
        return crate::playlists::write_playlist_artwork(connection, &image).await;
    }
    if record.kind == "root" {
        if let Some(value) = &record.value {
            remember_roots(connection, &[value]).await?;
        }
        return Ok(false);
    }
    if LINKS.iter().any(|(_, _, table)| *table == record.kind) {
        return Ok(false);
    }
    let Some(projection) = PROJECTIONS.iter().find(|p| p.kind == record.kind) else {
        return Ok(false);
    };
    let identity = format!("{}=?1", projection.key);
    let Some(value) = &record.value else {
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
        "state" => {
            let mut extra = old
                .as_ref()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for field in ["media_uri", "favorite", "rating"] {
                extra.remove(field);
            }
            if let Some(fields) = value.as_object() {
                for (key, value) in fields
                    .iter()
                    .filter(|(key, _)| !matches!(key.as_str(), "media_uri" | "favorite" | "rating"))
                {
                    if value.is_null() {
                        extra.remove(key);
                    } else {
                        extra.insert(key.clone(), value.clone());
                    }
                }
            }
            let extra = (!extra.is_empty())
                .then(|| serde_json::to_string(&extra))
                .transpose()?;
            return Ok(sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating,extra) VALUES(?1,json_extract(?2,'$.favorite'),json_extract(?2,'$.rating'),jsonb(?3)) ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite,rating=excluded.rating,extra=excluded.extra WHERE(favorite,rating,extra) IS NOT(excluded.favorite,excluded.rating,excluded.extra)")
                .bind(&record.key).bind(&payload).bind(extra).execute(connection).await?.rows_affected()>0);
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
                "INSERT INTO listens({columns}) VALUES({values}) ON CONFLICT(external_id) DO UPDATE SET listened_millis=max(listens.listened_millis,excluded.listened_millis),skipped=min(listens.skipped,excluded.skipped) WHERE excluded.listened_millis>listens.listened_millis OR excluded.skipped<listens.skipped"
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
        "smart" => {
            let record = sqlx::query_as::<_, crate::SmartPlaylistWrite>(
                "SELECT ?1 object_id,json_extract(?2,'$.name') name,json_extract(?2,'$.definition_json') definition_json,
                 (SELECT coalesce(max(position),-1)+1 FROM smart_playlists) position,
                 unhex(json_extract(?2,'$.artwork_bytes')) artwork_bytes,unhex(json_extract(?2,'$.artwork_binding')) artwork_binding",
            ).bind(&record.key).bind(payload).fetch_one(&mut *connection).await?;
            crate::smart_playlists::write_smart_playlist(connection, &record).await?;
        }
        _ => unreachable!(),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FavoriteTarget, ReadCancellation};
    const PROFILE_KEY: &str =
        "AGE-SECRET-KEY-1GQ9778VQXMMJVE8SK7J6VT8UJ4HDQAJUVSFCWCM02D8GEWQ72PVQ2Y5J33";

    #[tokio::test]
    async fn saved_playlist_media_replay_preserves_artwork_without_catalog_membership() {
        let directory = tempfile::tempdir().unwrap();
        let source = Database::open(directory.path().join("source.sqlite"))
            .await
            .unwrap();
        let destination_path = directory.path().join("destination.sqlite");
        let destination = Database::open(&destination_path).await.unwrap();
        let source_profile =
            crate::profile::ProfileStore::open(source.clone(), 1, PROFILE_KEY.parse().unwrap())
                .await
                .unwrap();
        source_profile
            .bind_profile("shared", "source")
            .await
            .unwrap();
        let destination_profile = crate::profile::ProfileStore::open(
            destination.clone(),
            2,
            PROFILE_KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        destination_profile
            .bind_profile("shared", "destination")
            .await
            .unwrap();
        destination.connect_set_active(true).await.unwrap();
        // A different source keeps the imported source key independent of the sender.
        crate::db::write_source_identity(
            destination.writer().await.unwrap().as_mut().unwrap(),
            "other",
        )
        .await
        .unwrap();
        let art = serde_json::to_vec(&[crate::ArtworkCandidate::new(
            crate::ArtworkImage::Native {
                source_id: crate::SourceId::new("server"),
                item_id: "cover".into(),
                tag: None,
            },
            None,
        )])
        .unwrap();
        source
            .reconcile_source(&crate::SourceId::new("server"), "server", "Server")
            .await
            .unwrap();
        let mut scan =
            std::future::ready(Ok::<_, crate::LibraryError>(crate::CatalogBatch::default()))
                .await
                .unwrap();
        std::future::ready(Ok::<_, crate::LibraryError>({
            scan.albums.extend_from_slice(&[crate::AlbumObservation {
                user_data: crate::NativeUserState {
                    favorite: Some(false),
                    ..Default::default()
                },
                metadata: crate::AlbumMetadata {
                    title: "Album".into(),
                    artist: "Artist".into(),
                    sort_text: "album".into(),
                    album_artists: vec![crate::ArtistCredit {
                        object_id: "artist".into(),
                        name: "Artist".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "album".into(),
                artwork_binding: Some(art.to_vec()),
                ..Default::default()
            }]);
            scan.albums.len()
        }))
        .await
        .unwrap();
        std::future::ready(Ok::<_, crate::LibraryError>({
            scan.artists.extend_from_slice(&[crate::ArtistObservation {
                metadata: crate::ArtistMetadata {
                    name: "Artist".into(),
                    sort_text: Some("artist".into()),
                    ..Default::default()
                },
                object_id: "artist".into(),
                artwork_binding: Some(art.to_vec()),
                ..Default::default()
            }]);
            scan.artists.len()
        }))
        .await
        .unwrap();
        std::future::ready(Ok::<_, crate::LibraryError>({
            scan.tracks.extend_from_slice(&[crate::TrackObservation {
                user_data: crate::NativeUserState {
                    favorite: Some(false),
                    ..Default::default()
                },
                metadata: crate::TrackMetadata {
                    title: "Track".into(),
                    album: "Album".into(),
                    artist: "Artist".into(),
                    sort_text: "track".into(),
                    duration_millis: 180000,
                    disc_number: 1,
                    track_number: 1,
                    artists: vec![crate::ArtistCredit {
                        object_id: "artist".into(),
                        name: "Artist".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                object_id: "track".into(),
                album_object_id: Some("album".into()),
                artwork_binding: Some(art.to_vec()),
                loudness_analysis_key: [1; 32],
                ..Default::default()
            }]);
            scan.tracks.len()
        }))
        .await
        .unwrap();
        async {
            let result = {
                let mut catalog = source.catalog("server", None).await?;
                catalog.apply_catalog(&scan, true).await?;
                catalog.into_changes()
            };
            Ok::<_, crate::LibraryError>(result)
        }
        .await
        .unwrap();
        let uri = crate::source_entity_uri(&crate::SourceId::new("server"), "track", "track");
        let (playlist, _) = source
            .create_playlist(
                None,
                "Saved",
                &crate::MemberSelection::Uris(((std::slice::from_ref(&uri)).to_vec()).into()),
            )
            .await
            .unwrap()
            .unwrap();
        let binding =
            Database::load_playlist_rows(&mut source.acquire_reader().await.unwrap(), &[playlist])
                .await
                .unwrap()
                .pop()
                .unwrap()
                .artwork_binding
                .unwrap();
        let candidate = serde_json::from_slice::<Vec<crate::ArtworkCandidate>>(&binding)
            .unwrap()
            .remove(0);
        source
            .resolve_playlist_artwork(candidate)
            .await
            .unwrap()
            .unwrap();
        let saved: Vec<u8> =
            sqlx::query_scalar("SELECT artwork_binding FROM playlists WHERE playlist_key=?1")
                .bind(playlist)
                .fetch_one(&mut *source.acquire_reader().await.unwrap())
                .await
                .unwrap();
        let entries = sqlx::query_scalar(
            "SELECT playlist_entry_key FROM playlist_entries WHERE playlist_key=?1",
        )
        .bind(playlist)
        .fetch_all(&mut *source.acquire_reader().await.unwrap())
        .await
        .unwrap();
        source
            .remove_playlist_entries(None, playlist, &entries)
            .await
            .unwrap();
        sqlx::query("UPDATE sources SET kind=NULL WHERE object_id='server'")
            .execute(source.writer().await.unwrap().as_mut().unwrap())
            .await
            .unwrap();
        source.connect_initialize_profile().await.unwrap();
        while source.connect_seed_page().await.unwrap() {}
        let identity:String=sqlx::query_scalar("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id) FROM playlists playlist WHERE playlist_key=?1")
            .bind(playlist).fetch_one(&mut *source.acquire_reader().await.unwrap()).await.unwrap();
        let records = source_profile
            .current_records(&[
                ("playlist".into(), identity.clone()),
                ("playlist_artwork".into(), identity),
                ("reference".into(), uri.clone()),
            ])
            .await
            .unwrap();
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
        let restored: Vec<u8> =
            sqlx::query_scalar("SELECT artwork_binding FROM playlists WHERE name='Saved'")
                .fetch_one(&mut *destination.acquire_reader().await.unwrap())
                .await
                .unwrap();
        assert_eq!(restored, saved);
        for (index, (compilation, expected)) in [
            (Some(serde_json::json!(1)), Some(true)),
            (Some(serde_json::json!(0)), Some(false)),
            (Some(serde_json::json!(true)), Some(true)),
            (Some(serde_json::json!(false)), Some(false)),
            (Some(Value::Null), None),
            (None, None),
        ]
        .into_iter()
        .enumerate()
        {
            let object_id = format!("compilation-{index}");
            let track_uri =
                crate::source_entity_uri(&crate::SourceId::new("server"), "track", &object_id);
            let mut album = serde_json::json!({
                "source_id": "server", "object_id": object_id,
                "title": "Album", "display_artist": "Artist",
            });
            let mut track = serde_json::json!({
                "source_id": "server", "object_id": object_id,
                "media_uri": track_uri, "title": "Track", "album_tags": {},
            });
            if let Some(compilation) = compilation {
                album["is_compilation"] = compilation.clone();
                track["album_tags"]["is_compilation"] = compilation;
            }
            let imported = [
                ConnectRecord {
                    kind: "album".into(),
                    key: crate::source_entity_uri(
                        &crate::SourceId::new("server"),
                        "album",
                        &object_id,
                    ),
                    value: Some(album),
                },
                ConnectRecord {
                    kind: "track".into(),
                    key: track_uri.clone(),
                    value: Some(track),
                },
            ];
            assert!(destination.connect_apply(&imported).await.unwrap());
            let (album, indexed): (String, bool) =
                sqlx::query_as("SELECT json(metadata),indexed FROM albums WHERE object_id=?1")
                    .bind(&object_id)
                    .fetch_one(&mut *destination.acquire_reader().await.unwrap())
                    .await
                    .unwrap();
            assert!(!indexed);
            assert_eq!(
                serde_json::from_str::<crate::AlbumMetadata>(&album)
                    .unwrap()
                    .is_compilation,
                expected
            );
            let (track, indexed): (String, bool) =
                sqlx::query_as("SELECT json(metadata),indexed FROM tracks WHERE media_uri=?1")
                    .bind(&track_uri)
                    .fetch_one(&mut *destination.acquire_reader().await.unwrap())
                    .await
                    .unwrap();
            assert!(!indexed);
            assert_eq!(
                serde_json::from_str::<crate::TrackMetadata>(&track)
                    .unwrap()
                    .album_tags
                    .unwrap()
                    .is_compilation,
                expected
            );
            assert!(!destination.connect_apply(&imported).await.unwrap());
        }
        assert!(!destination.connect_apply(&records).await.unwrap());
        assert!(
            destination_profile
                .changes_in_stream(0, CONNECT_PAGE_SIZE, crate::profile::SyncStream::Authored)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn accepted_edits_survive_restart_and_incoming_replay_does_not_echo() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("profile.sqlite");
        let database = Database::open(&path).await.unwrap();
        let profile =
            crate::profile::ProfileStore::open(database.clone(), 1, PROFILE_KEY.parse().unwrap())
                .await
                .unwrap();
        profile.bind_profile("shared", "source").await.unwrap();
        database.connect_set_active(true).await.unwrap();
        for index in 0..200 {
            database
                .set_favorite(
                    &FavoriteTarget::Track(format!("rufin:source/track/source/{index}")),
                    true,
                )
                .await
                .unwrap();
        }
        let page = profile
            .changes_in_stream(0, CONNECT_PAGE_SIZE, crate::profile::SyncStream::Authored)
            .await
            .unwrap();
        assert_eq!(page.len(), CONNECT_PAGE_SIZE);
        profile.close().await.unwrap();
        database.close().await.unwrap();
        let database = Database::open(&path).await.unwrap();
        let reopened_profile =
            crate::profile::ProfileStore::open(database.clone(), 1, PROFILE_KEY.parse().unwrap())
                .await
                .unwrap();
        let replay = reopened_profile
            .changes_in_stream(0, CONNECT_PAGE_SIZE, crate::profile::SyncStream::Authored)
            .await
            .unwrap();
        assert_eq!(page[0].name, replay[0].name);
        assert_eq!(page[0].version, replay[0].version);
        let destination = Database::open(directory.path().join("destination.sqlite"))
            .await
            .unwrap();
        let destination_profile = crate::profile::ProfileStore::open(
            destination.clone(),
            2,
            PROFILE_KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        destination_profile
            .bind_profile("shared", "destination")
            .await
            .unwrap();
        destination.connect_set_active(true).await.unwrap();
        let keys = page
            .iter()
            .map(|version| {
                (
                    "state".into(),
                    version.name.strip_prefix("state:").unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let records = reopened_profile.current_records(&keys).await.unwrap();
        assert!(destination.connect_apply(&records).await.unwrap());
        assert!(!destination.connect_apply(&records).await.unwrap());
        assert!(
            destination_profile
                .changes_in_stream(0, CONNECT_PAGE_SIZE, crate::profile::SyncStream::Authored)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened_profile
                .changes_in_stream(
                    page.last().unwrap().revision,
                    CONNECT_PAGE_SIZE,
                    crate::profile::SyncStream::Authored
                )
                .await
                .unwrap()
                .len(),
            72
        );
    }

    #[tokio::test]
    async fn imported_history_never_creates_service_submissions() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("profile.sqlite"))
            .await
            .unwrap();
        for (started, title, artist, album, duration, listened) in [
            (1, "Old title", "Old artist", "Old album", 1000, 900),
            (
                2,
                "Retained song",
                "Retained artist",
                "Retained album",
                2000,
                1900,
            ),
        ] {
            let record = serde_json::json!({"version":1,"listen_key":started,"source_id":"removed","media_uri":"https://example.org/retained","title":title,"artist":artist,"album":album,"started_at":started,"local_period":"2026-10","duration_millis":duration,"listened_millis":listened,"skipped":false});
            database
                .import_activity_jsonl(std::io::Cursor::new(serde_json::to_vec(&record).unwrap()))
                .await
                .unwrap();
        }
        database.close().await.unwrap();
        let database = Database::open(directory.path().join("profile.sqlite"))
            .await
            .unwrap();
        let recovered = database
            .tracks_by_uris(
                &([("https://example.org/retained").to_string()]),
                &crate::ReadCancellation::new(),
            )
            .await
            .map(|rows| {
                let mut rows = rows;
                rows.pop()
            })
            .unwrap()
            .unwrap();
        assert_eq!(recovered.metadata.title, "Retained song");
        assert_eq!(recovered.metadata.artist, "Retained artist");
        assert_eq!(recovered.metadata.album, "Retained album");
        {
            let mut reader = database.acquire_reader().await.unwrap();
            assert_eq!(
                crate::schema::pragma(&mut reader, "user_version")
                    .await
                    .unwrap(),
                54
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
        let selection = crate::MemberRead::new(crate::Collection::History {
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
        let history = (&crate::MemberRead::new(crate::Collection::History { source: None }))
            .rows(&database, 0, 100, &crate::ReadCancellation::new())
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(crate::MemberRow::into_track)
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].metadata.title, "Retained song");
        assert_eq!(history[1].metadata.title, "Song");
        assert_eq!(history[1].metadata.artist, "Artist");
        assert_eq!(history[1].metadata.album, "Album");
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
        let source_profile =
            crate::profile::ProfileStore::open(source.clone(), 1, PROFILE_KEY.parse().unwrap())
                .await
                .unwrap();
        source_profile
            .bind_profile("shared", "source")
            .await
            .unwrap();
        let destination_profile = crate::profile::ProfileStore::open(
            destination.clone(),
            2,
            PROFILE_KEY.parse().unwrap(),
        )
        .await
        .unwrap();
        destination_profile
            .bind_profile("shared", "destination")
            .await
            .unwrap();
        source.connect_set_active(true).await.unwrap();
        destination.connect_set_active(true).await.unwrap();
        let media = "https://example.org/same.flac".to_owned();
        let (playlist, _) = source
            .create_playlist(
                None,
                "Shared",
                &crate::MemberSelection::Uris(((&[media.clone(), media.clone()]).to_vec()).into()),
            )
            .await
            .unwrap()
            .unwrap();
        destination
            .create_playlist(
                None,
                "Old local playlist",
                &crate::MemberSelection::Uris(((std::slice::from_ref(&media)).to_vec()).into()),
            )
            .await
            .unwrap()
            .unwrap();
        let identity:String=sqlx::query_scalar("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id) FROM playlists playlist WHERE playlist_key=?1")
            .bind(playlist).fetch_one(&mut *source.acquire_reader().await.unwrap()).await.unwrap();
        let entry_keys:Vec<String>=sqlx::query_scalar("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id,entry.object_id) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.playlist_key=?1")
            .bind(playlist).fetch_all(&mut *source.acquire_reader().await.unwrap()).await.unwrap();
        let keys = std::iter::once(("playlist".into(), identity.clone()))
            .chain(entry_keys.into_iter().map(|key| ("entry".into(), key)))
            .collect::<Vec<_>>();
        let records = source_profile.current_records(&keys).await.unwrap();
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
        let snapshot = directory.path().join("shared.sqlite");
        source_profile
            .export_database(&snapshot, &PROFILE_KEY.parse().unwrap(), &[], &[])
            .await
            .unwrap();
        crate::profile::ProfileStore::install_prepared_database(destination.clone(), &snapshot)
            .await
            .unwrap();
        assert!(destination.connect_apply(&records).await.unwrap());
        assert!(!destination.connect_apply(&records).await.unwrap());
        let before = destination_profile.revision().await.unwrap();
        assert!(!destination.connect_apply(&records).await.unwrap());
        assert_eq!(destination_profile.revision().await.unwrap(), before);
        source
            .add_playlist_tracks(
                None,
                playlist,
                &crate::MemberSelection::Uris(((std::slice::from_ref(&media)).to_vec()).into()),
                false,
            )
            .await
            .unwrap();
        let last = sqlx::query_scalar::<_, crate::PlaylistEntryKey>(
            "SELECT playlist_entry_key FROM main.playlist_entries WHERE playlist_key=?1 ORDER BY position DESC LIMIT 1")
            .bind(playlist).fetch_one(&mut *source.acquire_reader().await.unwrap()).await.unwrap();
        source
            .move_playlist_entry(None, playlist, last, 0)
            .await
            .unwrap();
        let entry_keys:Vec<String>=sqlx::query_scalar("SELECT json_array((SELECT object_id FROM sources WHERE source_key=playlist.source_key),playlist.object_id,entry.object_id) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.playlist_key=?1")
            .bind(playlist).fetch_all(&mut *source.acquire_reader().await.unwrap()).await.unwrap();
        let mut edits = source_profile
            .current_records(
                &entry_keys
                    .into_iter()
                    .map(|key| ("entry".into(), key))
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
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
        let before: Vec<(String, i64)> = sqlx::query_as(
            "SELECT object_id,playlist_entry_key FROM playlist_entries ORDER BY object_id",
        )
        .fetch_all(&mut *database.acquire_reader().await.unwrap())
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
        let after: Vec<(String, i64)> = sqlx::query_as(
            "SELECT object_id,playlist_entry_key FROM playlist_entries ORDER BY object_id",
        )
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        assert_eq!(after, before);
    }
}
