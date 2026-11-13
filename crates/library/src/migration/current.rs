//! The one conversion of a released 0.15.0-0.16.6 Store and its catalog file, and the salvage
//! of a damaged or newer Store.
use crate::catalog::records::{added, picture_flag, release};
use crate::{LibraryError, LibraryResult, SourceId, source_entity_uri};
use sqlx::{Connection, Row, SqliteConnection};
use std::path::Path;

async fn attach(connection: &mut SqliteConnection, path: &Path, alias: &str) -> LibraryResult<()> {
    let mut uri = url::Url::from_file_path(path.canonicalize()?)
        .map_err(|_| LibraryError::Migration("database path is not absolute".into()))?;
    uri.set_query(Some("mode=ro"));
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ATTACH DATABASE ?1 AS {alias}"
    )))
    .bind(uri.as_str())
    .execute(connection)
    .await?;
    Ok(())
}

async fn columns(
    connection: &mut SqliteConnection,
    schema: &str,
    table: &str,
) -> LibraryResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?1,?2)")
            .bind(table)
            .bind(schema)
            .fetch_all(connection)
            .await?,
    )
}

async fn has_table(
    connection: &mut SqliteConnection,
    schema: &str,
    table: &str,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_schema WHERE type='table' AND name=?1)"
    )))
    .bind(table)
    .fetch_one(connection)
    .await?)
}

/// Copies the rows of a table that pass `filter` into the columns the target shares with it.
async fn copy_table(
    connection: &mut SqliteConnection,
    schema: &str,
    from: &str,
    to: &str,
    overrides: &[(&str, &str)],
    filter: &str,
) -> LibraryResult<()> {
    let old = columns(connection, schema, from).await?;
    if old.is_empty() {
        return Ok(());
    }
    let target = columns(connection, "main", to).await?;
    let selected: Vec<_> = target
        .iter()
        .filter_map(|name| {
            overrides
                .iter()
                .find(|(field, _)| *field == name)
                .map(|(_, expression)| (name.clone(), expression.to_string()))
                .or_else(|| {
                    old.contains(name)
                        .then(|| (name.clone(), format!("old.\"{name}\"")))
                })
        })
        .collect();
    let names = selected
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    let expressions = selected
        .iter()
        .map(|(_, expression)| expression.as_str())
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO main.{to}({names}) SELECT {expressions} FROM {schema}.{from} old WHERE {filter}"
    )))
    .execute(connection)
    .await?;
    Ok(())
}

/// A salvaged row whose reference names no row loses the reference, or the row itself when
/// the reference is required. Removing a row can leave its own children dangling, so this
/// repeats until nothing dangles.
async fn drop_dangling(connection: &mut SqliteConnection) -> LibraryResult<()> {
    loop {
        let violations: Vec<(String, i64)> =
            sqlx::query_as("SELECT DISTINCT \"table\",fkid FROM pragma_foreign_key_check")
                .fetch_all(&mut *connection)
                .await?;
        if violations.is_empty() {
            return Ok(());
        }
        for (table, id) in violations {
            let references: Vec<(String, Option<String>, String)> = sqlx::query_as(
                "SELECT \"from\",\"to\",\"table\" FROM pragma_foreign_key_list(?1) WHERE id=?2 ORDER BY seq",
            )
            .bind(&table)
            .bind(id)
            .fetch_all(&mut *connection)
            .await?;
            let Some((_, _, parent)) = references.first().cloned() else {
                continue;
            };
            let keys: Vec<String> =
                sqlx::query_scalar("SELECT name FROM pragma_table_info(?1) WHERE pk>0 ORDER BY pk")
                    .bind(&parent)
                    .fetch_all(&mut *connection)
                    .await?;
            let mut present = Vec::new();
            let mut set = Vec::new();
            let mut required = false;
            for (index, (from, to, _)) in references.iter().enumerate() {
                let to = to.clone().or_else(|| keys.get(index).cloned());
                let to = to.as_deref().unwrap_or("rowid");
                present.push(format!("target.\"{to}\"=child.\"{from}\""));
                set.push(format!("\"{from}\"=NULL"));
                required |= sqlx::query_scalar::<_, bool>(
                    "SELECT \"notnull\" FROM pragma_table_info(?1) WHERE name=?2",
                )
                .bind(&table)
                .bind(from)
                .fetch_one(&mut *connection)
                .await?;
            }
            let assigned = references
                .iter()
                .map(|(from, _, _)| format!("child.\"{from}\" IS NOT NULL"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let dangling = format!(
                "{assigned} AND NOT EXISTS(SELECT 1 FROM main.\"{parent}\" target WHERE {})",
                present.join(" AND ")
            );
            let statement = if required {
                format!("DELETE FROM main.\"{table}\" AS child WHERE {dangling}")
            } else {
                format!(
                    "UPDATE main.\"{table}\" AS child SET {} WHERE {dangling}",
                    set.join(",")
                )
            };
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&mut *connection)
                .await?;
        }
    }
}

/// Rebuilds a damaged or newer Store in place, in one transaction: every table takes this
/// schema's shape and keeps the columns it shares with it.
pub(crate) async fn recover_columns(path: &Path) -> LibraryResult<()> {
    let mut connection = crate::db::open_writer(path).await?;
    let recovered = async {
        crate::lyrics_tokenizer::register(&mut connection).await?;
        sqlx::query("PRAGMA foreign_keys=OFF").execute(&mut connection).await?;
        let mut transaction = connection.begin().await?;
        // Views, triggers and indexes come from the schema; the lyrics search rows are indexed
        // again from the copied lyrics.
        let derived: Vec<(String, String)> = sqlx::query_as("SELECT type,name FROM sqlite_schema WHERE type IN ('view','trigger') OR (type='index' AND sql IS NOT NULL)")
            .fetch_all(&mut *transaction).await?;
        for (kind, name) in derived {
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP {kind} IF EXISTS \"{name}\"")))
                .execute(&mut *transaction).await?;
        }
        sqlx::query("DROP TABLE IF EXISTS lyrics_fts").execute(&mut *transaction).await?;
        let old: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")
            .fetch_all(&mut *transaction).await?;
        for table in &old {
            sqlx::query(sqlx::AssertSqlSafe(format!("ALTER TABLE \"{table}\" RENAME TO \"recovered_{table}\"")))
                .execute(&mut *transaction).await?;
        }
        sqlx::query("PRAGMA user_version=0").execute(&mut *transaction).await?;
        crate::schema::initialize_durable(&mut transaction).await?;
        for table in &old {
            if has_table(&mut transaction, "main", table).await? {
                sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM main.\"{table}\"")))
                    .execute(&mut *transaction).await?;
                copy_table(&mut transaction, "main", &format!("recovered_{table}"), table, &[], "true").await?;
            }
        }
        if old.iter().any(|table| table == "legacy_activity") {
            play_counts_of_0_17(&mut transaction).await?;
        }
        playlist_folders_of_0_17(&mut transaction).await?;
        // 0.17 kept MusicBrainz's release types per album.
        if old.iter().any(|table| table == "album_release_observations") {
            sqlx::query("INSERT OR IGNORE INTO musicbrainz_release_types(identity,release_types) SELECT lookup_identity,release_types FROM recovered_album_release_observations")
                .execute(&mut *transaction).await?;
        }
        for table in &old {
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE \"recovered_{table}\"")))
                .execute(&mut *transaction).await?;
        }
        drop_dangling(&mut transaction).await?;
        crate::lyrics::index_all_lyrics(&mut transaction).await?;
        transaction.commit().await?;
        LibraryResult::Ok(())
    }.await;
    connection.close().await?;
    recovered
}

/// A 0.17 Store kept its counts from outside Rufin by source and object id, and the 0.16
/// baselines of file-library tracks as native counts, which those tracks never show. Both
/// become play counts.
async fn play_counts_of_0_17(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let counts = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} FROM recovered_legacy_activity",
        crate::activity::RELEASED_COUNTS
    )))
    .fetch_all(&mut *connection)
    .await?;
    crate::activity::carry_released_counts(connection, counts).await?;
    let files = "source_key IN(SELECT source_key FROM sources WHERE kind IN('local','webdav','smb')) AND native_play_count IS NOT NULL";
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO play_counts(media_uri,period,plays,skips,last_played_at)
        SELECT media_uri,'lifetime',native_play_count,0,native_last_played FROM tracks WHERE {files} {}", crate::activity::ADD_PLAY_COUNTS)))
        .execute(&mut *connection)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE tracks SET native_play_count=NULL,native_last_played=NULL WHERE {files}"
    )))
    .execute(&mut *connection)
    .await?;
    Ok(())
}

/// A 0.17 playlist or smart playlist named its one folder in its own row.
async fn playlist_folders_of_0_17(connection: &mut SqliteConnection) -> LibraryResult<()> {
    for (table, members, key) in [
        ("playlists", "playlist_folder_playlists", "playlist_key"),
        (
            "smart_playlists",
            "playlist_folder_smart_playlists",
            "smart_playlist_key",
        ),
    ] {
        let recovered = format!("recovered_{table}");
        if columns(connection, "main", &recovered)
            .await?
            .iter()
            .any(|column| column == "playlist_folder_key")
        {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT OR IGNORE INTO {members}({key},folder_key) SELECT {key},playlist_folder_key FROM \"{recovered}\"
                 WHERE playlist_folder_key IN(SELECT folder_key FROM playlist_folders) AND {key} IN(SELECT {key} FROM {table})"
            )))
            .execute(&mut *connection)
            .await?;
        }
    }
    Ok(())
}

/// One released entity row with the credits and tags its edges name.
struct Released {
    row: sqlx::sqlite::SqliteRow,
    key: i64,
}

fn text(row: &sqlx::sqlite::SqliteRow, name: &str) -> Option<String> {
    super::string(row, name)
}

fn number(row: &sqlx::sqlite::SqliteRow, name: &str) -> Option<i64> {
    super::integer(row, name)
}

/// Reads a released entity table one page at a time, in key order.
async fn released_page(
    connection: &mut SqliteConnection,
    table: &str,
    key: &str,
    after: i64,
) -> LibraryResult<Vec<Released>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT old.*,mapping.source_key migration_source_key,source.object_id migration_source FROM old_catalog.{table} old JOIN old_catalog.sources source USING(source_key) JOIN migration_source_keys mapping ON mapping.old_key=old.source_key WHERE old.{key}>?1 ORDER BY old.{key} LIMIT 256")))
        .bind(after)
        .fetch_all(&mut *connection)
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Released {
                key: row.try_get(key)?,
                row,
            })
        })
        .collect()
}

/// A released row's media URI. Genres, moods and folders had none; their URI follows from the
/// source and object identity.
fn released_uri(row: &sqlx::sqlite::SqliteRow, kind: &str) -> LibraryResult<String> {
    if let Some(uri) = text(row, "media_uri") {
        return Ok(uri);
    }
    let source = SourceId::new(row.try_get::<String, _>("migration_source")?);
    Ok(source_entity_uri(
        &source,
        kind,
        &row.try_get::<String, _>("object_id")?,
    ))
}

/// The released artwork binding in the current shape.
fn binding(row: &sqlx::sqlite::SqliteRow) -> LibraryResult<Option<Vec<u8>>> {
    row.try_get::<Option<Vec<u8>>, _>("artwork_binding")
        .ok()
        .flatten()
        .map(|bytes| crate::normalize_artwork_binding(&bytes))
        .transpose()
}

async fn credits(
    connection: &mut SqliteConnection,
    relation: &str,
    owner: &str,
    keys: &str,
) -> LibraryResult<std::collections::HashMap<i64, Vec<crate::ArtistCredit>>> {
    let mut result = std::collections::HashMap::<i64, Vec<crate::ArtistCredit>>::new();
    if !has_table(connection, "old_catalog", relation).await? {
        return Ok(result);
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT credit.{owner} owner,artist.object_id,artist.name,artist.sort_text,artist.musicbrainz_artist_id FROM old_catalog.{relation} credit JOIN old_catalog.artists artist USING(artist_key) WHERE credit.{owner} IN(SELECT value FROM json_each(?1)) ORDER BY credit.{owner},credit.position")))
        .bind(keys)
        .fetch_all(connection)
        .await?;
    for row in rows {
        result
            .entry(row.try_get("owner")?)
            .or_default()
            .push(crate::ArtistCredit {
                object_id: row.try_get("object_id")?,
                name: row.try_get("name")?,
                sort_text: text(&row, "sort_text"),
                musicbrainz_artist_id: text(&row, "musicbrainz_artist_id"),
            });
    }
    Ok(result)
}

async fn tags(
    connection: &mut SqliteConnection,
    relation: &str,
    owner: &str,
    kind: &str,
    keys: &str,
) -> LibraryResult<std::collections::HashMap<i64, Vec<crate::TagReference>>> {
    let mut result = std::collections::HashMap::<i64, Vec<crate::TagReference>>::new();
    if !has_table(connection, "old_catalog", relation).await? {
        return Ok(result);
    }
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT credit.{owner} owner,item.object_id,item.name,item.sort_text FROM old_catalog.{relation} credit JOIN old_catalog.{kind}s item USING({kind}_key) WHERE credit.{owner} IN(SELECT value FROM json_each(?1)) ORDER BY credit.{owner},credit.position")))
        .bind(keys)
        .fetch_all(connection)
        .await?;
    for row in rows {
        result
            .entry(row.try_get("owner")?)
            .or_default()
            .push(crate::TagReference {
                object_id: row.try_get("object_id")?,
                name: row.try_get("name")?,
                sort_text: text(&row, "sort_text"),
            });
    }
    Ok(result)
}

/// Gains the files or the server reported. Rufin's own analysis results are not carried over;
/// they are measured again. Each gain keeps the analysis key it was measured for, so a replay
/// gain joins the loudness of the same audio.
async fn gains(
    connection: &mut SqliteConnection,
    kind: &str,
    keys: &str,
) -> LibraryResult<std::collections::HashMap<i64, (Vec<u8>, crate::NativeGain)>> {
    let mut result = std::collections::HashMap::new();
    if has_table(connection, "old_catalog", "loudness_measurements").await? {
        let origin = columns(connection, "old_catalog", "loudness_measurements")
            .await?
            .iter()
            .any(|column| column == "origin");
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT entity_key,analysis_key,integrated_lufs,true_peak FROM old_catalog.loudness_measurements WHERE entity_kind=?1 AND entity_key IN(SELECT value FROM json_each(?2)){}", if origin { " AND origin='source'" } else { "" })))
            .bind(kind)
            .bind(keys)
            .fetch_all(&mut *connection)
            .await?;
        for row in rows {
            result.insert(
                row.try_get("entity_key")?,
                (
                    row.try_get("analysis_key")?,
                    crate::NativeGain {
                        integrated_lufs: row.try_get("integrated_lufs")?,
                        true_peak: row.try_get("true_peak")?,
                        ..Default::default()
                    },
                ),
            );
        }
    }
    if has_table(connection, "old_catalog", "replay_gain_measurements").await? {
        let rows = sqlx::query("SELECT entity_key,analysis_key,gain_db,peak FROM old_catalog.replay_gain_measurements WHERE entity_kind=?1 AND entity_key IN(SELECT value FROM json_each(?2))")
            .bind(kind)
            .bind(keys)
            .fetch_all(connection)
            .await?;
        for row in rows {
            let entry = result
                .entry(row.try_get("entity_key")?)
                .or_insert_with(|| (row.get("analysis_key"), crate::NativeGain::default()));
            if entry.0 == row.get::<Vec<u8>, _>("analysis_key") {
                entry.1.replay_gain_db = row.try_get("gain_db")?;
                entry.1.replay_gain_peak = row.try_get("peak")?;
            }
        }
    }
    Ok(result)
}

/// The gain a released row reported.
fn gain(
    gains: &mut std::collections::HashMap<i64, (Vec<u8>, crate::NativeGain)>,
    entity: &Released,
) -> Option<crate::NativeGain> {
    gains.remove(&entity.key).map(|(_, gain)| gain)
}

fn keys(page: &[Released]) -> LibraryResult<String> {
    Ok(serde_json::to_string(
        &page.iter().map(|entity| entity.key).collect::<Vec<_>>(),
    )?)
}

/// Artists, tags and folders: a name, a sort text and their own picture.
async fn copy_named(
    connection: &mut SqliteConnection,
    table: &str,
    key: &str,
    kind: &str,
) -> LibraryResult<()> {
    if !has_table(connection, "old_catalog", table).await? {
        return Ok(());
    }
    // Moods follow the genres in the one tags key space.
    let offset: i64 = if table == "moods" {
        sqlx::query_scalar("SELECT coalesce(max(tag_key),0) FROM tags")
            .fetch_one(&mut *connection)
            .await?
    } else {
        0
    };
    let mut after = 0;
    loop {
        let page = released_page(connection, table, key, after).await?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = last.key;
        for entity in page {
            let row = &entity.row;
            let object: String = row.try_get("object_id")?;
            let uri = released_uri(row, kind)?;
            let name = text(row, "name").unwrap_or_default();
            let artwork = binding(row)?;
            let source_key: i64 = row.try_get("migration_source_key")?;
            match table {
                "artists" => {
                    let musicbrainz = text(row, "musicbrainz_artist_id");
                    let artwork = crate::catalog::artwork::artist_binding(
                        &name,
                        musicbrainz.as_deref(),
                        artwork.as_deref(),
                    )?;
                    sqlx::query("INSERT INTO artists(artist_key,source_key,object_id,media_uri,indexed,native_metadata,name,sort_text,musicbrainz_artist_id,native_favorite,native_rating,native_artwork,flags) VALUES (?1,?2,?3,?4,1,1,?5,?6,?7,?8,?9,?10,?11)")
                        .bind(entity.key).bind(source_key).bind(&object).bind(uri).bind(&name)
                        .bind(text(row, "sort_text")).bind(musicbrainz)
                        .bind(number(row, "source_favorite")).bind(number(row, "source_rating"))
                        .bind(&artwork).bind(picture_flag(Some(&artwork)))
                        .execute(&mut *connection).await?;
                }
                "folders" => {
                    sqlx::query("INSERT INTO folders(folder_key,source_key,object_id,media_uri,indexed,native_metadata,name,sort_text,native_artwork,flags) VALUES (?1,?2,?3,?4,1,1,?5,?6,?7,?8)")
                        .bind(entity.key).bind(source_key).bind(&object).bind(uri).bind(&name)
                        .bind(text(row, "sort_text")).bind(&artwork).bind(picture_flag(artwork.as_deref()))
                        .execute(&mut *connection).await?;
                }
                _ => {
                    sqlx::query("INSERT INTO tags(tag_key,source_key,kind,object_id,media_uri,indexed,native_metadata,name,sort_text,native_artwork,flags) VALUES(?1,?2,?3,?4,?5,1,1,?6,?7,?8,?9)")
                        .bind(entity.key + offset).bind(source_key)
                        .bind(i64::from(table == "moods")).bind(&object).bind(uri).bind(&name)
                        .bind(text(row, "sort_text")).bind(&artwork).bind(picture_flag(artwork.as_deref()))
                        .execute(&mut *connection).await?;
                }
            }
        }
    }
}

async fn copy_albums(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let release_types = has_table(connection, "old_catalog", "album_release_types").await?;
    let mut after = 0;
    loop {
        let page = released_page(connection, "albums", "album_key", after).await?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = last.key;
        let keys = keys(&page)?;
        let mut artists = credits(connection, "album_artists", "album_key", &keys).await?;
        let mut genres = tags(connection, "album_genres", "album_key", "genre", &keys).await?;
        for entity in page {
            let row = &entity.row;
            let release_types: Vec<String> = if release_types {
                sqlx::query_scalar("SELECT release_type FROM old_catalog.album_release_types WHERE album_key=?1 ORDER BY position")
                    .bind(entity.key).fetch_all(&mut *connection).await?
            } else {
                Vec::new()
            };
            let metadata = crate::AlbumMetadata {
                title: text(row, "title").unwrap_or_default(),
                artist: text(row, "display_artist").unwrap_or_default(),
                sort_text: text(row, "sort_text").unwrap_or_default(),
                year: number(row, "year"),
                release_date: text(row, "release_date"),
                date_added: text(row, "date_added"),
                musicbrainz_release_id: text(row, "musicbrainz_release_id"),
                musicbrainz_release_group_id: text(row, "musicbrainz_release_group_id"),
                is_compilation: number(row, "is_compilation").map(|value| value != 0),
                album_artists: artists.remove(&entity.key).unwrap_or_default(),
                genres: genres.remove(&entity.key).unwrap_or_default(),
                release_types,
            };
            let artwork = binding(row)?;
            let first_seen = number(row, "first_seen_at");
            sqlx::query("INSERT INTO albums(album_key,source_key,object_id,media_uri,indexed,native_metadata,title,display_artist,sort_text,year,release_date,date_added,is_compilation,first_seen_at,native_favorite,native_rating,metadata,input_metadata,native_artwork,released,added_at,flags)
                VALUES(?1,?2,?3,?4,1,1,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,jsonb(?15),jsonb(?16),?17,?18,?19,?20)")
                .bind(entity.key)
                .bind(row.try_get::<i64, _>("migration_source_key")?)
                .bind(row.try_get::<String, _>("object_id")?)
                .bind(released_uri(row, "album")?)
                .bind(&metadata.title)
                .bind(&metadata.artist)
                .bind(&metadata.sort_text)
                .bind(metadata.year)
                .bind(&metadata.release_date)
                .bind(&metadata.date_added)
                .bind(metadata.is_compilation)
                .bind(first_seen)
                .bind(number(row, "source_favorite"))
                .bind(number(row, "source_rating"))
                .bind(metadata.details_json()?)
                .bind(serde_json::to_string(&metadata)?)
                .bind(&artwork)
                .bind(i64::from(release(metadata.release_date.as_deref())))
                .bind(added(metadata.date_added.as_deref(), first_seen))
                .bind(picture_flag(artwork.as_deref()))
                .execute(&mut *connection)
                .await?;
        }
    }
}

async fn copy_tracks(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut after = 0;
    loop {
        let page = released_page(connection, "tracks", "track_key", after).await?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = last.key;
        let keys = keys(&page)?;
        let mut artists = credits(connection, "track_artists", "track_key", &keys).await?;
        let mut album_artists =
            credits(connection, "track_album_artists", "track_key", &keys).await?;
        let mut genres = tags(connection, "track_genres", "track_key", "genre", &keys).await?;
        let mut moods = tags(connection, "track_moods", "track_key", "mood", &keys).await?;
        let mut gains = gains(connection, "track", &keys).await?;
        for entity in page {
            let row = &entity.row;
            let uri = released_uri(row, "track")?;
            let metadata = crate::TrackMetadata {
                title: text(row, "title").unwrap_or_else(|| uri.clone()),
                artist: text(row, "display_artist").unwrap_or_default(),
                album: text(row, "display_album").unwrap_or_default(),
                album_display_artist: text(row, "album_display_artist"),
                sort_text: text(row, "sort_text").unwrap_or_default(),
                duration_millis: number(row, "duration_millis").unwrap_or_default(),
                disc_number: number(row, "disc_number").unwrap_or_default(),
                track_number: number(row, "track_number").unwrap_or_default(),
                year: number(row, "year"),
                release_date: text(row, "release_date"),
                date_added: text(row, "date_added"),
                source_format: text(row, "source_format"),
                audio_properties: text(row, "audio_properties")
                    .and_then(|text| serde_json::from_str(&text).ok())
                    .unwrap_or_default(),
                comment: text(row, "comment"),
                bpm: number(row, "bpm"),
                musicbrainz_recording_id: text(row, "musicbrainz_recording_id"),
                musicbrainz_release_track_id: text(row, "musicbrainz_release_track_id"),
                musicbrainz_album_id: text(row, "musicbrainz_album_id"),
                musicbrainz_release_group_id: text(row, "musicbrainz_release_group_id"),
                primary_artist_musicbrainz_id: text(row, "primary_artist_musicbrainz_id"),
                artists: artists.remove(&entity.key).unwrap_or_default(),
                album_artists: album_artists.remove(&entity.key).unwrap_or_default(),
                genres: genres.remove(&entity.key).unwrap_or_default(),
                moods: moods.remove(&entity.key).unwrap_or_default(),
                source_loudness: gain(&mut gains, &entity),
                ..Default::default()
            };
            let artwork = binding(row)?;
            let cue_path = text(row, "cue_path");
            let flags = picture_flag(artwork.as_deref())
                | i64::from(cue_path.is_some()) * crate::schema::TRACK_CUE;
            let first_seen = number(row, "first_seen_at");
            sqlx::query("INSERT INTO tracks(track_key,source_key,object_id,media_uri,album_key,indexed,native_metadata,title,display_artist,display_album,sort_text,duration_millis,disc_number,track_number,year,release_date,date_added,first_seen_at,bpm,comment,source_format,flags,native_favorite,native_rating,released,added_at)
                VALUES(?1,?2,?3,?4,?5,1,1,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24)")
                .bind(entity.key)
                .bind(row.try_get::<i64, _>("migration_source_key")?)
                .bind(row.try_get::<String, _>("object_id")?)
                .bind(&uri)
                .bind(number(row, "album_key"))
                .bind(&metadata.title)
                .bind(&metadata.artist)
                .bind(&metadata.album)
                .bind(&metadata.sort_text)
                .bind(metadata.duration_millis)
                .bind(metadata.disc_number)
                .bind(metadata.track_number)
                .bind(metadata.year)
                .bind(&metadata.release_date)
                .bind(&metadata.date_added)
                .bind(first_seen)
                .bind(metadata.bpm)
                .bind(&metadata.comment)
                .bind(&metadata.source_format)
                .bind(flags)
                .bind(number(row, "source_favorite"))
                .bind(number(row, "source_rating"))
                .bind(i64::from(release(metadata.release_date.as_deref())))
                .bind(added(metadata.date_added.as_deref(), first_seen))
                .execute(&mut *connection)
                .await?;
            sqlx::query("INSERT INTO track_details(track_key,metadata,native_artwork,source_path,cue_path,cue_start_millis,cue_end_millis) VALUES(?1,jsonb(?2),?3,?4,?5,?6,?7)")
                .bind(entity.key)
                .bind(metadata.details_json()?)
                .bind(artwork)
                .bind(text(row, "source_path"))
                .bind(cue_path)
                .bind(number(row, "cue_start_millis"))
                .bind(number(row, "cue_end_millis"))
                .execute(&mut *connection)
                .await?;
        }
    }
}

async fn copy_links(connection: &mut SqliteConnection, mood_offset: i64) -> LibraryResult<()> {
    use crate::schema::{LINK_ALBUM_ARTIST, LINK_ARTIST, LINK_FOLDER, LINK_GENRE, LINK_MOOD};
    let track = ("track_key", "tracks", "track_links");
    let album = ("album_key", "albums", "album_links");
    for (relation, (owner, owners, links), target, role, offset) in [
        ("track_artists", track, "artist_key", LINK_ARTIST, 0),
        (
            "track_album_artists",
            track,
            "artist_key",
            LINK_ALBUM_ARTIST,
            0,
        ),
        ("track_genres", track, "genre_key", LINK_GENRE, 0),
        ("track_moods", track, "mood_key", LINK_MOOD, mood_offset),
        ("track_folders", track, "folder_key", LINK_FOLDER, 0),
        ("album_artists", album, "artist_key", LINK_ARTIST, 0),
        ("album_genres", album, "genre_key", LINK_GENRE, 0),
    ] {
        if !has_table(connection, "old_catalog", relation).await? {
            continue;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO {links} SELECT {owner},?1,row_number() OVER(PARTITION BY {owner} ORDER BY position)-1,{target}+?2 FROM old_catalog.{relation} WHERE {owner} IN(SELECT {owner} FROM main.{owners})")))
            .bind(role)
            .bind(offset)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

/// Copies the released catalog file: entities with their typed facts, their details, links,
/// the file index, Home rows and lyrics.
async fn copy_catalog(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::query("INSERT INTO sources(object_id) SELECT object_id FROM old_catalog.sources WHERE true ON CONFLICT(object_id) DO NOTHING")
        .execute(&mut *connection).await?;
    sqlx::query("CREATE TEMP TABLE migration_source_keys AS SELECT old.source_key old_key,current.source_key FROM old_catalog.sources old JOIN main.sources current USING(object_id)")
        .execute(&mut *connection).await?;
    let released = columns(connection, "old_catalog", "sources").await?;
    let column = |name: &str, absent: &'static str| {
        if released.iter().any(|column| column == name) {
            format!("old.{name}")
        } else {
            absent.to_owned()
        }
    };
    let (name, freshness) = (column("display_name", "''"), column("freshness", "NULL"));
    let complete = column("catalog_complete", "old.freshness IS NOT NULL");
    let complete = if released.iter().any(|column| column == "freshness") {
        complete
    } else {
        column("catalog_complete", "0")
    };
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE main.sources SET display_name={name},freshness={freshness},catalog_complete={complete} FROM old_catalog.sources old WHERE sources.object_id=old.object_id")))
        .execute(&mut *connection).await?;
    copy_named(connection, "artists", "artist_key", "artist").await?;
    copy_named(connection, "genres", "genre_key", "genre").await?;
    let mood_offset: i64 = sqlx::query_scalar("SELECT coalesce(max(tag_key),0) FROM tags")
        .fetch_one(&mut *connection)
        .await?;
    copy_named(connection, "moods", "mood_key", "mood").await?;
    copy_named(connection, "folders", "folder_key", "folder").await?;
    copy_albums(connection).await?;
    copy_tracks(connection).await?;
    // The plays servers report; Rufin's own listens come from the Store.
    if has_table(connection, "old_catalog", "activity_baseline").await? {
        sqlx::query("UPDATE main.tracks SET native_play_count=old.play_count,native_last_played=old.last_played_at FROM old_catalog.activity_baseline old JOIN migration_source_keys mapping ON mapping.old_key=old.source_key WHERE old.period='lifetime' AND old.item_kind='track' AND tracks.source_key=mapping.source_key AND tracks.object_id=old.track_object_id")
            .execute(&mut *connection).await?;
    }
    copy_links(connection, mood_offset).await?;
    copy_table(
        connection,
        "old_catalog",
        "home_entries",
        "home_entries",
        &[(
            "source_key",
            "(SELECT source_key FROM migration_source_keys WHERE old_key=old.source_key)",
        )],
        "true",
    )
    .await?;
    // A file whose released facts were waiting for a parse is read again on the next walk.
    let mut overrides = vec![
        ("file_key", "old.local_file_key"),
        (
            "source_key",
            "(SELECT source_key FROM migration_source_keys WHERE old_key=old.source_key)",
        ),
        ("relative_path", "replace(old.relative_path,char(92),'/')"),
        (
            "state",
            "CASE old.state WHEN 'observed' THEN 'accepted' ELSE old.state END",
        ),
    ];
    if columns(connection, "old_catalog", "local_files")
        .await?
        .iter()
        .any(|column| column == "parse_version")
    {
        overrides.push((
            "parse_version",
            "CASE old.state WHEN 'observed' THEN NULL ELSE old.parse_version END",
        ));
    }
    copy_table(
        connection,
        "old_catalog",
        "local_files",
        "files",
        &overrides,
        "true",
    )
    .await?;
    if has_table(connection, "old_catalog", "local_file_dependencies").await? {
        sqlx::query("INSERT INTO file_inputs(file_key,position,path) SELECT edge.local_file_key,edge.position,edge.dependency_path FROM old_catalog.local_file_dependencies edge JOIN main.files owner ON owner.file_key=edge.local_file_key WHERE owner.kind='cue' AND owner.state='accepted'")
            .execute(&mut *connection)
            .await?;
    }
    copy_table(
        connection,
        "old_catalog",
        "lyrics_cache",
        "lyrics_cache",
        &[],
        "true",
    )
    .await?;
    crate::lyrics::index_all_lyrics(connection).await?;
    Ok(())
}

const FIELDS: &[(&str, &str)] = &[
    ("title", "TEXT"),
    ("display_artist", "TEXT"),
    ("display_album", "TEXT"),
    ("album_display_artist", "TEXT"),
    ("duration_millis", "INTEGER"),
    ("disc_number", "INTEGER"),
    ("track_number", "INTEGER"),
    ("year", "INTEGER"),
    ("release_date", "TEXT"),
    ("source_format", "TEXT"),
    ("musicbrainz_recording_id", "TEXT"),
    ("musicbrainz_release_track_id", "TEXT"),
    ("musicbrainz_album_id", "TEXT"),
    ("musicbrainz_release_group_id", "TEXT"),
    ("primary_artist_musicbrainz_id", "TEXT"),
];

/// Typed `tracks` columns among FIELDS; the others go into the details JSON.
const THIN: &[&str] = &[
    "title",
    "display_artist",
    "display_album",
    "duration_millis",
    "disc_number",
    "track_number",
    "year",
    "release_date",
    "source_format",
];

async fn create_metadata_candidates(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let fields = FIELDS
        .iter()
        .map(|(name, kind)| format!("{name} {kind}"))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TEMP TABLE migration_metadata(media_uri TEXT NOT NULL,priority INTEGER NOT NULL,present INTEGER NOT NULL,stamp INTEGER NOT NULL,tie INTEGER NOT NULL,{fields}); CREATE INDEX migration_metadata_uri_idx ON migration_metadata(media_uri,priority,present,stamp DESC,tie DESC);")))
        .execute(connection).await?;
    Ok(())
}

async fn snapshot_candidates(
    connection: &mut SqliteConnection,
    schema: &str,
    table: &str,
    priority: i64,
    order: &str,
    renames: &[(&str, &str)],
) -> LibraryResult<()> {
    let old = columns(connection, schema, table).await?;
    if old.is_empty() {
        return Ok(());
    }
    let fields = FIELDS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",");
    let values = FIELDS
        .iter()
        .map(|(name, _)| {
            let column = renames
                .iter()
                .find(|(field, _)| field == name)
                .map_or(*name, |(_, column)| *column);
            if old.iter().any(|name| name == column) {
                format!("old.{column}")
            } else {
                "NULL".into()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,{fields}) SELECT old.media_uri,{priority},{order},{values} FROM {schema}.{table} old")))
        .execute(connection).await?;
    Ok(())
}

async fn saved_candidates(connection: &mut SqliteConnection) -> LibraryResult<()> {
    if !has_table(connection, "old_store", "queue_saved").await? {
        return Ok(());
    }
    let fields = FIELDS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",");
    let values = FIELDS
        .iter()
        .map(|(name, _)| {
            let field = match *name {
                "display_artist" => "artist",
                "display_album" => "album",
                field => field,
            };
            format!("json_extract(item.value,'$.{field}')")
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,{fields}) SELECT json_extract(item.value,'$.media_uri'),2,0,COALESCE(json_extract(item.value,'$.snapshot_at'),0),item.id,{values} FROM old_store.queue_saved saved,json_tree(saved.state) item WHERE item.type='object' AND json_type(item.value,'$.media_uri')='text'")))
        .execute(connection).await?;
    Ok(())
}

/// The best candidate value of a field for a media URI.
fn best(field: &str, uri: &str) -> String {
    format!(
        "(SELECT value.{field} FROM migration_metadata value WHERE value.media_uri={uri} AND value.{field} IS NOT NULL ORDER BY value.priority,value.present,value.stamp DESC,value.tie DESC LIMIT 1)"
    )
}

/// Tracks that a playlist entry, the queue or a download names but the catalog file lacks
/// become hidden reference rows with the display values the old Store kept. Listens keep
/// their own copy and need no row.
async fn reduce_metadata(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::query("INSERT INTO tracks(media_uri,indexed) SELECT DISTINCT media_uri,0 FROM migration_metadata WHERE priority<>3 AND true ON CONFLICT(media_uri) DO NOTHING")
        .execute(&mut *connection).await?;
    let missing = "tracks.indexed=0 AND NOT EXISTS(SELECT 1 FROM track_details WHERE track_details.track_key=tracks.track_key)";
    for field in THIN {
        let empty = if matches!(*field, "title" | "display_artist" | "display_album") {
            format!("{field}=''")
        } else if matches!(*field, "duration_millis" | "disc_number" | "track_number") {
            format!("{field}=0")
        } else {
            format!("{field} IS NULL")
        };
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE tracks SET {field}={} WHERE {missing} AND {empty} AND {} IS NOT NULL",
            best(field, "tracks.media_uri"),
            best(field, "tracks.media_uri")
        )))
        .execute(&mut *connection)
        .await?;
    }
    let details = FIELDS
        .iter()
        .filter(|(field, _)| !THIN.contains(field))
        .map(|(field, _)| format!("'{field}',{}", best(field, "tracks.media_uri")))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO track_details(track_key,metadata) SELECT track_key,jsonb(json_patch('{{}}',json_object({details}))) FROM tracks WHERE {missing}")))
        .execute(&mut *connection).await?;
    sqlx::query("UPDATE tracks SET sort_text=lower(CASE WHEN title='' THEN media_uri ELSE title END) WHERE indexed=0 AND sort_text=''")
        .execute(&mut *connection).await?;
    let mut after = 0_i64;
    loop {
        let rows: Vec<(i64, String)> = sqlx::query_as("SELECT track_key,media_uri FROM tracks WHERE track_key>?1 AND source_key IS NULL ORDER BY track_key LIMIT 256")
            .bind(after).fetch_all(&mut *connection).await?;
        if rows.is_empty() {
            break;
        }
        for (key, uri) in rows {
            after = key;
            if let Some((source, kind, object)) = crate::source_entity_parts(&uri)
                && kind == "track"
            {
                let source_key = source_identity(connection, source.as_str()).await?;
                sqlx::query("UPDATE tracks SET source_key=?2,object_id=?3 WHERE track_key=?1")
                    .bind(key)
                    .bind(source_key)
                    .bind(object)
                    .execute(&mut *connection)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn copy_native_playlists(connection: &mut SqliteConnection) -> LibraryResult<()> {
    if !has_table(connection, "old_catalog", "native_playlists").await? {
        return Ok(());
    }
    let columns = columns(connection, "old_catalog", "native_playlists").await?;
    let column = |name: &str, absent: &str| {
        if columns.iter().any(|column| column == name) {
            format!("old.{name}")
        } else {
            absent.to_owned()
        }
    };
    let writable = column("writable", "1");
    let metadata_writable = column("metadata_writable", &writable);
    let provider_revision = column("provider_revision", "NULL");
    let valid_until = column("valid_until", "NULL");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TEMP TABLE migration_playlist_keys AS SELECT old.playlist_key old_key,COALESCE(owned.playlist_key,(SELECT COALESCE(max(playlist_key),0) FROM playlists)+row_number() OVER(ORDER BY old.playlist_key)) playlist_key FROM old_catalog.native_playlists old JOIN migration_source_keys source ON source.old_key=old.source_key LEFT JOIN playlists owned ON owned.source_key=source.source_key AND owned.object_id=old.object_id;
            INSERT INTO playlists(playlist_key,source_key,object_id,name,owner,artwork_binding,writable,metadata_writable,provider_revision,valid_until)
            SELECT mapping.playlist_key,source.source_key,old.object_id,old.name,'source',old.artwork_binding,{writable},{metadata_writable},{provider_revision},{valid_until}
            FROM old_catalog.native_playlists old JOIN migration_playlist_keys mapping ON mapping.old_key=old.playlist_key JOIN migration_source_keys source ON source.old_key=old.source_key WHERE true
            ON CONFLICT(playlist_key) DO UPDATE SET owner='source',indexed=1,name=excluded.name,artwork_binding=COALESCE(playlists.artwork_binding,excluded.artwork_binding),writable=excluded.writable,metadata_writable=excluded.metadata_writable,provider_revision=excluded.provider_revision,valid_until=excluded.valid_until;")))
            .execute(&mut *connection).await?;
    let rank = crate::rank::nth_sql(
        "row_number() OVER(PARTITION BY mapping.playlist_key ORDER BY old.position,old.playlist_entry_key)-1",
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DELETE FROM playlist_entries WHERE playlist_key IN (SELECT playlist_key FROM migration_playlist_keys);
            INSERT INTO playlist_entries(playlist_entry_key,playlist_key,object_id,track_key,rank)
            SELECT (SELECT COALESCE(max(playlist_entry_key),0) FROM playlist_entries)+row_number() OVER(ORDER BY old.playlist_entry_key),mapping.playlist_key,old.object_id,track.track_key,{rank}
            FROM old_catalog.native_playlist_entries old JOIN migration_playlist_keys mapping ON mapping.old_key=old.playlist_key JOIN tracks track ON track.media_uri=old.media_uri;
            UPDATE home_entries SET entity_key=(SELECT playlist_key FROM migration_playlist_keys WHERE old_key=-home_entries.entity_key) WHERE entity_kind='playlist' AND entity_key<0;")))
            .execute(&mut *connection).await?;
    Ok(())
}

/// Released playlists keep their entries, in their order as ranks. Their list order and the
/// source scope of Rufin playlists made for a file source are not kept: every Rufin playlist
/// is global.
async fn playlists(connection: &mut SqliteConnection, catalog: bool) -> LibraryResult<()> {
    let rank = crate::rank::nth_sql(
        "row_number() OVER(PARTITION BY old.playlist_key ORDER BY old.position,old.playlist_entry_key)-1",
    );
    copy_table(
        connection,
        "old_store",
        "playlists",
        "playlists",
        &[
            (
                "owner",
                "CASE WHEN old.name IS NULL THEN 'source' ELSE 'rufin' END",
            ),
            ("indexed", "CASE WHEN old.name IS NULL THEN 0 ELSE 1 END"),
            (
                "source_key",
                "CASE WHEN old.name IS NULL THEN old.source_key END",
            ),
        ],
        "true",
    )
    .await?;
    sqlx::query("UPDATE playlists SET artwork_binding=CAST(json_object('kind','playlist','source_id',(SELECT object_id FROM sources WHERE source_key=playlists.source_key),'object_id',object_id,'revision',artwork_revision) AS BLOB) WHERE artwork_revision IS NOT NULL")
        .execute(&mut *connection).await?;
    copy_table(
        connection,
        "old_store",
        "playlist_entries",
        "playlist_entries",
        &[
            (
                "track_key",
                "(SELECT track_key FROM tracks WHERE media_uri=old.media_uri)",
            ),
            ("rank", rank.as_str()),
            // An imported entry was numbered within its playlist; ids are global.
            ("object_id", "CASE WHEN old.object_id GLOB 'playlist:[0-9]*' THEN 'rufin:entry:'||lower(hex(randomblob(16))) ELSE old.object_id END"),
        ],
        "true",
    )
    .await?;
    if catalog {
        copy_native_playlists(connection).await?;
    }
    for table in [
        "playlist_file_links",
        "playlist_file_ignored",
        "playlist_file_settings",
    ] {
        copy_table(connection, "old_store", table, table, &[], "true").await?;
    }
    Ok(())
}

/// Favorites and ratings, pending favorite deliveries, local file access and download records.
/// A released download was a receipt plus a locator, or a locator alone before 0.16; it becomes
/// one record, with the size its access metadata held. A download without a receipt has an
/// unknown quality until the downloads owner reads its record file.
async fn user_data(connection: &mut SqliteConnection, catalog: bool) -> LibraryResult<()> {
    copy_table(
        connection,
        "old_store",
        "user_media_state",
        "user_state",
        &[("changed_at", "unixepoch()")],
        "true",
    )
    .await?;
    copy_table(
        connection,
        "old_store",
        "favorite_outbox",
        "outbox",
        &[("kind", "'favorite'"), ("value", "old.favorite")],
        "true",
    )
    .await?;
    if !has_table(connection, "old_store", "local_locators").await? {
        return Ok(());
    }
    copy_table(
        connection,
        "old_store",
        "local_locators",
        "local_locators",
        &[],
        // Folder mapping locators are resolved from the saved mapping now.
        "old.origin NOT IN ('download','mapping')",
    )
    .await?;
    let present = columns(connection, "old_store", "local_locators").await?;
    let locator = |name: &str, absent: &str| {
        if present.iter().any(|column| column == name) {
            format!("locator.{name}")
        } else {
            absent.to_owned()
        }
    };
    let (source, root, relative) = (
        locator("source_key", "NULL"),
        locator("root", "''"),
        locator("relative_path", "''"),
    );
    let sized = catalog
        && columns(connection, "old_catalog", "local_access_metadata")
            .await?
            .iter()
            .any(|column| column == "size_bytes");
    let size = |access: &str| {
        if sized {
            format!(
                "(SELECT size_bytes FROM old_catalog.local_access_metadata metadata WHERE metadata.access_uri={access})"
            )
        } else {
            "NULL".to_owned()
        }
    };
    if has_table(connection, "old_store", "connect_media_files").await? {
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO downloads(media_uri,quality,size,path,source_id,root,relative_path,managed)
            SELECT receipt.media_uri,receipt.encoding,{},receipt.path,
                coalesce((SELECT object_id FROM sources WHERE source_key={source}),(SELECT source.object_id FROM tracks track JOIN sources source USING(source_key) WHERE track.media_uri=receipt.media_uri)),
                coalesce({root},''),coalesce({relative},''),receipt.managed
            FROM old_store.connect_media_files receipt
            LEFT JOIN old_store.local_locators locator ON locator.media_uri=receipt.media_uri AND locator.origin='download' AND locator.access_uri=receipt.path
            ORDER BY receipt.rowid ON CONFLICT DO NOTHING", size("receipt.path"))))
            .execute(&mut *connection).await?;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO downloads(media_uri,quality,size,path,source_id,root,relative_path,managed)
        SELECT locator.media_uri,'unknown',{},locator.access_uri,(SELECT object_id FROM sources WHERE source_key={source}),{root},{relative},1
        FROM old_store.local_locators locator WHERE locator.origin='download' AND NOT EXISTS(SELECT 1 FROM downloads WHERE downloads.media_uri=locator.media_uri)", size("locator.access_uri"))))
        .execute(&mut *connection).await?;
    Ok(())
}

async fn source_identity(connection: &mut SqliteConnection, source: &str) -> LibraryResult<i64> {
    sqlx::query("INSERT INTO sources(object_id) VALUES (?1) ON CONFLICT(object_id) DO NOTHING")
        .bind(source)
        .execute(&mut *connection)
        .await?;
    Ok(
        sqlx::query_scalar("SELECT source_key FROM sources WHERE object_id=?1")
            .bind(source)
            .fetch_one(connection)
            .await?,
    )
}

/// A track the migrated queue names.
async fn reference_track(
    connection: &mut SqliteConnection,
    uri: &str,
) -> LibraryResult<crate::TrackKey> {
    let identity = match crate::source_entity_parts(uri) {
        Some((source, kind, object)) if kind == "track" => {
            Some((source_identity(connection, source.as_str()).await?, object))
        }
        _ => None,
    };
    let (source, object) = identity.unzip();
    let known: Option<crate::TrackKey> = sqlx::query_scalar("SELECT track_key FROM tracks WHERE media_uri=?1 OR (source_key=?2 AND object_id=?3) ORDER BY media_uri=?1 DESC LIMIT 1")
        .bind(uri).bind(source).bind(object.as_deref())
        .fetch_optional(&mut *connection).await?;
    if let Some(key) = known {
        return Ok(key);
    }
    Ok(sqlx::query_scalar("INSERT INTO tracks(media_uri,source_key,object_id,sort_text) VALUES(?1,?2,?3,lower(?1)) RETURNING track_key")
        .bind(uri).bind(source).bind(object)
        .fetch_one(connection).await?)
}

async fn restore_saved_queue(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let saved = if has_table(connection, "old_store", "queue_saved").await? {
        sqlx::query_scalar::<_, String>("SELECT state FROM old_store.queue_saved WHERE singleton=1")
            .fetch_optional(&mut *connection)
            .await?
            .map(|json| serde_json::from_str::<serde_json::Value>(&json))
            .transpose()?
    } else {
        None
    };
    let (mut state, occurrences) = match saved {
        Some(saved) => {
            let saved = crate::queue::legacy::decode_saved(saved)?;
            let mut keys = Vec::with_capacity(saved.uris.len());
            for uri in &saved.uris {
                keys.push(reference_track(connection, uri).await?);
            }
            saved.restore(keys)
        }
        None => {
            let (rows, keys) = crate::queue::legacy::stored_rows(connection).await?;
            let mut ordinary = (0..rows.len()).collect::<Vec<_>>();
            ordinary.sort_unstable_by_key(|index| rows[*index].canonical_position);
            let mut order = vec![0; rows.len()];
            let mut entries = Vec::with_capacity(rows.len());
            let mut occurrences = Vec::with_capacity(rows.len());
            for (position, index) in ordinary.into_iter().enumerate() {
                order[index] = position as u32;
                entries.push(rows[index].entry(keys[index]));
                occurrences.push(Some(rows[index].occurrence.clone()));
            }
            (
                crate::QueueRestore {
                    ids: (1..=entries.len() as u64).collect(),
                    entries: entries.into(),
                    order: order.into(),
                    ..Default::default()
                },
                occurrences,
            )
        }
    };
    if has_table(connection, "old_store", "queue_order").await? {
        if let Some(order) = sqlx::query_scalar::<_, String>(
            "SELECT state FROM old_store.queue_order WHERE singleton=1",
        )
        .fetch_optional(&mut *connection)
        .await?
        {
            state.order = serde_json::from_str(&order)?;
        }
    }
    if let Some((current,progress,repeat,shuffled))=sqlx::query_as::<_,(Option<String>,i64,String,bool)>("SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM old_store.queue_state WHERE singleton=1").fetch_optional(&mut *connection).await? {
        state.current_index=current.and_then(|current|state.order.iter().position(|index|occurrences.get(*index as usize).and_then(Option::as_deref)==Some(current.as_str())));
        state.progress_millis=progress;
        state.repeat_mode=match repeat.as_str() {"none"=>crate::QueueRepeatMode::Off,"one"=>crate::QueueRepeatMode::One,"all"=>crate::QueueRepeatMode::All,_=>return Err(LibraryError::InvalidStore("invalid saved repeat mode".into()))};
        state.shuffled=shuffled;
    }
    crate::queue::save_queue_on(connection, &state).await
}

/// Builds the Store at `path` from the released files, which it only reads, in one
/// transaction, so an upgrade that stops early leaves a Store without a version and starts over.
pub(crate) async fn import(released: &Path, path: &Path, catalog: &Path) -> LibraryResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut connection = crate::db::open_writer(path).await?;
    attach(&mut connection, released, "old_store").await?;
    let has_catalog = catalog.exists();
    if has_catalog {
        attach(&mut connection, catalog, "old_catalog").await?;
    }
    let mut transaction = connection.begin().await?;
    crate::schema::initialize_durable(&mut transaction).await?;
    sqlx::query("DELETE FROM smart_playlists")
        .execute(&mut *transaction)
        .await?;
    sqlx::query("INSERT INTO sources(source_key,object_id) SELECT source_key,object_id FROM old_store.source_ids")
        .execute(&mut *transaction).await?;
    if has_catalog {
        copy_catalog(&mut transaction).await?;
    }
    create_metadata_candidates(&mut transaction).await?;
    snapshot_candidates(
        &mut transaction,
        "old_store",
        "playlist_entries",
        1,
        "(old.title IS NULL),old.snapshot_at,old.playlist_entry_key",
        &[("display_artist", "artist"), ("display_album", "album")],
    )
    .await?;
    if has_catalog {
        snapshot_candidates(
            &mut transaction,
            "old_catalog",
            "native_playlist_entries",
            1,
            "(old.title IS NULL),old.snapshot_at,-old.playlist_entry_key",
            &[("display_artist", "artist"), ("display_album", "album")],
        )
        .await?;
    }
    snapshot_candidates(
        &mut transaction,
        "old_store",
        "queue_occurrences",
        2,
        "0,old.snapshot_at,old.queue_occurrence_key",
        &[("display_artist", "artist"), ("display_album", "album")],
    )
    .await?;
    snapshot_candidates(
        &mut transaction,
        "old_store",
        "listens",
        3,
        "0,old.started_at,old.listen_key",
        &[
            ("title", "track_title"),
            ("display_artist", "artist_name"),
            ("display_album", "album_title"),
        ],
    )
    .await?;
    if has_catalog {
        sqlx::query("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,title,display_artist,display_album,duration_millis,disc_number,track_number) SELECT locator.media_uri,4,0,0,locator.local_access_file_key,metadata.title,metadata.artist,metadata.album,metadata.duration_millis,metadata.disc_number,metadata.track_number FROM old_store.local_locators locator JOIN old_catalog.local_access_metadata metadata USING(access_uri) WHERE locator.origin='download'")
            .execute(&mut *transaction).await?;
    }
    saved_candidates(&mut transaction).await?;
    reduce_metadata(&mut transaction).await?;
    playlists(&mut transaction, has_catalog).await?;
    for table in ["smart_playlists", "listens", "listen_outbox"] {
        copy_table(&mut transaction, "old_store", table, table, &[], "true").await?;
    }
    user_data(&mut transaction, has_catalog).await?;
    // Import locators of files inside a music folder go, so those files hide and return with
    // their folder. A released walk hid a track outside the folders unless an import locator
    // named it, so a root holding a listed track without one is a music folder.
    sqlx::query("DELETE FROM local_locators WHERE origin='import' AND path IN(SELECT path FROM files WHERE root IN(SELECT file.root FROM tracks track CROSS JOIN track_details details USING(track_key) CROSS JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path WHERE track.indexed=1 AND NOT EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import')))")
        .execute(&mut *transaction)
        .await?;
    if has_table(&mut transaction, "old_store", "legacy_activity").await? {
        let counts = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{} FROM old_store.legacy_activity",
            crate::activity::RELEASED_COUNTS
        )))
        .fetch_all(&mut *transaction)
        .await?;
        crate::activity::carry_released_counts(&mut transaction, counts).await?;
    }
    sqlx::query("DROP TABLE migration_metadata")
        .execute(&mut *transaction)
        .await?;
    sqlx::query("UPDATE smart_playlists SET artwork_binding=CAST(json_object('kind','playlist','smart',json('true'),'source_id',NULL,'object_id',object_id,'revision',artwork_revision) AS BLOB) WHERE artwork_revision IS NOT NULL").execute(&mut *transaction).await?;
    super::normalize_stored_artwork(&mut transaction).await?;
    restore_saved_queue(&mut transaction).await?;
    // Hidden rows start their 60-day expiry at the upgrade.
    sqlx::query("UPDATE tracks SET hidden_since=unixepoch() WHERE indexed=0")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    sqlx::raw_sql("PRAGMA main.optimize=0x10002")
        .execute(&mut connection)
        .await?;
    connection.close().await?;
    Ok(())
}
