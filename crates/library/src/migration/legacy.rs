//! Reads released source-scoped user facts; catalog rows are consulted only for referenced locators.
use super::{has_table, imported, integer, string};
use crate::{
    Database, LibraryError, LibraryResult, OccurrenceId, QueueItem, QueueOccurrence,
    QueueProvenance, SourceId,
};
use futures_util::TryStreamExt;
use sqlx::{SqliteConnection, sqlite::SqliteRow};
use std::io::{Seek, Write};

fn required(row: &SqliteRow, name: &str) -> LibraryResult<String> {
    string(row, name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| LibraryError::InvalidStore(format!("missing legacy {name}")))
}
fn file_uri(path: &str) -> Option<String> {
    crate::normalize_direct_media_uri(path).or_else(|| {
        url::Url::from_file_path(path)
            .ok()
            .map(|url| url.to_string())
    })
}
async fn track(
    lookup: &mut SqliteConnection,
    source: &str,
    id: &str,
) -> LibraryResult<Option<SqliteRow>> {
    if !has_table(lookup, "tracks").await? || !has_table(lookup, "source_libraries").await? {
        return Ok(None);
    }
    Ok(sqlx::query("SELECT track.* FROM source_libraries library CROSS JOIN tracks track ON track.library_id=library.library_id WHERE library.source_id=?1 AND track.track_id=?2 ORDER BY library.accepted_at IS NULL,library.library_id DESC LIMIT 1").bind(source).bind(id).fetch_optional(lookup).await?)
}
fn uri(source: &str, kind: &str, id: &str, track: Option<&SqliteRow>) -> LibraryResult<String> {
    if source.is_empty() || id.is_empty() {
        return Err(LibraryError::InvalidStore(
            "missing legacy media locator".into(),
        ));
    }
    if kind == "track" && source == "local:server:library" {
        if let Some(track) = track {
            if let Some(path) = string(track, "source_path").and_then(|path| file_uri(&path)) {
                if let (Some(_), Some(start), Some(end)) = (
                    string(track, "cue_path"),
                    integer(track, "cue_start_millis"),
                    integer(track, "cue_end_millis"),
                ) {
                    return Ok(crate::cue_media_uri(id, &path, start, end));
                }
                return Ok(path);
            }
        }
        if let Some(path) = file_uri(id) {
            return Ok(path);
        }
    }
    Ok(crate::normalize_direct_media_uri(id)
        .unwrap_or_else(|| crate::source_entity_uri(&SourceId::new(source), kind, id)))
}
fn item(media_uri: String, track: Option<&SqliteRow>) -> QueueItem {
    let mut item = QueueItem::direct(media_uri, "", "", "", 0);
    if let Some(track) = track {
        item.title = string(track, "title").unwrap_or_else(|| item.media_uri.clone());
        item.artist = string(track, "display_artist").unwrap_or_default();
        item.album = string(track, "display_album").unwrap_or_default();
        item.duration_millis = integer(track, "duration_seconds")
            .unwrap_or(0)
            .saturating_mul(1000)
            .max(0);
        item.disc_number = integer(track, "disc_number");
        item.track_number = integer(track, "track_number");
        item.year = integer(track, "year");
        item.release_date = string(track, "release_date");
        item.source_format = string(track, "source_format");
        item.musicbrainz_recording_id = string(track, "musicbrainz_recording_id");
        item.musicbrainz_release_track_id = string(track, "musicbrainz_release_track_id");
    } else {
        item.title = item.media_uri.clone();
    }
    item
}

pub(super) async fn import(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    database: &Database,
    selected: Option<&SourceId>,
) -> LibraryResult<()> {
    {
        let mut writer = database.writer().await?;
        let target = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let result = playlists(source, lookup, target).await;
        if let Err(error) = result {
            imported::<()>(Err(error), "legacy playlists");
        }
        let result = user_state(source, lookup, target).await;
        if let Err(error) = result {
            imported::<()>(Err(error), "legacy favorites and ratings");
        }
        let result = smart(source, target).await;
        if let Err(error) = result {
            imported::<()>(Err(error), "legacy Smart playlists");
        }
        let result = listens(source, lookup, target).await;
        if let Err(error) = result {
            imported::<()>(Err(error), "legacy Activity");
        }
        let result = locators(source, lookup, target).await;
        if let Err(error) = result {
            imported::<()>(Err(error), "legacy Local locators");
        }
    }
    let result = queue(source, lookup, database, selected).await;
    if let Err(error) = result {
        imported::<()>(Err(error), "legacy Queue");
    }
    Ok(())
}

async fn playlists(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
) -> LibraryResult<()> {
    if !has_table(source, "local_playlists").await? {
        return Ok(());
    }
    sqlx::query("CREATE TEMP TABLE legacy_playlist_keys(source_id TEXT,object_id TEXT,playlist_key INTEGER,PRIMARY KEY(source_id,object_id)) WITHOUT ROWID")
        .execute(&mut *target).await?;
    let mut rows = sqlx::query("SELECT * FROM local_playlists ORDER BY source_id,playlist_id")
        .fetch(&mut *source);
    let mut position = 0;
    while let Some(row) = rows.try_next().await? {
        let result = async {
            let source_id = required(&row, "source_id")?;
            let object_id = required(&row, "playlist_id")?;
            let identity = sqlx::query_scalar::<_, String>(
                "SELECT CASE WHEN EXISTS(SELECT 1 FROM main.playlists WHERE source_key IS NULL AND object_id=?1)
                 THEN 'rufin:playlist:'||lower(hex(randomblob(16))) ELSE ?1 END",
            ).bind(&object_id).fetch_one(&mut *target).await?;
            let key = crate::playlists::write_playlist_identity(
                target,
                &crate::playlists::PlaylistIdentity {
                    source_id: None,
                    object_id: identity,
                    name: Some(required(&row, "name")?),
                    position,
                },
            )
            .await?;
            sqlx::query("INSERT INTO temp.legacy_playlist_keys VALUES(?1,?2,?3)")
                .bind(source_id).bind(object_id).bind(key).execute(&mut *target).await?;
            Ok(())
        }
        .await;
        if imported(result, "legacy playlist").is_some() {
            position += 1;
        }
    }
    drop(rows);
    if !has_table(source, "local_playlist_entries").await? {
        return Ok(());
    }
    let mut rows =
        sqlx::query("SELECT * FROM local_playlist_entries ORDER BY source_id,playlist_id,position")
            .fetch(&mut *source);
    while let Some(row) = rows.try_next().await? {
        let result=async {
            let source_id=required(&row,"source_id")?;let object_id=required(&row,"playlist_id")?;let track_id=required(&row,"track_id")?;
            let key=sqlx::query_scalar::<_,crate::PlaylistKey>("SELECT playlist_key FROM temp.legacy_playlist_keys WHERE source_id=?1 AND object_id=?2").bind(&source_id).bind(object_id).fetch_optional(&mut *target).await?.ok_or_else(||LibraryError::InvalidStore("legacy playlist entry has no readable playlist".into()))?;
            let track=track(lookup,&source_id,&track_id).await?;let item=item(uri(&source_id,"track",&track_id,track.as_ref())?,track.as_ref());
            crate::playlists::write_playlist_entry(target,key,&crate::playlists::PlaylistEntryWrite{object_id:required(&row,"occurrence_id")?,media_uri:item.media_uri,title:Some(item.title),artist:Some(item.artist),album:Some(item.album),album_display_artist:item.album_display_artist,snapshot_at:0,duration_millis:Some(item.duration_millis),disc_number:item.disc_number,track_number:item.track_number,year:item.year,release_date:item.release_date,source_format:item.source_format,musicbrainz_recording_id:item.musicbrainz_recording_id,musicbrainz_release_track_id:item.musicbrainz_release_track_id,position:integer(&row,"position").ok_or_else(||LibraryError::InvalidStore("missing playlist order".into()))?}).await
        }.await;
        imported(result, "legacy playlist occurrence");
    }
    Ok(())
}

async fn user_state(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
) -> LibraryResult<()> {
    for (table, query) in [
        ("local_favorites", "SELECT * FROM local_favorites"),
        ("user_ratings", "SELECT * FROM user_ratings"),
        ("pending_favorites", "SELECT * FROM pending_favorites"),
    ] {
        if !has_table(source, table).await? {
            continue;
        }
        let mut rows = sqlx::query(query).fetch(&mut *source);
        while let Some(row) = rows.try_next().await? {
            let result=async {
                let source_id=required(&row,"source_id")?;let kind=required(&row,"item_kind")?;let id=required(&row,"item_id")?;
                if !matches!(kind.as_str(),"track"|"album"|"artist") {return Err(LibraryError::InvalidStore("unknown legacy favorite kind".into()));}
                let track=if kind=="track" {track(lookup,&source_id,&id).await?} else {None};
                let media_uri=uri(&source_id,&kind,&id,track.as_ref())?;
                let (favorite,rating)=sqlx::query_as::<_,(Option<bool>,Option<i64>)>("SELECT favorite,rating FROM user_media_state WHERE media_uri=?1").bind(&media_uri).fetch_optional(&mut *target).await?.unwrap_or((None,None));
                let record=crate::favorites::UserMediaStateWrite{media_uri:media_uri.clone(),favorite:if table=="local_favorites" {Some(true)} else if table=="pending_favorites" {Some(integer(&row,"favorite")==Some(1))} else {favorite},rating:if table=="user_ratings" {Some(integer(&row,"rating").ok_or_else(||LibraryError::InvalidStore("missing rating".into()))?.saturating_mul(10))} else {rating}};
                crate::favorites::write_user_media_state(target,&record).await?;
                if table=="pending_favorites" {sqlx::query("INSERT INTO favorite_outbox(media_uri,favorite,previous_favorite,attempts,next_attempt_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite,previous_favorite=excluded.previous_favorite,attempts=excluded.attempts,next_attempt_at=excluded.next_attempt_at").bind(media_uri).bind(integer(&row,"favorite")).bind(integer(&row,"previous_favorite")).bind(integer(&row,"attempts").unwrap_or(0)).bind(integer(&row,"next_attempt_at").unwrap_or(0)).execute(&mut *target).await?;}
                Ok(())
            }.await;
            imported(result, table);
        }
    }
    Ok(())
}

async fn smart(source: &mut SqliteConnection, target: &mut SqliteConnection) -> LibraryResult<()> {
    if !has_table(source, "smart_playlists").await? {
        return Ok(());
    }
    let version = sqlx::query_scalar::<_, i64>("PRAGMA user_version")
        .fetch_one(&mut *source)
        .await?;
    let mut rows =
        sqlx::query("SELECT * FROM smart_playlists ORDER BY source_id,position,smart_playlist_id")
            .fetch(&mut *source);
    let mut position = 0;
    while let Some(row) = rows.try_next().await? {
        let result = async {
            let mut definition: serde_json::Value =
                serde_json::from_str(&required(&row, "definition_json")?)?;
            definition["current"] = true.into();
            if version < 40 {
                for field in ["match_all", "match_any"] {
                    if let Some(rules) = definition
                        .get_mut(field)
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for rule in rules {
                            if rule["field"] == "Rating" {
                                if let Some(value) = rule["value"]["Number"].as_i64() {
                                    rule["value"]["Number"] = value.saturating_mul(2).into();
                                }
                                for edge in ["min", "max"] {
                                    if let Some(value) = rule["value"]["NumberRange"][edge].as_i64()
                                    {
                                        rule["value"]["NumberRange"][edge] =
                                            value.saturating_mul(2).into();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            crate::smart_playlists::write_smart_playlist(
                target,
                &crate::smart_playlists::SmartPlaylistWrite {
                    object_id: required(&row, "smart_playlist_id")?,
                    name: required(&row, "name")?,
                    definition_json: serde_json::to_string(&definition)?,
                    position,
                },
            )
            .await
        }
        .await;
        if imported(result, "legacy Smart playlist").is_some() {
            position += 1;
        }
    }
    Ok(())
}

async fn listens(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
) -> LibraryResult<()> {
    let has_recent = has_table(source, "recent_plays").await?;
    for (table, query) in [
        (
            "recent_plays",
            "SELECT *,strftime('%Y-%m',played_at,'unixepoch') period FROM recent_plays ORDER BY played_at,play_id",
        ),
        (
            "pending_scrobbles",
            "SELECT *,strftime('%Y-%m',started_at,'unixepoch') period FROM pending_scrobbles ORDER BY started_at,play_id",
        ),
    ] {
        if !has_table(source, table).await? {
            continue;
        }
        let mut rows = sqlx::query(query).fetch(&mut *source);
        while let Some(row) = rows.try_next().await? {
            let result=async {
                let play=required(&row,"play_id")?;
                let recent=if table=="pending_scrobbles" && has_recent {sqlx::query("SELECT source_id,track_id FROM recent_plays WHERE play_id=?1").bind(&play).fetch_optional(&mut *lookup).await?} else {None};
                let source_id=string(&row,"source_id").or_else(||recent.as_ref().and_then(|row|string(row,"source_id")));
                let track_id=string(&row,"track_id").or_else(||recent.as_ref().and_then(|row|string(row,"track_id"))).unwrap_or_else(||play.clone());
                let track=if let Some(source_id)=&source_id {track(lookup,source_id,&track_id).await?} else {None};
                let item=item(uri(source_id.as_deref().unwrap_or("rufin:recovered"),"track",&track_id,track.as_ref())?,track.as_ref());
                let duration=integer(&row,"duration_millis").unwrap_or(item.duration_millis).max(0);
                let listen=crate::ListenWrite{external_id:Some(play),media_uri:item.media_uri,title:required(&row,"track_title")?,artist:string(&row,"artist_name").unwrap_or_default(),album:string(&row,"album_title").unwrap_or_default(),duration_millis:duration,disc_number:item.disc_number,track_number:item.track_number,year:item.year,release_date:item.release_date,source_format:item.source_format,musicbrainz_recording_id:item.musicbrainz_recording_id,musicbrainz_release_track_id:item.musicbrainz_release_track_id,started_at:integer(&row,"played_at").or_else(||integer(&row,"started_at")).ok_or_else(||LibraryError::InvalidStore("missing legacy listen time".into()))?,local_period:required(&row,"period")?,listened_millis:if table=="pending_scrobbles" {duration} else {0},skipped:false};
                let key=crate::activity::write_listen(target,&listen,source_id.as_deref()).await?;
                if table=="pending_scrobbles" {
                    sqlx::query("INSERT INTO listen_outbox(listen_key,service,account_id,attempts,next_attempt_at,last_error) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(listen_key,service,account_id) DO UPDATE SET attempts=excluded.attempts,next_attempt_at=excluded.next_attempt_at,last_error=excluded.last_error").bind(key).bind(required(&row,"service")?).bind(required(&row,"account_id")?).bind(integer(&row,"attempts").unwrap_or(0)).bind(integer(&row,"next_attempt_at")).bind(string(&row,"last_error")).execute(&mut *target).await?;
                }
                Ok(())
            }.await;
            imported(result, table);
        }
    }
    if has_table(source, "listening_aggregates").await? {
        let mut rows = sqlx::query("SELECT * FROM listening_aggregates").fetch(&mut *source);
        while let Some(row) = rows.try_next().await? {
            let result = async {
                crate::activity::write_legacy_activity(
                    target,
                    &crate::activity::LegacyActivityRecord {
                        source_id: required(&row, "source_id")?,
                        period: required(&row, "period")?,
                        item_kind: required(&row, "item_kind")?,
                        track_object_id: required(&row, "item_id")?,
                        play_count: integer(&row, "play_count").unwrap_or(0),
                        skip_count: integer(&row, "skip_count").unwrap_or(0),
                        last_played_at: integer(&row, "last_played_at"),
                    },
                )
                .await?;
                Ok(())
            }
            .await;
            imported(result, "legacy Activity totals");
        }
    }
    Ok(())
}

async fn write_locator(
    target: &mut SqliteConnection,
    source: &str,
    media_uri: &str,
    path: &str,
    root: &str,
    relative: &str,
    origin: &str,
) -> LibraryResult<()> {
    let access = file_uri(path)
        .ok_or_else(|| LibraryError::InvalidStore("invalid legacy file locator".into()))?;
    crate::local::write_local_locator(
        target,
        &crate::LocalLocatorWrite {
            source_id: Some(source.to_string()),
            media_uri: media_uri.to_string(),
            origin: origin.to_string(),
            path: path.to_string(),
            root: root.to_string(),
            relative_path: relative.to_string(),
            access_uri: access,
        },
    )
    .await?;
    Ok(())
}
async fn locators(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
) -> LibraryResult<()> {
    // Resolve only access-file references. Old schemas have no path index, so make one
    // bounded-field temporary relation once instead of rescanning tracks per mapping.
    if has_table(lookup, "local_access_files").await? && has_table(lookup, "tracks").await? {
        sqlx::raw_sql("CREATE TEMP TABLE legacy_access_tracks AS SELECT library.source_id,track.track_id,track.source_path FROM source_libraries library JOIN tracks track USING(library_id) WHERE track.source_path IN (SELECT path FROM local_access_files); CREATE INDEX temp.legacy_access_tracks_path ON legacy_access_tracks(source_id,source_path);").execute(&mut *lookup).await?;
    }
    for (table, query) in [
        (
            "local_files",
            "SELECT file.*,library.source_id FROM local_files file JOIN source_libraries library USING(library_id) WHERE file.kind IN('media','cue') ORDER BY library.library_id",
        ),
        ("local_access_files", "SELECT * FROM local_access_files"),
        ("local_imports", "SELECT * FROM local_imports"),
        ("downloaded_tracks", "SELECT * FROM downloaded_tracks"),
    ] {
        if !has_table(source, table).await? {
            continue;
        }
        let mut rows = sqlx::query(query).fetch(&mut *source);
        while let Some(row) = rows.try_next().await? {
            let result = async {
                let source_id = required(&row, "source_id")?;
                if matches!(table,"local_files"|"local_imports") && source_id != "local:server:library" { return Ok(()); }
                let track_id = if table == "local_access_files" && has_table(lookup,"tracks").await? {
                    sqlx::query_scalar::<_,String>("SELECT track_id FROM temp.legacy_access_tracks WHERE source_id=?1 AND source_path=?2 ORDER BY track_id LIMIT 1").bind(&source_id).bind(string(&row,"path")).fetch_optional(&mut *lookup).await?
                } else {string(&row, "track_id")};
                let track = if let Some(id) = &track_id {
                    track(lookup, &source_id, id).await?
                } else {
                    None
                };
                let path = string(&row, "path")
                    .or_else(|| track.as_ref().and_then(|row| string(row, "source_path")))
                    .or_else(|| track_id.clone().filter(|id| id.starts_with('/')))
                    .ok_or_else(|| {
                        LibraryError::InvalidStore(
                            "legacy Local import has no readable file locator".into(),
                        )
                    })?;
                let media_uri = if let Some(id) = &track_id {
                    uri(&source_id, "track", id, track.as_ref())?
                } else {
                    file_uri(&path).ok_or_else(|| {
                        LibraryError::InvalidStore("invalid legacy Local path".into())
                    })?
                };
                let origin = match table {
                    "local_imports" => "import",
                    "downloaded_tracks" => "download",
                    "local_access_files" => "mapping",
                    _ => "local",
                };
                write_locator(
                    target,
                    &source_id,
                    &media_uri,
                    &path,
                    &string(&row, "root").unwrap_or_default(),
                    &string(&row, "relative_path").unwrap_or_default(),
                    origin,
                )
                .await
            }
            .await;
            imported(result, table);
        }
    }
    Ok(())
}

async fn queue(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    database: &Database,
    selected: Option<&SourceId>,
) -> LibraryResult<()> {
    if !has_table(source, "playback_queues").await? {
        return Ok(());
    }
    let Some(selected) = selected else {
        return Ok(());
    };
    let selected = selected.as_str();
    let present = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM playback_queues WHERE source_id=?1)",
    )
    .bind(selected)
    .fetch_one(&mut *source)
    .await?;
    if !present {
        return Ok(());
    }
    let split = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('playback_queues') WHERE name='rows_json')",
    )
    .fetch_one(&mut *source)
    .await?;
    // SQLite parses each old JSON payload once into indexed temporary relations; no full
    // fallback Track vector or catalog reconstruction is needed in Rust.
    sqlx::raw_sql("CREATE TEMP TABLE legacy_queue_rows(id TEXT PRIMARY KEY,track_id TEXT,canonical INTEGER,provenance TEXT); CREATE TEMP TABLE legacy_queue_fallback(id TEXT PRIMARY KEY,payload TEXT); CREATE TEMP TABLE legacy_queue_traversal(id TEXT PRIMARY KEY,position INTEGER);").execute(&mut *lookup).await?;
    let rows_sql = if split {
        "INSERT OR IGNORE INTO temp.legacy_queue_rows SELECT json_extract(value,'$.id'),json_extract(value,'$.track_id'),key,json_extract(value,'$.provenance') FROM playback_queues,json_each(rows_json,'$.occurrences') WHERE source_id=?1 AND json_valid(value)"
    } else {
        "INSERT OR IGNORE INTO temp.legacy_queue_rows SELECT json_extract(value,'$.id'),json_extract(value,'$.track_id'),key,json_extract(value,'$.provenance') FROM playback_queues,json_each(payload_json,'$.occurrences') WHERE source_id=?1 AND json_valid(value)"
    };
    sqlx::query(rows_sql)
        .bind(selected)
        .execute(&mut *lookup)
        .await?;
    let fallback_sql = if split {
        "INSERT OR IGNORE INTO temp.legacy_queue_fallback SELECT json_extract(value,'$.id'),value FROM playback_queues,json_each(rows_json,'$.fallback_tracks') WHERE source_id=?1 AND json_valid(value)"
    } else {
        "INSERT OR IGNORE INTO temp.legacy_queue_fallback SELECT json_extract(value,'$.id'),value FROM playback_queues,json_each(payload_json,'$.fallback_tracks') WHERE source_id=?1 AND json_valid(value)"
    };
    sqlx::query(fallback_sql)
        .bind(selected)
        .execute(&mut *lookup)
        .await?;
    let traversal_sql = if split {
        "INSERT OR IGNORE INTO temp.legacy_queue_traversal SELECT value,key FROM playback_queues,json_each(traversal_json) WHERE source_id=?1 AND type='text'"
    } else {
        "INSERT OR IGNORE INTO temp.legacy_queue_traversal SELECT value,key FROM playback_queues,json_each(payload_json,'$.traversal') WHERE source_id=?1 AND type='text'"
    };
    sqlx::query(traversal_sql)
        .bind(selected)
        .execute(&mut *lookup)
        .await?;
    let invalid=sqlx::query("DELETE FROM temp.legacy_queue_rows WHERE id IS NULL OR id='' OR track_id IS NULL OR track_id=''").execute(&mut *lookup).await?.rows_affected();
    if invalid > 0 {
        tracing::warn!(invalid, "skipped malformed released Queue entries");
    }
    sqlx::raw_sql("CREATE TEMP TABLE legacy_queue_ready AS WITH canonical AS (SELECT *,row_number() OVER(ORDER BY canonical)-1 canonical_position FROM temp.legacy_queue_rows) SELECT entry.id,entry.track_id,entry.canonical_position,entry.provenance,fallback.payload,row_number() OVER(ORDER BY traversal.position IS NULL,traversal.position,entry.canonical_position)-1 traversal_position FROM canonical entry LEFT JOIN temp.legacy_queue_fallback fallback ON fallback.id=entry.track_id LEFT JOIN temp.legacy_queue_traversal traversal ON traversal.id=entry.id; CREATE UNIQUE INDEX temp.legacy_queue_ready_position ON legacy_queue_ready(traversal_position);").execute(&mut *lookup).await?;
    let state = if has_table(source, "playback_state").await? {
        sqlx::query("SELECT * FROM playback_state WHERE source_id=?1")
            .bind(selected)
            .fetch_optional(&mut *source)
            .await?
    } else {
        None
    };
    let current = state
        .as_ref()
        .and_then(|row| string(row, "selected_occurrence_id"));
    let current = if let Some(id) = current {
        sqlx::query_scalar::<_, String>("SELECT id FROM temp.legacy_queue_rows WHERE id=?1")
            .bind(id)
            .fetch_optional(&mut *lookup)
            .await?
    } else {
        None
    };
    let progress = state
        .as_ref()
        .and_then(|row| integer(row, "progress_millis"))
        .unwrap_or(0)
        .max(0);
    let shuffled =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM temp.legacy_queue_traversal)")
            .fetch_one(&mut *lookup)
            .await?;
    let mut stream = tempfile::tempfile()?;
    serde_json::to_writer(
        &mut stream,
        &serde_json::json!({"version":1,"current_occurrence":current,"progress_millis":progress,"repeat_mode":"Off","shuffled":shuffled}),
    )?;
    stream.write_all(b"\n")?;
    let mut cursor = -1;
    loop {
        let row=sqlx::query("SELECT * FROM temp.legacy_queue_ready WHERE traversal_position>?1 ORDER BY traversal_position LIMIT 1").bind(cursor).fetch_optional(&mut *lookup).await?;
        let Some(row) = row else {
            break;
        };
        cursor = integer(&row, "traversal_position").unwrap_or(cursor + 1);
        let result = async {
            let id = required(&row, "id")?;
            let track_id = required(&row, "track_id")?;
            let track = match track(source, selected, &track_id).await {
                Ok(track) => track,
                Err(error) => {
                    imported::<()>(Err(error), "legacy Queue metadata");
                    None
                }
            };
            let mut item = item(
                uri(selected, "track", &track_id, track.as_ref())?,
                track.as_ref(),
            );
            if let Some(fallback) = string(&row, "payload") {
                let fallback: serde_json::Value = serde_json::from_str(&fallback)?;
                if track.is_none() {
                    item.title = fallback["title"].as_str().unwrap_or(&track_id).into();
                    item.artist = fallback["artist"].as_str().unwrap_or_default().into();
                    item.album = fallback["album"].as_str().unwrap_or_default().into();
                    item.duration_millis = fallback["duration_seconds"]
                        .as_i64()
                        .unwrap_or(0)
                        .saturating_mul(1000);
                    item.disc_number = fallback["disc_number"].as_i64();
                    item.track_number = fallback["track_number"].as_i64();
                    item.year = fallback["year"].as_i64();
                    item.source_format = fallback["source_format"].as_str().map(str::to_owned);
                    item.musicbrainz_recording_id = fallback["musicbrainz_recording_id"]
                        .as_str()
                        .map(str::to_owned);
                }
                if selected == "local:server:library" {
                    if let Some(path) = fallback["source_path"].as_str().and_then(file_uri) {
                        item.media_uri = if let (Some(_), Some(start), Some(end)) = (
                            fallback["cue"]["cue_path"].as_str(),
                            fallback["cue"]["start_millis"].as_i64(),
                            fallback["cue"]["end_millis"].as_i64(),
                        ) {
                            crate::cue_media_uri(&track_id, &path, start, end)
                        } else {
                            path
                        };
                    }
                }
            }
            let provenance = string(&row, "provenance")
                .and_then(|value| {
                    serde_json::from_str::<QueueProvenance>(&value)
                        .or_else(|_| serde_json::from_value(serde_json::Value::String(value)))
                        .ok()
                })
                .unwrap_or(QueueProvenance::Legacy);
            let occurrence = QueueOccurrence {
                occurrence: OccurrenceId::new(id),
                item,
                canonical_position: integer(&row, "canonical_position").unwrap_or(0) as usize,
                provenance,
            };
            serde_json::to_writer(&mut stream, &occurrence)?;
            stream.write_all(b"\n")?;
            Ok(())
        }
        .await;
        imported(result, "legacy Queue occurrence");
    }
    stream.rewind()?;
    database
        .import_queue_jsonl(std::io::BufReader::new(stream))
        .await
}
