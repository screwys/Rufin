//! Migrates the last released Store into the current schema and salvages readable
//! families from older or damaged released Stores. Unreleased development schemas
//! receive no compatibility path.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::{Connection, Sqlite, SqliteConnection, Transaction};

use crate::{
    LibraryError, LibraryResult, db, loudness::initialize_recovered_loudness_keys, schema,
};

static RECOVERY_NUMBER: AtomicU64 = AtomicU64::new(0);

const SCHEMA_40_TABLES: &[&str] = &[
    "album_release_info",
    "albums",
    "artists",
    "genres",
    "listening_aggregates",
    "local_access_files",
    "local_favorites",
    "local_files",
    "local_imports",
    "local_playlist_entries",
    "local_playlists",
    "loudness_measurements",
    "lyrics_cache",
    "music_folders",
    "pending_favorites",
    "pending_scrobbles",
    "playback_queues",
    "playback_state",
    "recent_plays",
    "smart_playlists",
    "source_libraries",
    "source_playlist_entries",
    "source_playlists",
    "tracks",
    "user_ratings",
];

/// What was preserved while replacing a released Store.
#[derive(Debug)]
pub(crate) struct RecoveryReport {
    pub preserved_store: PathBuf,
    pub recovered_rows: usize,
    pub unreadable_families: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleasedCopyMode {
    Migration,
    Repair,
}

pub(crate) async fn is_migratable_schema_40(path: &Path) -> LibraryResult<bool> {
    released_store_matches(path, true).await
}

pub(crate) async fn is_repairable_released(path: &Path) -> LibraryResult<bool> {
    released_store_matches(path, false).await
}

async fn released_store_matches(path: &Path, exact_schema_40: bool) -> LibraryResult<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = match SqliteConnection::connect_with(&options).await {
        Ok(connection) => connection,
        Err(_) => return Ok(false),
    };
    let application_id = schema::pragma(&mut connection, "application_id")
        .await
        .unwrap_or_default();
    let user_version = schema::pragma(&mut connection, "user_version")
        .await
        .unwrap_or_default();
    let expected_version = if exact_schema_40 {
        user_version == schema::LAST_LEGACY_SCHEMA_VERSION
    } else {
        user_version < schema::RELEASED_SCHEMA_VERSION
    };
    if !schema::is_released(application_id, user_version) || !expected_version {
        connection.close().await?;
        return Ok(false);
    }
    let released_schema = if exact_schema_40 {
        let intact = sqlx::query_scalar::<_, String>("PRAGMA quick_check")
            .fetch_one(&mut connection)
            .await
            .is_ok_and(|value| value == "ok");
        let tables = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut connection)
        .await?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        intact && SCHEMA_40_TABLES.iter().all(|table| tables.contains(*table))
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='source_libraries'",
        )
        .fetch_optional(&mut connection)
        .await?
        .is_some()
    };
    connection.close().await?;
    Ok(released_schema)
}

pub(crate) async fn migrate_schema_40(path: &Path) -> LibraryResult<RecoveryReport> {
    if !is_migratable_schema_40(path).await? {
        return Err(LibraryError::InvalidRequest(
            "only a recognizable schema-40 Rufin Store can be migrated".to_string(),
        ));
    }
    let pending_store = unique_sibling(path, "schema-41-pending")?;
    let preserved_store = unique_sibling(path, "schema-40")?;
    let mut report = RecoveryReport {
        preserved_store: preserved_store.clone(),
        recovered_rows: 0,
        unreadable_families: Vec::new(),
    };
    let migration = async {
        let mut destination = db::open_writer(&pending_store).await?;
        schema::initialize(&mut destination).await?;
        copy_released(
            &mut destination,
            path,
            &mut report,
            ReleasedCopyMode::Migration,
        )
        .await?;
        schema::validate(&mut destination).await?;
        destination.close().await?;
        Ok::<(), LibraryError>(())
    }
    .await;
    if let Err(error) = migration {
        remove_store_family(&pending_store)?;
        return Err(error);
    }
    install_migrated_store(path, &pending_store, &preserved_store)?;
    Ok(report)
}

pub(crate) async fn repair_released(path: &Path) -> LibraryResult<RecoveryReport> {
    if !is_repairable_released(path).await? {
        return Err(LibraryError::InvalidRequest(
            "only a recognizable released Rufin Store can be repaired".to_string(),
        ));
    }
    replace_released(path).await
}

async fn replace_released(path: &Path) -> LibraryResult<RecoveryReport> {
    let preserved_store = unique_sibling(path, "recovered")?;
    fs::rename(path, &preserved_store)?;
    preserve_sidecar(path, &preserved_store, "-wal")?;
    preserve_sidecar(path, &preserved_store, "-shm")?;

    let mut report = RecoveryReport {
        preserved_store: preserved_store.clone(),
        recovered_rows: 0,
        unreadable_families: Vec::new(),
    };
    let result = async {
        let mut destination = db::open_writer(path).await?;
        schema::initialize(&mut destination).await?;
        let mut source = db::open_writer(&report.preserved_store).await?;
        let user_version = schema::pragma(&mut source, "user_version").await?;
        let intact = sqlx::query_scalar::<_, String>("PRAGMA quick_check")
            .fetch_one(&mut source)
            .await
            .map(|value| value == "ok")
            .unwrap_or(false);
        if intact && user_version < schema::LAST_LEGACY_SCHEMA_VERSION {
            schema::upgrade_legacy_released(&mut source).await?;
        }
        source.close().await?;
        let preserved_store = report.preserved_store.clone();
        copy_released(
            &mut destination,
            &preserved_store,
            &mut report,
            ReleasedCopyMode::Repair,
        )
        .await?;
        schema::validate(&mut destination).await?;
        destination.close().await?;
        Ok(report)
    }
    .await;
    result
}

pub(crate) async fn rebuild_unusable(path: &Path) -> LibraryResult<()> {
    if path.exists() {
        let preserved = unique_sibling(path, "recovered")?;
        fs::rename(path, &preserved)?;
        preserve_sidecar(path, &preserved, "-wal")?;
        preserve_sidecar(path, &preserved, "-shm")?;
    }
    let mut destination = db::open_writer(path).await?;
    schema::initialize(&mut destination).await?;
    destination.close().await?;
    Ok(())
}

pub(crate) fn is_store_content_failure(error: &LibraryError) -> bool {
    match error {
        LibraryError::UnsupportedStore { .. } | LibraryError::InvalidStore(_) => true,
        LibraryError::Sqlite(sqlx::Error::Database(error)) => error
            .code()
            .as_deref()
            .is_some_and(|code| code == "11" || code == "26"),
        _ => false,
    }
}

async fn copy_released(
    destination: &mut SqliteConnection,
    released_path: &Path,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    sqlx::query("ATTACH DATABASE ?1 AS released")
        .bind(released_path.to_string_lossy().as_ref())
        .execute(&mut *destination)
        .await?;
    let mut transaction = destination.begin().await?;
    copy_family(
        &mut transaction,
        report,
        mode,
        "sources",
        "INSERT INTO sources(
             object_id, display_name, normalized_name, freshness,
             catalog_digest, artwork_digest, catalog_revision
         )
         SELECT library.source_id, library.source_id, lower(library.source_id),
                library.freshness_marker,
                COALESCE(library.content_digest, library.input_digest),
                zeroblob(32), 1
         FROM released.source_libraries AS library
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id = (
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id = library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        &mut transaction,
        report,
        mode,
        "albums",
        "INSERT INTO albums(
             source_key, object_id, title, normalized_title, display_artist,
             sort_text, year, release_date, date_added,
             musicbrainz_release_id, musicbrainz_release_group_id,
             is_compilation,artwork_binding, source_favorite, source_rating, first_seen_at
         )
         SELECT source.source_key, album.album_id, album.title, lower(album.title),
                album.display_artist, lower(album.title), album.year,
                album.release_date, album.date_added,
                album.musicbrainz_release_id, album.musicbrainz_release_group_id,
                album.is_compilation,
                CASE album.local_artwork_kind
                  WHEN 'file' THEN CAST(json_object('File',json_object(
                    'path',album.local_artwork_path,'revision',album.local_artwork_revision)) AS BLOB)
                  WHEN 'embedded' THEN CAST(json_object('Embedded',json_object(
                    'path',album.local_artwork_path,'picture_index',album.local_artwork_picture_index,
                    'revision',album.local_artwork_revision)) AS BLOB)
                  ELSE CASE WHEN album.image_item_id IS NULL THEN NULL ELSE
                    CAST(json_object('item_id', album.image_item_id, 'tag', album.image_tag) AS BLOB)
                  END
                END,
                album.favorite, album.user_rating, NULL
         FROM released.albums AS album
         JOIN released.source_libraries AS library USING (library_id)
         JOIN sources AS source ON source.object_id = library.source_id
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id = (
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id = library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        &mut transaction,
        report,
        mode,
        "tracks",
        "INSERT INTO tracks(
             source_key, object_id, album_key, title, normalized_search,
             display_album, display_artist, sort_text, duration_millis,
             disc_number, track_number, year, release_date, date_added, media_uri, source_path,
             source_format, comment, bpm, musicbrainz_recording_id,
             musicbrainz_release_track_id, cue_path, cue_start_millis, cue_end_millis,
             artwork_binding, source_favorite, source_rating, first_seen_at
         )
         SELECT source.source_key, track.track_id, album.album_key,
                track.title,
                lower(
                    track.title || ' ' || track.display_album || ' '
                    || track.display_artist || ' ' || COALESCE(track.comment, '')
                ),
                track.display_album,
                track.display_artist, lower(track.title),
                track.duration_seconds * 1000, track.disc_number,
                track.track_number, track.year, track.release_date,
                track.date_added,
                CASE WHEN library.source_id<>'local:server:library' THEN NULL
                     WHEN track.source_path IS NULL THEN NULL
                     WHEN substr(track.source_path,1,7)='file://' THEN track.source_path
                     ELSE 'file://' || track.source_path END,
                track.source_path,
                track.source_format,
                track.comment, track.bpm, track.musicbrainz_recording_id,
                track.musicbrainz_release_track_id, track.cue_path,
                track.cue_start_millis, track.cue_end_millis,
                CASE track.local_artwork_kind
                  WHEN 'file' THEN CAST(json_object('File',json_object(
                    'path',track.local_artwork_path,'revision',track.local_artwork_revision)) AS BLOB)
                  WHEN 'embedded' THEN CAST(json_object('Embedded',json_object(
                    'path',track.local_artwork_path,'picture_index',track.local_artwork_picture_index,
                    'revision',track.local_artwork_revision)) AS BLOB)
                  ELSE CASE WHEN track.image_item_id IS NULL THEN NULL ELSE
                    CAST(json_object('item_id', track.image_item_id, 'tag', track.image_tag) AS BLOB)
                  END
                END,
                track.favorite, track.user_rating, NULL
         FROM released.tracks AS track
         JOIN released.source_libraries AS library USING (library_id)
         JOIN sources AS source ON source.object_id = library.source_id
         LEFT JOIN albums AS album
           ON album.source_key = source.source_key
          AND album.object_id = track.album_id
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id = (
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id = library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        &mut transaction,
        report,
        mode,
        "Track first-seen facts",
        "UPDATE tracks SET first_seen_at=(SELECT import.first_seen_at FROM released.local_imports import JOIN sources source ON source.object_id=import.source_id WHERE source.source_key=tracks.source_key AND import.track_id=tracks.object_id) WHERE first_seen_at IS NULL",
    )
    .await?;
    copy_family(
        &mut transaction,
        report,
        mode,
        "Album first-seen facts",
        "UPDATE albums SET first_seen_at=(SELECT min(track.first_seen_at) FROM tracks track WHERE track.album_key=albums.album_key) WHERE first_seen_at IS NULL",
    )
    .await?;
    salvage_named_entities(&mut transaction, report, mode).await?;
    salvage_relationships(&mut transaction, report, mode).await?;
    salvage_playlists(&mut transaction, report, mode).await?;
    copy_family(
        &mut transaction,
        report,
        mode,
        "cached Home",
        "INSERT INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,artwork_binding)
         SELECT source.source_key,
                CASE json_extract(section.value,'$.kind')
                  WHEN 'MostPlayed' THEN 'most-played' WHEN 'NewlyAdded' THEN 'newly-added'
                  WHEN 'RecentlyPlayed' THEN 'recently-played' WHEN 'RecentlyReleased' THEN 'recently-released' END,
                CAST(item.key AS INTEGER),json_extract(item.value,'$.kind'),
                CASE json_extract(item.value,'$.kind')
                  WHEN 'track' THEN track.track_key WHEN 'album' THEN album.album_key END,
                CASE json_extract(item.value,'$.kind')
                  WHEN 'track' THEN track.title WHEN 'album' THEN album.title END,
                CASE json_extract(item.value,'$.kind')
                  WHEN 'track' THEN track.display_artist WHEN 'album' THEN album.display_artist END,
                CASE json_extract(item.value,'$.kind')
                  WHEN 'track' THEN COALESCE(track.artwork_binding,track_album.artwork_binding)
                  WHEN 'album' THEN album.artwork_binding END
         FROM released.source_libraries library
         JOIN sources source ON source.object_id=library.source_id
         JOIN json_each(json_extract(library.home_json,'$.sections')) section
         JOIN json_each(json_extract(section.value,'$.items')) item
         LEFT JOIN tracks track ON track.source_key=source.source_key AND json_extract(item.value,'$.kind')='track' AND track.object_id=json_extract(item.value,'$.id')
         LEFT JOIN albums track_album ON track_album.album_key=track.album_key
         LEFT JOIN albums album ON album.source_key=source.source_key AND json_extract(item.value,'$.kind')='album' AND album.object_id=json_extract(item.value,'$.id')
         WHERE library.home_json IS NOT NULL AND library.accepted_at IS NOT NULL
           AND library.library_id=(SELECT max(current.library_id) FROM released.source_libraries current WHERE current.source_id=library.source_id AND current.accepted_at IS NOT NULL)
           AND (track.track_key IS NOT NULL OR album.album_key IS NOT NULL)",
    ).await?;
    salvage_album_release(&mut transaction, report, mode).await?;
    salvage_user_facts(&mut transaction, report, mode).await?;
    if mode == ReleasedCopyMode::Repair {
        salvage_queue(&mut transaction, report, mode).await?;
    }
    salvage_local(&mut transaction, report, mode).await?;
    initialize_recovered_loudness_keys(&mut transaction).await?;
    if mode == ReleasedCopyMode::Migration {
        validate_migration_copy(&mut transaction).await?;
    }
    transaction.commit().await?;
    sqlx::raw_sql("DETACH DATABASE released; PRAGMA optimize;")
        .execute(&mut *destination)
        .await?;
    Ok(())
}

async fn validate_migration_copy(transaction: &mut Transaction<'_, Sqlite>) -> LibraryResult<()> {
    for (family, expected_sql, actual_sql) in [
        (
            "sources",
            "SELECT count(*) FROM released.source_libraries AS library
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT max(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
            "SELECT count(*) FROM sources",
        ),
        (
            "Albums",
            "SELECT count(*) FROM released.albums AS item
             JOIN released.source_libraries AS library USING (library_id)
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT max(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
            "SELECT count(*) FROM albums",
        ),
        (
            "Tracks",
            "SELECT count(*) FROM released.tracks AS item
             JOIN released.source_libraries AS library USING (library_id)
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT max(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
            "SELECT count(*) FROM tracks",
        ),
        (
            "Playlist entries",
            "SELECT
                 (SELECT count(*) FROM released.source_playlist_entries AS entry
                  JOIN released.source_libraries AS library USING (library_id)
                  WHERE library.accepted_at IS NOT NULL
                    AND library.library_id=(
                        SELECT max(current.library_id)
                        FROM released.source_libraries AS current
                        WHERE current.source_id=library.source_id
                          AND current.accepted_at IS NOT NULL
                    ))
                 + (SELECT count(*) FROM released.local_playlist_entries)",
            "SELECT count(*) FROM playlist_entries",
        ),
    ] {
        let expected = sqlx::query_scalar::<_, i64>(expected_sql)
            .fetch_one(&mut **transaction)
            .await?;
        let actual = sqlx::query_scalar::<_, i64>(actual_sql)
            .fetch_one(&mut **transaction)
            .await?;
        if actual != expected {
            return Err(LibraryError::InvalidStore(format!(
                "schema-40 migration changed {family} count from {expected} to {actual}"
            )));
        }
    }
    let foreign_key_failure = sqlx::query("PRAGMA foreign_key_check")
        .fetch_optional(&mut **transaction)
        .await?
        .is_some();
    if foreign_key_failure {
        return Err(LibraryError::InvalidStore(
            "schema-40 migration failed foreign-key validation".to_string(),
        ));
    }
    Ok(())
}

async fn salvage_album_release(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    copy_family(transaction, report, mode, "Album release lookup",
        "UPDATE albums SET release_lookup_identity=(SELECT info.exact_identity_key FROM released.album_release_info info JOIN sources source ON source.object_id=info.source_id WHERE source.source_key=albums.source_key AND info.album_id=albums.object_id) WHERE EXISTS (SELECT 1 FROM released.album_release_info info JOIN sources source ON source.object_id=info.source_id WHERE source.source_key=albums.source_key AND info.album_id=albums.object_id);
         DELETE FROM album_release_types WHERE EXISTS (SELECT 1 FROM released.album_release_info info JOIN sources source ON source.object_id=info.source_id JOIN albums album ON album.source_key=source.source_key AND album.object_id=info.album_id WHERE info.lookup_state='found' AND album.album_key=album_release_types.album_key);
         INSERT INTO album_release_types(album_key,release_type,position) SELECT album.album_key,value.value,value.key FROM released.album_release_info info JOIN sources source ON source.object_id=info.source_id JOIN albums album ON album.source_key=source.source_key AND album.object_id=info.album_id JOIN json_each(info.release_types_json) value WHERE info.lookup_state='found'").await
}

async fn salvage_relationships(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    for (family, sql) in [
        (
            "moods",
            "INSERT OR IGNORE INTO moods(source_key, object_id, name, normalized_name, sort_text)
             SELECT source.source_key, json_extract(mood.value, '$.id'),
                    json_extract(mood.value, '$.name'),
                    lower(json_extract(mood.value, '$.name')),
                    lower(json_extract(mood.value, '$.name'))
             FROM released.tracks AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN json_each(item.relations_json, '$.moods') AS mood
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "album artists",
            "INSERT INTO album_artists(album_key, artist_key, position)
             SELECT album.album_key, artist.artist_key, relation.key
             FROM released.albums AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN albums AS album
               ON album.source_key=source.source_key AND album.object_id=item.album_id
             JOIN json_each(item.relations_json, '$.album_artists') AS relation
             JOIN artists AS artist
               ON artist.source_key=source.source_key
              AND artist.object_id=json_extract(relation.value, '$.id')
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "track artists",
            "INSERT INTO track_artists(track_key, artist_key, position)
             SELECT track.track_key, artist.artist_key, relation.key
             FROM released.tracks AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN tracks AS track
               ON track.source_key=source.source_key AND track.object_id=item.track_id
             JOIN json_each(item.relations_json, '$.artists') AS relation
             JOIN artists AS artist
               ON artist.source_key=source.source_key
              AND artist.object_id=json_extract(relation.value, '$.id')
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "album genres",
            "INSERT INTO album_genres(album_key, genre_key, position)
             SELECT album.album_key, genre.genre_key, relation.key
             FROM released.albums AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN albums AS album
               ON album.source_key=source.source_key AND album.object_id=item.album_id
             JOIN json_each(item.relations_json, '$.genres') AS relation
             JOIN genres AS genre
               ON genre.source_key=source.source_key
              AND genre.object_id=json_extract(relation.value, '$.id')
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "track genres",
            "INSERT INTO track_genres(track_key, genre_key, position)
             SELECT track.track_key, genre.genre_key, relation.key
             FROM released.tracks AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN tracks AS track
               ON track.source_key=source.source_key AND track.object_id=item.track_id
             JOIN json_each(item.relations_json, '$.genres') AS relation
             JOIN genres AS genre
               ON genre.source_key=source.source_key
              AND genre.object_id=json_extract(relation.value, '$.id')
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "track moods",
            "INSERT INTO track_moods(track_key, mood_key, position)
             SELECT track.track_key, mood.mood_key, relation.key
             FROM released.tracks AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN tracks AS track
               ON track.source_key=source.source_key AND track.object_id=item.track_id
             JOIN json_each(item.relations_json, '$.moods') AS relation
             JOIN moods AS mood
               ON mood.source_key=source.source_key
              AND mood.object_id=json_extract(relation.value, '$.id')
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "track folders",
            "INSERT INTO track_folders(track_key, folder_key, position)
             SELECT track.track_key, folder.folder_key, relation.key
             FROM released.tracks AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN tracks AS track
               ON track.source_key=source.source_key AND track.object_id=item.track_id
             JOIN json_each(item.relations_json, '$.music_folders') AS relation
             JOIN folders AS folder
               ON folder.source_key=source.source_key AND folder.object_id=relation.value
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "album release types",
            "INSERT INTO album_release_types(album_key, release_type, position)
             SELECT album.album_key, relation.value, relation.key
             FROM released.albums AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id=library.source_id
             JOIN albums AS album
               ON album.source_key=source.source_key AND album.object_id=item.album_id
             JOIN json_each(item.release_types_json) AS relation
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id=(
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id=library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
    ] {
        copy_family(transaction, report, mode, family, sql).await?;
    }
    Ok(())
}

async fn salvage_named_entities(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    for (family, sql) in [
        (
            "artists",
            "INSERT INTO artists(
                 source_key, object_id, name, normalized_name, sort_text,
                 musicbrainz_artist_id, artwork_binding,
                 source_favorite, source_rating
             )
             SELECT source.source_key, item.artist_id, item.name,
                    lower(item.name), lower(item.name), item.musicbrainz_artist_id,
                    CASE item.local_artwork_kind
                      WHEN 'file' THEN CAST(json_object('File',json_object(
                        'path',item.local_artwork_path,'revision',item.local_artwork_revision)) AS BLOB)
                      WHEN 'embedded' THEN CAST(json_object('Embedded',json_object(
                        'path',item.local_artwork_path,'picture_index',item.local_artwork_picture_index,
                        'revision',item.local_artwork_revision)) AS BLOB)
                      ELSE CASE WHEN item.image_item_id IS NULL THEN NULL ELSE
                        CAST(json_object('item_id', item.image_item_id, 'tag', item.image_tag) AS BLOB)
                      END
                    END,
                    item.favorite, item.user_rating
             FROM released.artists AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id = library.source_id
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id = (
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id = library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "genres",
            "INSERT INTO genres(
                 source_key, object_id, name, normalized_name, sort_text, artwork_binding
             )
             SELECT source.source_key, item.genre_id, item.name,
                    lower(item.name), lower(item.name),
                    CASE WHEN item.image_item_id IS NULL THEN NULL ELSE
                        CAST(json_object('item_id', item.image_item_id, 'tag', item.image_tag) AS BLOB)
                    END
             FROM released.genres AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id = library.source_id
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id = (
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id = library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
        (
            "folders",
            "INSERT INTO folders(
                 source_key, object_id, name, normalized_name, sort_text, artwork_binding
             )
             SELECT source.source_key, item.folder_id, item.name,
                    lower(item.name), lower(item.name),
                    CASE WHEN item.image_item_id IS NULL THEN NULL ELSE
                        CAST(json_object('item_id', item.image_item_id, 'tag', item.image_tag) AS BLOB)
                    END
             FROM released.music_folders AS item
             JOIN released.source_libraries AS library USING (library_id)
             JOIN sources AS source ON source.object_id = library.source_id
             WHERE library.accepted_at IS NOT NULL
               AND library.library_id = (
                   SELECT MAX(current.library_id)
                   FROM released.source_libraries AS current
                   WHERE current.source_id = library.source_id
                     AND current.accepted_at IS NOT NULL
               )",
        ),
    ] {
        copy_family(transaction, report, mode, family, sql).await?;
    }
    Ok(())
}

async fn salvage_playlists(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    copy_family(
        transaction,
        report,
        mode,
        "playlists",
        "INSERT INTO playlists(
             source_key, ownership, object_id, name, normalized_name, sort_text, artwork_binding
         )
         SELECT source.source_key, 'source', item.playlist_id, item.name,
                lower(item.name), lower(item.name),
                CASE WHEN item.image_item_id IS NULL THEN NULL ELSE
                    CAST(json_object('item_id', item.image_item_id, 'tag', item.image_tag) AS BLOB)
                END
         FROM released.source_playlists AS item
         JOIN released.source_libraries AS library USING (library_id)
         JOIN sources AS source ON source.object_id = library.source_id
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id = (
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id = library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "playlist entries",
        "INSERT INTO playlist_entries(
             playlist_key, object_id, track_key, track_object_id, position
         )
         SELECT playlist.playlist_key, entry.occurrence_id, track.track_key,
                entry.track_id, entry.position
         FROM released.source_playlist_entries AS entry
         JOIN released.source_libraries AS library USING (library_id)
         JOIN sources AS source ON source.object_id = library.source_id
         JOIN playlists AS playlist
           ON playlist.source_key = source.source_key
          AND playlist.ownership = 'source'
          AND playlist.object_id = entry.playlist_id
         LEFT JOIN tracks AS track
           ON track.source_key = source.source_key
          AND track.object_id = entry.track_id
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id = (
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id = library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "user playlists",
        "INSERT INTO playlists(
             source_key, ownership, object_id, name, normalized_name, sort_text
         )
         SELECT source.source_key, 'user', item.playlist_id, item.name,
                lower(item.name), lower(item.name)
         FROM released.local_playlists AS item
         JOIN sources AS source ON source.object_id=item.source_id",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "user playlist entries",
        "INSERT INTO playlist_entries(
             playlist_key, object_id, track_key, track_object_id, position
         )
         SELECT playlist.playlist_key, entry.occurrence_id, track.track_key,
                entry.track_id, entry.position
         FROM released.local_playlist_entries AS entry
         JOIN sources AS source ON source.object_id=entry.source_id
         JOIN playlists AS playlist
           ON playlist.source_key=source.source_key
          AND playlist.ownership='user'
          AND playlist.object_id=entry.playlist_id
         LEFT JOIN tracks AS track
           ON track.source_key=source.source_key
          AND track.object_id=entry.track_id",
    )
    .await?;
    Ok(())
}

async fn salvage_user_facts(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    for (family, sql) in [
        (
            "favorite overrides",
            "UPDATE tracks SET user_favorite = 1
             WHERE EXISTS (
                 SELECT 1 FROM released.local_favorites AS favorite
                 JOIN sources AS source ON source.object_id = favorite.source_id
                 WHERE favorite.item_kind = 'track'
                   AND favorite.item_id = tracks.object_id
                   AND source.source_key = tracks.source_key
             );
             UPDATE albums SET user_favorite = 1
             WHERE EXISTS (
                 SELECT 1 FROM released.local_favorites AS favorite
                 JOIN sources AS source ON source.object_id = favorite.source_id
                 WHERE favorite.item_kind = 'album'
                   AND favorite.item_id = albums.object_id
                   AND source.source_key = albums.source_key
             );
             UPDATE artists SET user_favorite = 1
             WHERE EXISTS (
                 SELECT 1 FROM released.local_favorites AS favorite
                 JOIN sources AS source ON source.object_id = favorite.source_id
                 WHERE favorite.item_kind = 'artist'
                   AND favorite.item_id = artists.object_id
                   AND source.source_key = artists.source_key
             )",
        ),
        (
            "rating overrides",
            "UPDATE tracks SET user_rating = (
                 SELECT rating * 10 FROM released.user_ratings AS rating
                 JOIN sources AS source ON source.object_id = rating.source_id
                 WHERE rating.item_kind = 'track'
                   AND rating.item_id = tracks.object_id
                   AND source.source_key = tracks.source_key
             ) WHERE EXISTS (
                 SELECT 1 FROM released.user_ratings AS rating
                 JOIN sources AS source ON source.object_id = rating.source_id
                 WHERE rating.item_kind = 'track'
                   AND rating.item_id = tracks.object_id
                   AND source.source_key = tracks.source_key
             );
             UPDATE albums SET user_rating = (
                 SELECT rating * 10 FROM released.user_ratings AS rating
                 JOIN sources AS source ON source.object_id = rating.source_id
                 WHERE rating.item_kind = 'album'
                   AND rating.item_id = albums.object_id
                   AND source.source_key = albums.source_key
             ) WHERE EXISTS (
                 SELECT 1 FROM released.user_ratings AS rating
                 JOIN sources AS source ON source.object_id = rating.source_id
                 WHERE rating.item_kind = 'album'
                   AND rating.item_id = albums.object_id
                   AND source.source_key = albums.source_key
             );
             UPDATE artists SET user_rating = (
                 SELECT rating * 10 FROM released.user_ratings AS rating
                 JOIN sources AS source ON source.object_id = rating.source_id
                 WHERE rating.item_kind = 'artist'
                   AND rating.item_id = artists.object_id
                   AND source.source_key = artists.source_key
             ) WHERE EXISTS (
                 SELECT 1 FROM released.user_ratings AS rating
                 JOIN sources AS source ON source.object_id = rating.source_id
                 WHERE rating.item_kind = 'artist'
                   AND rating.item_id = artists.object_id
                   AND source.source_key = artists.source_key
             )",
        ),
        (
            "favorite outbox",
            "INSERT INTO favorite_outbox(
                 source_key, entity_kind, entity_key, favorite,
                 previous_favorite, attempts, next_attempt_at
             )
             SELECT source.source_key, pending.item_kind,
                    COALESCE(track.track_key, album.album_key, artist.artist_key),
                    pending.favorite, pending.previous_favorite,
                    pending.attempts, pending.next_attempt_at
             FROM released.pending_favorites AS pending
             JOIN sources AS source ON source.object_id=pending.source_id
             LEFT JOIN tracks AS track
               ON pending.item_kind='track' AND track.source_key=source.source_key
              AND track.object_id=pending.item_id
             LEFT JOIN albums AS album
               ON pending.item_kind='album' AND album.source_key=source.source_key
              AND album.object_id=pending.item_id
             LEFT JOIN artists AS artist
               ON pending.item_kind='artist' AND artist.source_key=source.source_key
              AND artist.object_id=pending.item_id
             WHERE COALESCE(track.track_key, album.album_key, artist.artist_key) IS NOT NULL",
        ),
        (
            "smart playlists",
            "INSERT INTO smart_playlists(
                 source_key, object_id, name, normalized_name, definition_json, position
             )
             SELECT source.source_key, item.smart_playlist_id, item.name,
                    lower(item.name), item.definition_json, item.position
             FROM released.smart_playlists AS item
             JOIN sources AS source ON source.object_id = item.source_id",
        ),
        (
            "loudness",
            "INSERT INTO loudness_measurements(
                 source_key, entity_kind, entity_key, analysis_key,
                 integrated_lufs, true_peak
             )
             SELECT source.source_key, item.scope,
                    CASE item.scope WHEN 'track' THEN track.track_key ELSE album.album_key END,
                    item.analysis_key, item.integrated_lufs, item.true_peak
             FROM released.loudness_measurements AS item
             JOIN sources AS source ON source.object_id = item.source_id
             LEFT JOIN tracks AS track
               ON item.scope = 'track' AND track.source_key = source.source_key
              AND track.object_id = item.item_id
             LEFT JOIN albums AS album
               ON item.scope = 'album' AND album.source_key = source.source_key
              AND album.object_id = item.item_id
             WHERE track.track_key IS NOT NULL OR album.album_key IS NOT NULL",
        ),
        (
            "activity baseline",
            "INSERT INTO activity_baseline(
                 source_key, period, item_kind, track_object_id,
                 play_count, skip_count, last_played_at
             )
             SELECT source.source_key, item.period, item.item_kind,
                    item.item_id, item.play_count,
                    COALESCE(item.skip_count, 0), item.last_played_at
             FROM released.listening_aggregates AS item
             JOIN sources AS source ON source.object_id=item.source_id
             WHERE item.period='lifetime'
                OR (length(item.period)=7 AND substr(item.period,5,1)='-')",
        ),
        (
            "recent listens",
             "INSERT INTO listens(
                 external_id, source_key, track_key, track_object_id, track_title,
                 artist_name, album_title, started_at, local_period, duration_millis, listened_millis, skipped
             )
             SELECT item.play_id, source.source_key, track.track_key, item.track_id,
                    item.track_title, item.artist_name, COALESCE(item.album_title, ''),
                    item.played_at, strftime('%Y-%m',item.played_at,'unixepoch'), COALESCE(track.duration_millis, 0), 0, 0
             FROM released.recent_plays AS item
             JOIN sources AS source ON source.object_id=item.source_id
             LEFT JOIN tracks AS track
               ON track.source_key=source.source_key AND track.object_id=item.track_id",
        ),
        (
            "lyrics",
            "INSERT INTO lyrics_cache(
                 source_key, track_key, authority, role, language, script,
                 cache_input_digest, lyrics, updated_at
             )
             SELECT source.source_key, track.track_key,
                    item.origin, item.role, item.language, item.script,
                    item.input_digest, item.payload, item.cached_at
             FROM released.lyrics_cache AS item
             JOIN sources AS source ON source.object_id=item.source_id
             JOIN tracks AS track
               ON track.source_key=source.source_key AND track.object_id=item.track_id",
        ),
    ] {
        copy_family(transaction, report, mode, family, sql).await?;
    }
    Ok(())
}

async fn salvage_queue(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    copy_family(
        transaction,
        report,
        mode,
        "queue occurrences",
        "INSERT INTO queue_occurrences(
             source_key, object_id, position, traversal_position,
             provenance_kind, provenance_context_id, provenance_source_rank,
             track_key, track_object_id,
             fallback_title, fallback_artist, fallback_album, fallback_album_display_artist,
             fallback_album_object_id, fallback_primary_artist_object_id,
             fallback_media_uri, fallback_artwork_binding,
             fallback_duration_millis, fallback_disc_number,
             fallback_track_number, fallback_year, fallback_release_date, fallback_favorite,
             fallback_source_format, fallback_musicbrainz_recording_id,
             fallback_musicbrainz_release_track_id, fallback_musicbrainz_album_id,
             fallback_musicbrainz_release_group_id, fallback_primary_artist_musicbrainz_id,
             fallback_cue_path, fallback_cue_start_millis, fallback_cue_end_millis
         )
         SELECT source.source_key,
                json_extract(occurrence.value, '$.id'),
                occurrence.key,
                CASE
                    WHEN json_array_length(queue.traversal_json)=0 THEN occurrence.key
                    ELSE (
                        SELECT traversal.key
                        FROM json_each(queue.traversal_json) AS traversal
                        WHERE traversal.value=json_extract(occurrence.value, '$.id')
                    )
                END,
                CASE
                    WHEN json_type(occurrence.value, '$.provenance.Context') = 'object'
                        THEN 'context'
                    WHEN json_extract(occurrence.value, '$.provenance') = 'Manual'
                        THEN 'manual'
                    WHEN json_extract(occurrence.value, '$.provenance') = 'Random'
                        THEN 'random'
                    WHEN json_extract(occurrence.value, '$.provenance') = 'Radio'
                        THEN 'radio'
                    WHEN json_extract(occurrence.value, '$.provenance') = 'AutoDj'
                        THEN 'auto-dj'
                    WHEN json_extract(occurrence.value, '$.provenance') = 'Legacy'
                        THEN 'legacy'
                END,
                json_extract(
                    occurrence.value, '$.provenance.Context.context_id'
                ),
                json_extract(
                    occurrence.value, '$.provenance.Context.source_rank'
                ),
                track.track_key,
                json_extract(occurrence.value, '$.track_id'),
                json_extract(fallback.value, '$.title'),
                json_extract(fallback.value, '$.artist'),
                json_extract(fallback.value, '$.album'),
                NULL,
                json_extract(fallback.value, '$.album_id'),
                json_extract(fallback.value, '$.primary_artist_id'),
                CASE
                    WHEN source.object_id<>'local:server:library' THEN NULL
                    WHEN json_extract(fallback.value, '$.source_path') IS NULL THEN NULL
                    WHEN substr(json_extract(fallback.value, '$.source_path'),1,7)='file://'
                        THEN json_extract(fallback.value, '$.source_path')
                    ELSE 'file://' || json_extract(fallback.value, '$.source_path')
                END,
                CASE
                    WHEN json_extract(fallback.value, '$.image_ref') IS NOT NULL
                        THEN CAST(json_extract(fallback.value, '$.image_ref') AS BLOB)
                    WHEN json_extract(fallback.value, '$.local_artwork') IS NOT NULL
                        THEN CAST(json_object(
                            'File', json_object(
                                'path', json_extract(fallback.value, '$.local_artwork'),
                                'revision', 'released'
                            )
                        ) AS BLOB)
                    ELSE NULL
                END,
                json_extract(fallback.value, '$.duration_seconds') * 1000,
                json_extract(fallback.value, '$.disc_number'),
                json_extract(fallback.value, '$.track_number'),
                json_extract(fallback.value, '$.year'),
                json_extract(fallback.value, '$.release_date'),
                json_extract(fallback.value, '$.favorite'),
                json_extract(fallback.value, '$.source_format'),
                json_extract(fallback.value, '$.musicbrainz_recording_id'),
                json_extract(fallback.value, '$.musicbrainz_release_track_id'),
                json_extract(fallback.value, '$.album_artwork.musicbrainz_album_id'),
                json_extract(fallback.value, '$.album_artwork.musicbrainz_release_group_id'),
                json_extract(fallback.value, '$.relations.artists[0].musicbrainz_artist_id'),
                json_extract(fallback.value, '$.cue.cue_path'),
                json_extract(fallback.value, '$.cue.start_millis'),
                json_extract(fallback.value, '$.cue.end_millis')
         FROM released.playback_queues AS queue
         JOIN sources AS source ON source.object_id=queue.source_id
         JOIN json_each(queue.rows_json, '$.occurrences') AS occurrence
         LEFT JOIN tracks AS track
           ON track.source_key=source.source_key
          AND track.object_id=json_extract(occurrence.value, '$.track_id')
         LEFT JOIN json_each(queue.rows_json, '$.fallback_tracks') AS fallback
           ON json_extract(fallback.value, '$.id')=json_extract(occurrence.value, '$.track_id')",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "queue state",
        "INSERT INTO queue_state(
             source_key, current_occurrence_key, progress_millis, repeat_mode, shuffled
         )
         SELECT source.source_key, occurrence.queue_occurrence_key,
                state.progress_millis, 'none',
                CASE WHEN EXISTS (
                    SELECT 1
                    FROM json_each(queue.traversal_json) AS traversal
                    JOIN json_each(queue.rows_json, '$.occurrences') AS occurrence
                      ON occurrence.key=traversal.key
                    WHERE traversal.value<>json_extract(occurrence.value, '$.id')
                ) THEN 1 ELSE 0 END
         FROM released.playback_queues AS queue
         JOIN released.playback_state AS state
           ON state.source_id=queue.source_id AND state.revision=queue.revision
         JOIN sources AS source ON source.object_id=queue.source_id
         LEFT JOIN queue_occurrences AS occurrence
           ON occurrence.source_key=source.source_key
          AND occurrence.object_id=state.selected_occurrence_id",
    )
    .await?;
    Ok(())
}

async fn salvage_local(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
) -> LibraryResult<()> {
    copy_family(
        transaction,
        report,
        mode,
        "Local files",
        "INSERT INTO local_files(
             source_key, path, root, relative_path, kind, size_bytes,
             mtime_ns, device_id, inode, parse_version, state
         )
         SELECT source.source_key, item.path, item.root, item.relative_path,
                item.kind, item.size_bytes, item.mtime_ns, item.device_id,
                item.inode, item.parse_version, item.state
         FROM released.local_files AS item
         JOIN released.source_libraries AS library USING (library_id)
         JOIN sources AS source ON source.object_id=library.source_id
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id=(
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id=library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "Local file dependencies",
        "INSERT INTO local_file_dependencies(
             local_file_key, dependency_path, position
         )
         SELECT file.local_file_key, dependency.value, dependency.key
         FROM released.local_files AS item
         JOIN released.source_libraries AS library USING (library_id)
         JOIN sources AS source ON source.object_id=library.source_id
         JOIN local_files AS file
           ON file.source_key=source.source_key AND file.path=item.path
         JOIN json_each(item.dependencies_json) AS dependency
         WHERE library.accepted_at IS NOT NULL
           AND library.library_id=(
               SELECT MAX(current.library_id)
               FROM released.source_libraries AS current
               WHERE current.source_id=library.source_id
                 AND current.accepted_at IS NOT NULL
           )",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "Local access files",
        "INSERT INTO local_access_files(
             source_key, origin, path, root, relative_path, size_bytes, mtime_ns,
             device_id, inode, parser_version, title, normalized_title,
             album, normalized_album, artist, normalized_artist,
             disc_number, track_number, duration_millis, media_uri
         )
         SELECT source.source_key, 'mapping', item.path, item.root, item.relative_path,
                item.size_bytes, item.mtime_ns, item.device_id, item.inode,
                item.parser_version, item.title, lower(item.title),
                item.album, lower(item.album), item.artist, lower(item.artist),
                item.disc_number, item.track_number,
                item.duration_seconds * 1000,
                CASE WHEN substr(item.path,1,7)='file://' THEN item.path
                     ELSE 'file://' || item.path END
         FROM released.local_access_files AS item
         JOIN sources AS source ON source.object_id=item.source_id",
    )
    .await?;
    copy_family(
        transaction,
        report,
        mode,
        "listen outbox",
        "INSERT OR IGNORE INTO listens(
             external_id, source_key, track_object_id, track_title,
             artist_name, album_title, started_at, local_period, duration_millis, listened_millis, skipped
         )
         SELECT item.play_id, NULL, '', item.track_title, item.artist_name,
                COALESCE(item.album_title, ''), item.started_at,
                strftime('%Y-%m',item.started_at,'unixepoch'),
                item.duration_millis, item.duration_millis, 0
         FROM released.pending_scrobbles AS item;
         INSERT INTO listen_outbox(
             listen_key, service, account_id, attempts, next_attempt_at, last_error
         )
         SELECT listen.listen_key, item.service, item.account_id, item.attempts,
                item.next_attempt_at, item.last_error
         FROM released.pending_scrobbles AS item
         JOIN listens AS listen ON listen.external_id=item.play_id",
    )
    .await?;
    Ok(())
}

async fn copy_family(
    transaction: &mut Transaction<'_, Sqlite>,
    report: &mut RecoveryReport,
    mode: ReleasedCopyMode,
    family: &'static str,
    sql: &'static str,
) -> LibraryResult<()> {
    let before = sqlx::query_scalar::<_, i64>("SELECT total_changes()")
        .fetch_one(&mut **transaction)
        .await?;
    match sqlx::raw_sql(sql).execute(&mut **transaction).await {
        Ok(_) => {
            let after = sqlx::query_scalar::<_, i64>("SELECT total_changes()")
                .fetch_one(&mut **transaction)
                .await?;
            report.recovered_rows += usize::try_from(after - before).unwrap_or_default();
            Ok(())
        }
        Err(error) => match mode {
            ReleasedCopyMode::Migration => Err(error.into()),
            ReleasedCopyMode::Repair => {
                report.unreadable_families.push(family);
                Ok(())
            }
        },
    }
}

fn restore_preserved_store(path: &Path, preserved: &Path) -> std::io::Result<()> {
    remove_store_file(path)?;
    remove_store_sidecar(path, "-wal")?;
    remove_store_sidecar(path, "-shm")?;
    fs::rename(preserved, path)?;
    restore_sidecar(path, preserved, "-wal")?;
    restore_sidecar(path, preserved, "-shm")?;
    Ok(())
}

fn install_migrated_store(path: &Path, pending: &Path, preserved: &Path) -> std::io::Result<()> {
    let install = (|| {
        fs::rename(path, preserved)?;
        preserve_sidecar(path, preserved, "-wal")?;
        preserve_sidecar(path, preserved, "-shm")?;
        fs::rename(pending, path)?;
        preserve_sidecar(pending, path, "-wal")?;
        preserve_sidecar(pending, path, "-shm")?;
        Ok(())
    })();
    if let Err(error) = install {
        remove_store_family(path)?;
        if preserved.exists() {
            restore_preserved_store(path, preserved)?;
        }
        return Err(error);
    }
    Ok(())
}

fn remove_store_family(path: &Path) -> std::io::Result<()> {
    remove_store_file(path)?;
    remove_store_sidecar(path, "-wal")?;
    remove_store_sidecar(path, "-shm")?;
    Ok(())
}

fn remove_store_file(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn remove_store_sidecar(path: &Path, suffix: &str) -> std::io::Result<()> {
    let mut sidecar = OsString::from(path.as_os_str());
    sidecar.push(suffix);
    remove_store_file(&PathBuf::from(sidecar))
}

fn restore_sidecar(path: &Path, preserved: &Path, suffix: &str) -> std::io::Result<()> {
    let mut preserved_sidecar = OsString::from(preserved.as_os_str());
    preserved_sidecar.push(suffix);
    let preserved_sidecar = PathBuf::from(preserved_sidecar);
    if preserved_sidecar.exists() {
        let mut original_sidecar = OsString::from(path.as_os_str());
        original_sidecar.push(suffix);
        fs::rename(preserved_sidecar, PathBuf::from(original_sidecar))?;
    }
    Ok(())
}

fn preserve_sidecar(path: &Path, preserved: &Path, suffix: &str) -> std::io::Result<()> {
    let mut original = OsString::from(path.as_os_str());
    original.push(suffix);
    let original = PathBuf::from(original);
    if original.exists() {
        let mut target = OsString::from(preserved.as_os_str());
        target.push(suffix);
        fs::rename(original, PathBuf::from(target))?;
    }
    Ok(())
}

fn unique_sibling(path: &Path, suffix: &str) -> LibraryResult<PathBuf> {
    let file_name = path
        .file_name()
        .ok_or_else(|| LibraryError::InvalidStore("Store path has no file name".to_string()))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    loop {
        let mut candidate = OsString::from(file_name);
        candidate.push(format!(
            ".{suffix}-{}-{}",
            std::process::id(),
            RECOVERY_NUMBER.fetch_add(1, Ordering::Relaxed)
        ));
        let candidate = parent.join(candidate);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
}
