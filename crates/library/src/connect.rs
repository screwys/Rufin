//! Connect's own Store tables and the reads Connect makes by key. Connect does not write
//! library rows.
use crate::{Catalog, LibraryError, LibraryResult};
mod database;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

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

pub(crate) const NEEDED_TRACK_KEYS: &str = "SELECT entry.track_key FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.owner='rufin'
    UNION SELECT track_key FROM queue_entries
    UNION SELECT track.track_key FROM local_locators locator JOIN tracks track USING(media_uri) WHERE locator.origin='download'
    UNION SELECT track.track_key FROM (SELECT artwork_binding FROM playlists UNION ALL SELECT artwork_binding FROM smart_playlists) artwork
    CROSS JOIN json_each(artwork.artwork_binding) candidate
    CROSS JOIN json_each(candidate.value,'$.image.automatic.chosen') chosen
    JOIN tracks track ON track.media_uri=chosen.value";

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

impl Catalog {
    pub async fn connect_set_active(&self, enabled: bool) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("UPDATE connect_profile SET enabled=?1 WHERE singleton=1")
            .bind(enabled)
            .execute(&mut *writer)
            .await?;
        Ok(())
    }

    /// What a peer needs to stream or download a track this device holds.
    pub async fn connect_track_reference(&self, media_uri: &str) -> LibraryResult<Option<Value>> {
        let mut connection = self.acquire_reader().await?;
        let metadata = crate::track_metadata_sql("r", "d");
        let whole_file = crate::local::whole_file_cue_sql("r", "d");
        let payload: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT json_set(json_patch({metadata},json_object('object_id',r.object_id,'media_uri',r.media_uri,'first_seen_at',r.first_seen_at,
                'artwork_binding',CASE WHEN d.native_artwork IS NULL THEN NULL ELSE hex(d.native_artwork) END,
                'source_id',(SELECT object_id FROM sources WHERE source_key=r.source_key),'album_id',(SELECT object_id FROM albums WHERE album_key=r.album_key),
                'root_id',(SELECT root_id FROM local_files file WHERE file.source_key=r.source_key AND file.path=d.source_path LIMIT 1),
                '_local_root',coalesce((SELECT root FROM local_files WHERE source_key=r.source_key AND path=d.source_path),(SELECT root FROM local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),
                'relative_path',coalesce((SELECT relative_path FROM local_files WHERE source_key=r.source_key AND path=d.source_path),(SELECT relative_path FROM local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)),
                'revision',(SELECT revision FROM local_files WHERE source_key=r.source_key AND path=coalesce(d.source_path,(SELECT path FROM local_locators WHERE media_uri=r.media_uri AND origin IN ('local','import') LIMIT 1)) LIMIT 1),
                'display_artist',r.display_artist,'display_album',r.display_album,'source_favorite',r.native_favorite,'source_rating',r.native_rating,
                'local_media',json(CASE WHEN EXISTS(SELECT 1 FROM local_files f WHERE f.source_key=r.source_key AND f.path=d.source_path AND f.path LIKE 'rufin-document:%') OR EXISTS(SELECT 1 FROM local_locators l WHERE l.media_uri=r.media_uri AND l.origin IN ('local','import') AND l.access_uri LIKE 'content://%') THEN 'true' ELSE 'false' END),
                'cue_start_millis',d.cue_start_millis,'cue_end_millis',d.cue_end_millis)),
                '$.index_source_path',d.source_path,'$.index_cue_path',d.cue_path,'$.cue_end_millis',CASE WHEN {whole_file} THEN NULL ELSE d.cue_end_millis END)
             FROM tracks r LEFT JOIN track_details d USING(track_key) WHERE r.media_uri=?1 AND (r.indexed=1 OR (r.track_key IN ({NEEDED_TRACK_KEYS}) AND (d.source_path IS NOT NULL OR EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=r.media_uri AND locator.origin IN ('local','import')))))"
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
