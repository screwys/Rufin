//! Translates supported released formats into the ordinary user-state writers.
mod legacy;
use crate::{Database, LibraryError, LibraryResult, SourceId, source_entity_uri};
use futures_util::TryStreamExt;
use sqlx::{
    Connection, Row, SqliteConnection,
    sqlite::{SqliteConnectOptions, SqliteRow},
};
use std::collections::BTreeMap;
use std::io::{Seek, Write};
use std::path::Path;

fn imported<T>(result: LibraryResult<T>, family: &str) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, family, "could not import released user-state record");
            None
        }
    }
}

async fn reader(path: &Path) -> LibraryResult<SqliteConnection> {
    Ok(SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false)
            .pragma("trusted_schema", "OFF"),
    )
    .await?)
}

async fn has_table(connection: &mut SqliteConnection, table: &str) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
    )
    .bind(table)
    .fetch_one(connection)
    .await?)
}

fn string(row: &SqliteRow, name: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(name).ok().flatten()
}
fn integer(row: &SqliteRow, name: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(name).ok().flatten()
}

fn canonical_locator(uri: &str) -> Option<String> {
    if crate::source_entity_parts(uri).is_some() || crate::cue_media_parts(uri).is_some() {
        Some(uri.to_string())
    } else {
        crate::normalize_direct_media_uri(uri)
    }
}

async fn media_uri(
    lookup: &mut SqliteConnection,
    sources: &BTreeMap<i64, String>,
    row: &SqliteRow,
    kind: &str,
) -> LibraryResult<String> {
    let source = integer(row, "source_key")
        .and_then(|key| sources.get(&key))
        .cloned()
        .or_else(|| string(row, "source_id"))
        .unwrap_or_else(|| "rufin:recovered".into());
    let orphan_listen = source == "rufin:recovered" && integer(row, "listen_key").is_some();
    let object = if orphan_listen {
        string(row, "external_id").or_else(|| integer(row, "listen_key").map(|key| key.to_string()))
    } else {
        string(row, "track_object_id").or_else(|| string(row, "object_id"))
    }
    .or_else(|| string(row, "external_id"))
    .or_else(|| integer(row, "listen_key").map(|key| key.to_string()))
    .ok_or_else(|| LibraryError::InvalidStore("missing media locator".into()))?;
    if kind == "track" {
        let cue = string(row, "cue_path").or_else(|| string(row, "fallback_cue_path"));
        let start =
            integer(row, "cue_start_millis").or_else(|| integer(row, "fallback_cue_start_millis"));
        let end =
            integer(row, "cue_end_millis").or_else(|| integer(row, "fallback_cue_end_millis"));
        let file = string(row, "media_uri")
            .or_else(|| string(row, "fallback_media_uri"))
            .or_else(|| {
                string(row, "source_path").and_then(|path| {
                    url::Url::from_file_path(path)
                        .ok()
                        .map(|url| url.to_string())
                })
            });
        if let (Some(_), Some(start), Some(end), Some(file)) = (cue, start, end, file) {
            return Ok(crate::cue_media_uri(&object, &file, start, end));
        }
    }
    if kind == "track" && has_table(lookup, "tracks").await? {
        let track = sqlx::query("SELECT * FROM tracks WHERE source_key=?1 AND object_id=?2")
            .bind(integer(row, "source_key"))
            .bind(&object)
            .fetch_optional(&mut *lookup)
            .await?;
        if let Some(track) = track {
            if source == "local:server:library" {
                if let Some(path) = string(&track, "source_path") {
                    let uri = crate::normalize_direct_media_uri(&path).or_else(|| {
                        url::Url::from_file_path(&path)
                            .ok()
                            .map(|uri| uri.to_string())
                    });
                    if let Some(uri) = uri {
                        if let (Some(start), Some(end)) = (
                            integer(&track, "cue_start_millis"),
                            integer(&track, "cue_end_millis"),
                        ) {
                            return Ok(crate::cue_media_uri(&object, &uri, start, end));
                        }
                        return Ok(uri);
                    }
                }
            }
            if let Some(uri) = string(&track, "media_uri").and_then(|uri| canonical_locator(&uri)) {
                return Ok(uri);
            }
        }
    }
    if let Some(uri) = string(row, "media_uri")
        .and_then(|uri| canonical_locator(&uri))
        .or_else(|| crate::normalize_direct_media_uri(&object))
    {
        return Ok(uri);
    }
    Ok(source_entity_uri(&SourceId::new(source), kind, &object))
}

/// A version selects a known reader; arbitrary damaged/current databases are never salvaged.
pub(crate) async fn import_released(
    input: &Path,
    destination: &Path,
    configured: &[SourceId],
    selected: Option<&SourceId>,
) -> LibraryResult<()> {
    let mut source = reader(input).await?;
    let version = crate::schema::pragma(&mut source, "user_version").await?;
    if !(1..=43).contains(&version) {
        return Err(LibraryError::Migration(format!(
            "no released reader for schema {version}"
        )));
    }
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix("rufin-migration-")
        .tempdir_in(parent)?;
    let pending = stage.path().join("store.sqlite");
    let database = Database::open_final(&pending, &stage.path().join("catalog.sqlite")).await?;
    let mut lookup = reader(input).await?;
    if version < 41 {
        legacy::import(&mut source, &mut lookup, &database, selected).await?;
    } else {
        let sources: BTreeMap<i64, String> =
            sqlx::query_as("SELECT source_key,object_id FROM sources")
                .fetch_all(&mut source)
                .await?
                .into_iter()
                .collect();
        let mut order = Vec::new();
        for id in selected.into_iter().chain(configured.iter()) {
            if let Some(key) = sources
                .iter()
                .find_map(|(key, value)| (value == id.as_str()).then_some(*key))
            {
                if !order.contains(&key) {
                    order.push(key);
                }
            }
        }
        let order = serde_json::to_string(&order)?;
        {
            let mut writer = database.writer().await?;
            let target = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
            for id in sources.values() {
                crate::db::write_source_identity(target, id).await?;
            }
            import_playlists(&mut source, &mut lookup, target, &sources, &order, version).await;
            import_smart(&mut source, target, &order).await;
            import_user_state(&mut source, &mut lookup, target, &sources).await;
            import_listens(&mut source, &mut lookup, target, &sources).await;
            let mut history = sqlx::query_as::<_,crate::activity::LegacyActivityRecord>(
                "SELECT source.object_id source_id,activity.period,activity.item_kind,activity.track_object_id,activity.play_count,activity.skip_count,activity.last_played_at FROM activity_baseline activity JOIN sources source USING(source_key)"
            ).fetch(&mut source);
            while let Some(record) = history.try_next().await? {
                crate::activity::write_legacy_activity(target, &record).await?;
            }
            drop(history);
            import_locators(&mut source, &mut lookup, target, &sources).await;
        }
        import_queue(&mut source, &mut lookup, &database, &sources, selected).await;
    }
    source.close().await?;
    lookup.close().await?;
    database.close().await?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&pending)?
        .sync_all()?;
    if input == destination {
        crate::db::preserve_store(input)?;
    }
    std::fs::rename(&pending, destination)?;
    Ok(())
}

async fn import_playlists(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
    sources: &BTreeMap<i64, String>,
    source_order: &str,
    version: i64,
) {
    let order = if version < 43 {
        "sort_text"
    } else {
        "position"
    };
    let query = format!(
        "SELECT * FROM playlists ORDER BY COALESCE((SELECT key FROM json_each(?1) WHERE value=source_key),2147483647),{order},playlist_key"
    );
    let mut rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(source_order)
        .fetch(&mut *source);
    let mut rank = 0;
    loop {
        let row = match rows.try_next().await {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                imported::<()>(Err(error.into()), "playlists");
                break;
            }
        };
        let result=async {
            let id=string(&row,"object_id").ok_or_else(||LibraryError::InvalidStore("playlist ID unreadable".into()))?;
            let old_source=integer(&row,"source_key").and_then(|key|sources.get(&key)).cloned();
            let authored=string(&row,"ownership").as_deref()==Some("user") || old_source.is_none()
                || id.starts_with("rufin:playlist:") || old_source.as_deref()==Some("local:server:library");
            let identity=crate::PlaylistIdentity {source_id:if authored {None}else{old_source.clone()},object_id:id,
                name:if authored {string(&row,"name")}else{None},position:rank};
            let key=crate::playlists::write_playlist_identity(target,&identity).await?;
            if authored {
                let old_key=integer(&row,"playlist_key").ok_or_else(||LibraryError::InvalidStore("playlist key unreadable".into()))?;
                let mut after=-1;
                loop {
                    let entry=sqlx::query("SELECT * FROM playlist_entries WHERE playlist_key=?1 AND position>?2 ORDER BY position LIMIT 1")
                        .bind(old_key).bind(after).fetch_optional(&mut *lookup).await?;
                    let Some(entry)=entry else {break};
                    after=integer(&entry,"position").unwrap_or(after+1);
                    let entry_result=async {
                        let object=string(&entry,"track_object_id");
                        let track=if let Some(object)=&object {
                            sqlx::query("SELECT * FROM tracks WHERE source_key=?1 AND object_id=?2")
                                .bind(integer(&row,"source_key")).bind(object).fetch_optional(&mut *lookup).await.ok().flatten()
                        } else {None};
                        let uri=if let Some(uri)=string(&entry,"media_uri") {uri}
                            else if let Some(track)=&track {media_uri(lookup,sources,track,"track").await?}
                            else {source_entity_uri(&SourceId::new(old_source.clone().unwrap_or_else(||"rufin:recovered".into())),"track",&object.unwrap_or_default())};
                        let text=|name:&str,track_name:&str| string(&entry,name).or_else(||track.as_ref().and_then(|track|string(track,track_name)));
                        let number=|name:&str| integer(&entry,name).or_else(||track.as_ref().and_then(|track|integer(track,name)));
                        let write=crate::PlaylistEntryWrite {
                            object_id:string(&entry,"object_id").ok_or_else(||LibraryError::InvalidStore("occurrence ID unreadable".into()))?,media_uri:uri,
                            title:text("title","title"),artist:text("artist","display_artist"),album:text("album","display_album"),
                            album_display_artist:text("album_display_artist","album_display_artist"),snapshot_at:integer(&entry,"snapshot_at").unwrap_or(0),
                            duration_millis:number("duration_millis"),disc_number:number("disc_number"),track_number:number("track_number"),year:number("year"),
                            release_date:text("release_date","release_date"),source_format:text("source_format","source_format"),
                            musicbrainz_recording_id:text("musicbrainz_recording_id","musicbrainz_recording_id"),
                            musicbrainz_release_track_id:text("musicbrainz_release_track_id","musicbrainz_release_track_id"),position:after};
                        crate::playlists::write_playlist_entry(target,key,&write).await
                    }.await;
                    imported(entry_result,"playlist occurrence");
                }
            }
            Ok::<_,LibraryError>(())
        }.await;
        imported(result, "playlist");
        rank += 1;
    }
}

async fn import_smart(
    source: &mut SqliteConnection,
    target: &mut SqliteConnection,
    source_order: &str,
) {
    let query = "SELECT * FROM smart_playlists ORDER BY COALESCE((SELECT key FROM json_each(?1) WHERE value=source_key),2147483647),position";
    let mut rows = sqlx::query(query).bind(source_order).fetch(source);
    let mut rank = 0;
    loop {
        let row = match rows.try_next().await {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                imported::<()>(Err(error.into()), "Smart playlists");
                break;
            }
        };
        let result = async {
            let mut definition: serde_json::Value = serde_json::from_str(
                &string(&row, "definition_json")
                    .ok_or_else(|| LibraryError::InvalidStore("Smart rules unreadable".into()))?,
            )?;
            if let Some(object) = definition.as_object_mut() {
                object
                    .entry("current")
                    .or_insert(serde_json::Value::Bool(true));
                object.remove("source_id");
            }
            let record = crate::SmartPlaylistWrite {
                object_id: string(&row, "object_id")
                    .or_else(|| string(&row, "smart_playlist_id"))
                    .ok_or_else(|| {
                        LibraryError::InvalidStore("Smart identity unreadable".into())
                    })?,
                name: string(&row, "name").unwrap_or_default(),
                definition_json: serde_json::to_string(&definition)?,
                position: rank,
            };
            if sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM smart_playlists WHERE object_id=?1)",
            )
            .bind(&record.object_id)
            .fetch_one(&mut *target)
            .await?
            {
                return Ok(());
            }
            crate::smart_playlists::write_smart_playlist(target, &record)
                .await
                .map(|_| ())
        }
        .await;
        imported(result, "Smart playlist");
        rank += 1;
    }
}

async fn import_user_state(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
    sources: &BTreeMap<i64, String>,
) {
    for (table, kind) in [
        ("tracks", "track"),
        ("albums", "album"),
        ("artists", "artist"),
    ] {
        let mut rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT * FROM {table} WHERE user_favorite IS NOT NULL OR user_rating IS NOT NULL"
        )))
        .fetch(&mut *source);
        loop {
            let row = match rows.try_next().await {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(error) => {
                    imported::<()>(Err(error.into()), "user overrides");
                    break;
                }
            };
            let result = async {
                let state = crate::UserMediaStateWrite {
                    media_uri: media_uri(lookup, sources, &row, kind).await?,
                    favorite: row.try_get("user_favorite")?,
                    rating: row.try_get("user_rating")?,
                };
                crate::favorites::write_user_media_state(target, &state).await
            }
            .await;
            imported(result, "user override");
        }
    }
    if has_table(source, "favorite_outbox").await.unwrap_or(false) {
        let mut rows = sqlx::query("SELECT * FROM favorite_outbox").fetch(source);
        loop {
            let row = match rows.try_next().await {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(error) => {
                    imported::<()>(Err(error.into()), "favorite delivery");
                    break;
                }
            };
            let result=async {
                let uri=if let Some(uri)=string(&row,"media_uri") {uri} else {
                    let kind=string(&row,"entity_kind").unwrap_or_default();
                    let (table,key)=match kind.as_str(){"track"=>("tracks","track_key"),"album"=>("albums","album_key"),"artist"=>("artists","artist_key"),_=>return Err(LibraryError::InvalidStore("unknown favorite entity".into()))};
                    let entity=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {table} WHERE {key}=?1"))).bind(integer(&row,"entity_key")).fetch_one(&mut *lookup).await?;
                    media_uri(lookup,sources,&entity,&kind).await?
                };
                sqlx::query("INSERT INTO favorite_outbox(media_uri,favorite,previous_favorite,attempts,next_attempt_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(media_uri) DO NOTHING")
                    .bind(uri).bind(row.try_get::<bool,_>("favorite")?).bind(row.try_get::<bool,_>("previous_favorite")?)
                    .bind(row.try_get::<i64,_>("attempts")?).bind(row.try_get::<i64,_>("next_attempt_at")?).execute(&mut *target).await?;
                Ok(())
            }.await;
            imported(result, "favorite delivery");
        }
    }
}

async fn import_listens(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
    sources: &BTreeMap<i64, String>,
) {
    let has_delivery = has_table(lookup, "listen_outbox").await.unwrap_or(false);
    let mut rows = sqlx::query("SELECT * FROM listens ORDER BY listen_key").fetch(source);
    loop {
        let row = match rows.try_next().await {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                imported::<()>(Err(error.into()), "Activity");
                break;
            }
        };
        let result=async {
            let old_key=row.try_get::<i64,_>("listen_key")?;
            let source_id=string(&row,"source_id").or_else(||integer(&row,"source_key").and_then(|key|sources.get(&key)).cloned());
            let listen=crate::ListenWrite {
                external_id:string(&row,"external_id"),
                media_uri:media_uri(lookup,sources,&row,"track").await?,title:row.try_get("track_title")?,
                artist:row.try_get("artist_name")?,album:row.try_get("album_title")?,duration_millis:row.try_get("duration_millis")?,
                disc_number:integer(&row,"disc_number"),track_number:integer(&row,"track_number"),year:integer(&row,"year"),
                release_date:string(&row,"release_date"),source_format:string(&row,"source_format"),
                musicbrainz_recording_id:string(&row,"musicbrainz_recording_id"),musicbrainz_release_track_id:string(&row,"musicbrainz_release_track_id"),
                started_at:row.try_get("started_at")?,local_period:row.try_get("local_period")?,listened_millis:row.try_get("listened_millis")?,skipped:row.try_get("skipped")?};
            let key=crate::activity::write_imported_listen(target,&listen,source_id.as_deref(),Some(old_key)).await?;
            if has_delivery {
                let mut deliveries=sqlx::query("SELECT * FROM listen_outbox WHERE listen_key=?1").bind(old_key).fetch(&mut *lookup);
                while let Some(delivery)=deliveries.try_next().await? {
                    let result=async {
                        sqlx::query("INSERT INTO listen_outbox(listen_key,service,account_id,attempts,next_attempt_at,last_error)
                            VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(service,account_id,listen_key) DO NOTHING")
                            .bind(key).bind(delivery.try_get::<String,_>("service")?).bind(delivery.try_get::<String,_>("account_id")?)
                            .bind(delivery.try_get::<i64,_>("attempts")?).bind(delivery.try_get::<Option<i64>,_>("next_attempt_at")?)
                            .bind(delivery.try_get::<Option<String>,_>("last_error")?).execute(&mut *target).await?;
                        Ok(())
                    }.await;
                    imported(result,"listen delivery");
                }
            }
            Ok(())
        }.await;
        imported(result, "accepted listen");
    }
}

async fn import_locators(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    target: &mut SqliteConnection,
    sources: &BTreeMap<i64, String>,
) {
    let mut rows = sqlx::query("SELECT * FROM local_access_files").fetch(source);
    loop {
        let row = match rows.try_next().await {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                imported::<()>(Err(error.into()), "Local locators");
                break;
            }
        };
        let result = async {
            let source_id = integer(&row, "source_key").and_then(|key| sources.get(&key));
            let path = string(&row, "path")
                .ok_or_else(|| LibraryError::InvalidStore("Local path unreadable".into()))?;
            let access = string(&row, "access_uri")
                .or_else(|| string(&row, "media_uri"))
                .or_else(|| {
                    url::Url::from_file_path(&path)
                        .ok()
                        .map(|uri| uri.to_string())
                })
                .ok_or_else(|| LibraryError::InvalidStore("Local access unreadable".into()))?;
            let uri = if source_id.is_some_and(|id| id == "local:server:library") {
                media_uri(lookup, sources, &row, "track").await?
            } else if let (Some(object), Some(source_id)) =
                (string(&row, "track_object_id"), source_id)
            {
                source_entity_uri(&SourceId::new(source_id), "track", &object)
            } else {
                crate::normalize_direct_media_uri(&access).unwrap_or_else(|| access.clone())
            };
            crate::local::write_local_locator(
                target,
                &crate::LocalLocatorWrite {
                    source_id: source_id.cloned(),
                    media_uri: uri,
                    origin: string(&row, "origin").unwrap_or_else(|| "mapping".into()),
                    path,
                    root: string(&row, "root").unwrap_or_default(),
                    relative_path: string(&row, "relative_path").unwrap_or_default(),
                    access_uri: access,
                },
            )
            .await?;
            Ok(())
        }
        .await;
        imported(result, "Local locator");
    }
}

async fn import_queue(
    source: &mut SqliteConnection,
    lookup: &mut SqliteConnection,
    database: &Database,
    sources: &BTreeMap<i64, String>,
    selected: Option<&SourceId>,
) {
    let result = async {
        let selected_key = selected.and_then(|id| {
            sources
                .iter()
                .find(|(_, source)| source.as_str() == id.as_str())
                .map(|(key, _)| *key)
        });
        let state_rows = sqlx::query("SELECT * FROM queue_state")
            .fetch_all(&mut *source)
            .await?;
        let state = state_rows.iter().find(|row| {
            integer(row, "source_key") == selected_key
        });
        let Some(state) = state else { return Ok(()) };
        let current = if let Some(id) = string(state, "current_occurrence_id") {
            Some(id)
        } else if let Some(key) = integer(state, "current_occurrence_key") {
            sqlx::query_scalar::<_, String>(
                "SELECT object_id FROM queue_occurrences WHERE queue_occurrence_key=?1",
            )
            .bind(key)
            .fetch_optional(&mut *lookup)
            .await?
        } else {
            None
        };
        sqlx::raw_sql("DROP TABLE IF EXISTS temp.queue_survivors;CREATE TEMP TABLE queue_survivors(object_id TEXT PRIMARY KEY,canonical_position INTEGER NOT NULL UNIQUE,traversal_position INTEGER NOT NULL UNIQUE,payload TEXT NOT NULL);").execute(&mut *lookup).await?;
        let mut traversal = 0_i64;
        let mut file = tempfile::tempfile()?;
        let repeat = match string(state, "repeat_mode").as_deref() {
            Some("one") => "One",
            Some("all") => "All",
            _ => "Off",
        };
        let sql = "SELECT * FROM queue_occurrences WHERE source_key=?1 ORDER BY traversal_position";
        let mut rows = sqlx::query(sql).bind(selected_key).fetch(&mut *source);
        loop {
            let row = match rows.try_next().await {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(error) => {
                    imported::<()>(Err(error.into()), "Queue occurrences");
                    break;
                }
            };
            let occurrence = async {
                integer(&row,"traversal_position").filter(|value|*value>=0).ok_or_else(||LibraryError::InvalidStore("Queue traversal position unreadable".into()))?;
                let uri = media_uri(lookup, sources, &row, "track").await?;
                let text = |name: &str| {
                    string(&row, name).or_else(|| string(&row, &format!("fallback_{name}")))
                };
                let number = |name: &str| {
                    integer(&row, name).or_else(|| integer(&row, &format!("fallback_{name}")))
                };
                let item = crate::QueueItem {
                    media_uri: uri,
                    title: text("title").unwrap_or_default(),
                    artist: text("artist").unwrap_or_default(),
                    album: text("album").unwrap_or_default(),
                    album_display_artist: text("album_display_artist"),
                    artwork_binding: None,
                    duration_millis: number("duration_millis").unwrap_or(0).max(0),
                    disc_number: number("disc_number").filter(|value| *value >= 0),
                    track_number: number("track_number").filter(|value| *value >= 0),
                    year: number("year"),
                    release_date: text("release_date"),
                    source_format: text("source_format"),
                    musicbrainz_recording_id: text("musicbrainz_recording_id").filter(|value| !value.is_empty()),
                    musicbrainz_release_track_id: text("musicbrainz_release_track_id").filter(|value| !value.is_empty()),
                    musicbrainz_album_id: text("musicbrainz_album_id").filter(|value| !value.is_empty()),
                    musicbrainz_release_group_id: text("musicbrainz_release_group_id").filter(|value| !value.is_empty()),
                    primary_artist_musicbrainz_id: text("primary_artist_musicbrainz_id").filter(|value| !value.is_empty()),
                };
                let provenance = match string(&row, "provenance_kind").as_deref() {
                    Some("context") => crate::QueueProvenance::Context {
                        context_id: string(&row, "provenance_context_id")
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| LibraryError::InvalidStore("Queue context identity unreadable".into()))?
                            .into(),
                        source_rank: integer(&row, "provenance_source_rank").filter(|value| *value >= 0)
                            .ok_or_else(|| LibraryError::InvalidStore("Queue context rank unreadable".into()))? as usize,
                    },
                    Some("manual") => crate::QueueProvenance::Manual,
                    Some("random") => crate::QueueProvenance::Random,
                    Some("radio") => crate::QueueProvenance::Radio,
                    Some("auto-dj") => crate::QueueProvenance::AutoDj,
                    _ => crate::QueueProvenance::Legacy,
                };
                let id = string(&row, "object_id")
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        LibraryError::InvalidStore("Queue occurrence ID unreadable".into())
                    })?;
                Ok(crate::QueueOccurrence {
                    source_index:None,playlist_entry_id:None,
                    occurrence: crate::OccurrenceId::new(id),
                    item,
                    canonical_position: integer(&row,"position").filter(|position|*position>=0).ok_or_else(||LibraryError::InvalidStore("Queue position unreadable".into()))? as usize,
                    provenance,
                })
            }
            .await;
            if let Some(occurrence) = imported(occurrence, "Queue occurrence") {
                let inserted=sqlx::query("INSERT INTO temp.queue_survivors VALUES(?1,?2,?3,?4)")
                    .bind(occurrence.occurrence.as_str()).bind(occurrence.canonical_position as i64).bind(traversal)
                    .bind(serde_json::to_string(&occurrence)?).execute(&mut *lookup).await;
                if imported(inserted.map_err(Into::into),"Queue survivor").is_some() {traversal+=1;}

            }
        }
        drop(rows);
        let current=if let Some(current)=current {
            sqlx::query_scalar::<_,String>("SELECT object_id FROM temp.queue_survivors WHERE object_id=?1").bind(current).fetch_optional(&mut *lookup).await?
        } else {None};
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({"version":1,"current_occurrence":current,
            "progress_millis":if current.is_some() {integer(state,"progress_millis").unwrap_or(0).max(0)} else {0},"repeat_mode":repeat,
            "shuffled":integer(state,"shuffled").unwrap_or(0)!=0}),
        )?;
        file.write_all(b"\n")?;
        let mut survivors=sqlx::query_scalar::<_,String>("SELECT json_set(payload,'$.canonical_position',rank) FROM (SELECT payload,traversal_position,row_number() OVER(ORDER BY canonical_position)-1 rank FROM temp.queue_survivors) ORDER BY traversal_position").fetch(&mut *lookup);
        while let Some(payload)=survivors.try_next().await? {file.write_all(payload.as_bytes())?;file.write_all(b"\n")?;}
        drop(survivors);
        file.rewind()?;
        database
            .import_queue_jsonl(std::io::BufReader::new(file))
            .await
    }
    .await;
    imported(result, "Queue");
}
