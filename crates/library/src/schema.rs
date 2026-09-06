//! Independent durable Store and disposable catalog schemas, joined by connection-local views.
use crate::LibraryResult;
use sqlx::{Connection, SqliteConnection};

pub(crate) async fn pragma(
    connection: &mut SqliteConnection,
    name: &'static str,
) -> LibraryResult<i64> {
    Ok(
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("PRAGMA {name}")))
            .fetch_one(connection)
            .await?,
    )
}
pub(crate) const STORE_SCHEMA: &str = r#"PRAGMA application_id = 1381320270;

PRAGMA user_version = 44;

CREATE TABLE IF NOT EXISTS source_ids (source_key INTEGER PRIMARY KEY, object_id TEXT NOT NULL UNIQUE CHECK(object_id<>'')) STRICT;

CREATE TABLE IF NOT EXISTS playlists (
    playlist_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES source_ids ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT,
    normalized_name TEXT,
    sort_text TEXT,
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (source_key, object_id)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS playlists_global_object_idx ON playlists(object_id)
    WHERE source_key IS NULL;

CREATE INDEX IF NOT EXISTS playlists_order_idx ON playlists(source_key, sort_text, playlist_key);

CREATE INDEX IF NOT EXISTS playlists_title_idx ON playlists(source_key, sort_text, playlist_key);

CREATE UNIQUE INDEX IF NOT EXISTS playlists_position_idx ON playlists(position);

CREATE TABLE IF NOT EXISTS playlist_entries (
    playlist_entry_key INTEGER PRIMARY KEY,
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    title TEXT, artist TEXT, album TEXT, album_display_artist TEXT,
    snapshot_at INTEGER NOT NULL DEFAULT (unixepoch()),
    duration_millis INTEGER, disc_number INTEGER,
    track_number INTEGER, year INTEGER, release_date TEXT,
    source_format TEXT, musicbrainz_recording_id TEXT,
    musicbrainz_release_track_id TEXT,
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (playlist_key, object_id),
    UNIQUE (playlist_key, position)
) STRICT;

CREATE INDEX IF NOT EXISTS playlist_entries_order_idx ON playlist_entries(playlist_key, position);

CREATE INDEX IF NOT EXISTS playlist_entries_media_idx ON playlist_entries(media_uri,(title IS NULL),snapshot_at DESC,playlist_entry_key DESC);

CREATE TABLE IF NOT EXISTS smart_playlists (
    smart_playlist_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    definition_json TEXT NOT NULL CHECK (json_valid(definition_json)),
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (object_id),
    UNIQUE (position)
) STRICT;

CREATE INDEX IF NOT EXISTS smart_playlists_title_idx
    ON smart_playlists(normalized_name, smart_playlist_key);

CREATE TABLE IF NOT EXISTS favorite_outbox (
    outbox_key INTEGER PRIMARY KEY,
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    favorite INTEGER NOT NULL CHECK (favorite IN (0, 1)),
    previous_favorite INTEGER NOT NULL CHECK (previous_favorite IN (0, 1)),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0)
) STRICT;

CREATE INDEX IF NOT EXISTS favorite_outbox_due_idx ON favorite_outbox(next_attempt_at, outbox_key);

CREATE TABLE IF NOT EXISTS user_media_state (
    media_uri TEXT PRIMARY KEY CHECK (media_uri <> ''),
    favorite INTEGER CHECK (favorite IS NULL OR favorite IN (0,1)),
    rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 100)
) STRICT;

CREATE TABLE IF NOT EXISTS listens (
    listen_key INTEGER PRIMARY KEY,
    external_id TEXT UNIQUE,
    source_id TEXT,
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    track_title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    disc_number INTEGER, track_number INTEGER, year INTEGER, release_date TEXT,
    source_format TEXT, musicbrainz_recording_id TEXT, musicbrainz_release_track_id TEXT,
    started_at INTEGER NOT NULL CHECK (started_at >= 0),
    local_period TEXT NOT NULL CHECK (
        length(local_period)=7 AND substr(local_period,5,1)='-'
        AND substr(local_period,1,4) NOT GLOB '*[^0-9]*'
        AND substr(local_period,6,2) IN ('01','02','03','04','05','06','07','08','09','10','11','12')
    ),
    duration_millis INTEGER NOT NULL CHECK (duration_millis >= 0),
    listened_millis INTEGER NOT NULL CHECK (listened_millis >= 0),
    skipped INTEGER NOT NULL DEFAULT 0 CHECK (skipped IN (0, 1))
) STRICT;

CREATE INDEX IF NOT EXISTS listens_history_idx ON listens(source_id, started_at DESC, listen_key DESC);
CREATE INDEX IF NOT EXISTS listens_recent_idx ON listens(started_at DESC, listen_key DESC);

CREATE INDEX IF NOT EXISTS listens_media_idx ON listens(media_uri, started_at DESC,listen_key DESC);

CREATE TABLE IF NOT EXISTS listen_outbox (
    outbox_key INTEGER PRIMARY KEY,
    listen_key INTEGER NOT NULL REFERENCES listens ON DELETE CASCADE,
    service TEXT NOT NULL CHECK (service <> ''),
    account_id TEXT NOT NULL CHECK (account_id <> ''),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER CHECK (next_attempt_at IS NULL OR next_attempt_at >= 0),
    last_error TEXT,
    UNIQUE (service, account_id, listen_key)
) STRICT;

CREATE INDEX IF NOT EXISTS listen_outbox_due_idx ON listen_outbox(next_attempt_at, outbox_key)
    WHERE next_attempt_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS queue_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    current_occurrence_id TEXT,
    progress_millis INTEGER NOT NULL DEFAULT 0 CHECK (progress_millis >= 0),
    repeat_mode TEXT NOT NULL DEFAULT 'none' CHECK (repeat_mode IN ('none', 'one', 'all')),
    shuffled INTEGER NOT NULL DEFAULT 0 CHECK (shuffled IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS queue_occurrences (
    queue_occurrence_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    snapshot_at INTEGER NOT NULL DEFAULT (unixepoch()),
    position INTEGER NOT NULL CHECK (position >= 0),
    traversal_position INTEGER NOT NULL CHECK (traversal_position >= 0),
    provenance_kind TEXT NOT NULL CHECK (
        provenance_kind IN ('context', 'manual', 'random', 'radio', 'auto-dj', 'legacy')
    ),
    provenance_context_id TEXT,
    provenance_source_rank INTEGER,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT NOT NULL,
    album_display_artist TEXT,
    duration_millis INTEGER NOT NULL CHECK (duration_millis >= 0),
    disc_number INTEGER,
    track_number INTEGER,
    year INTEGER,
    release_date TEXT,
    source_format TEXT,
    musicbrainz_recording_id TEXT,
    musicbrainz_release_track_id TEXT,
    musicbrainz_album_id TEXT,
    musicbrainz_release_group_id TEXT,
    primary_artist_musicbrainz_id TEXT,
    CHECK (disc_number IS NULL OR disc_number >= 0),
    CHECK (track_number IS NULL OR track_number >= 0),
    CHECK (
        musicbrainz_recording_id IS NULL OR musicbrainz_recording_id <> ''
    ),
    CHECK (
        (
            provenance_kind = 'context'
            AND provenance_context_id IS NOT NULL
            AND provenance_context_id <> ''
            AND provenance_source_rank IS NOT NULL
            AND provenance_source_rank >= 0
        )
        OR (
            provenance_kind <> 'context'
            AND provenance_context_id IS NULL
            AND provenance_source_rank IS NULL
        )
    ),
    UNIQUE (object_id)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS queue_occurrences_page_idx
    ON queue_occurrences(position);

CREATE UNIQUE INDEX IF NOT EXISTS queue_occurrences_traversal_idx
    ON queue_occurrences(traversal_position);

CREATE INDEX IF NOT EXISTS queue_occurrences_media_idx ON queue_occurrences(media_uri,snapshot_at DESC,queue_occurrence_key DESC);

CREATE TABLE IF NOT EXISTS local_locators (
    local_access_file_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES source_ids ON DELETE CASCADE,
    media_uri TEXT NOT NULL CHECK(media_uri<>''),
    origin TEXT NOT NULL CHECK(origin IN ('local','mapping','download','import')),
    path TEXT NOT NULL CHECK(path<>''), root TEXT NOT NULL, relative_path TEXT NOT NULL,
    access_uri TEXT NOT NULL CHECK(access_uri<>''),
    UNIQUE(media_uri,origin), UNIQUE(source_key,path)
) STRICT;
CREATE INDEX IF NOT EXISTS local_locators_source_idx ON local_locators(source_key,origin);
CREATE INDEX IF NOT EXISTS local_locators_access_idx ON local_locators(media_uri,origin,local_access_file_key);

CREATE INDEX IF NOT EXISTS queue_occurrences_context_idx ON queue_occurrences(provenance_context_id,provenance_source_rank,media_uri);

CREATE TABLE IF NOT EXISTS legacy_activity (
    source_id TEXT NOT NULL, period TEXT NOT NULL, item_kind TEXT NOT NULL,
    track_object_id TEXT NOT NULL, play_count INTEGER NOT NULL, skip_count INTEGER NOT NULL,
    last_played_at INTEGER,
    PRIMARY KEY(source_id,period,item_kind,track_object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS local_locators_precedence_idx ON local_locators(media_uri,CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local_access_file_key);
"#;
pub(crate) const CATALOG_SCHEMA: &str = r#"PRAGMA application_id = 1381320270;

PRAGMA user_version = 45;

CREATE TABLE IF NOT EXISTS sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    display_name TEXT NOT NULL, normalized_name TEXT NOT NULL, freshness BLOB,
    catalog_digest BLOB NOT NULL CHECK (length(catalog_digest) = 32),
    artwork_digest BLOB NOT NULL CHECK (length(artwork_digest) = 32),
    distinct_track_covers INTEGER NOT NULL DEFAULT 0 CHECK (distinct_track_covers IN (0,1)),
    catalog_revision INTEGER NOT NULL DEFAULT 0 CHECK (catalog_revision >= 0)
) STRICT;

CREATE TABLE IF NOT EXISTS albums (
    album_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    title TEXT NOT NULL,
    normalized_title TEXT NOT NULL,
    display_artist TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    year INTEGER,
    release_date TEXT,
    date_added TEXT,
    musicbrainz_release_id TEXT,
    musicbrainz_release_group_id TEXT,
    is_compilation INTEGER CHECK (is_compilation IS NULL OR is_compilation IN (0,1)),
    release_lookup_identity TEXT CHECK (
        release_lookup_identity IS NULL OR release_lookup_identity <> ''
    ),
    artwork_binding BLOB,
    source_favorite INTEGER NOT NULL DEFAULT 0 CHECK (source_favorite IN (0, 1)),
    source_rating INTEGER CHECK (source_rating IS NULL OR source_rating BETWEEN 0 AND 100),
    first_seen_at INTEGER,
    source_loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(source_loudness_analysis_key) = 32),
    loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(loudness_analysis_key) = 32),
    CHECK (musicbrainz_release_id IS NULL OR musicbrainz_release_id <> ''),
    CHECK (
        musicbrainz_release_group_id IS NULL OR musicbrainz_release_group_id <> ''
    ),
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS albums_order_idx ON albums(source_key, sort_text, album_key);

CREATE INDEX IF NOT EXISTS albums_key_idx ON albums(source_key, album_key);

CREATE INDEX IF NOT EXISTS albums_artwork_idx ON albums(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS tracks (
    track_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    album_key INTEGER REFERENCES albums ON DELETE SET NULL,
    title TEXT NOT NULL,
    normalized_search TEXT NOT NULL,
    display_album TEXT NOT NULL,
    display_artist TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    duration_millis INTEGER NOT NULL CHECK (duration_millis >= 0),
    disc_number INTEGER NOT NULL DEFAULT 0 CHECK (disc_number >= 0),
    track_number INTEGER NOT NULL DEFAULT 0 CHECK (track_number >= 0),
    year INTEGER,
    release_date TEXT,
    date_added TEXT,
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    source_path TEXT,
    source_format TEXT,
    comment TEXT,
    bpm INTEGER,
    musicbrainz_recording_id TEXT,
    musicbrainz_release_track_id TEXT,
    cue_path TEXT,
    cue_start_millis INTEGER,
    cue_end_millis INTEGER,
    artwork_binding BLOB,
    source_favorite INTEGER NOT NULL DEFAULT 0 CHECK (source_favorite IN (0, 1)),
    source_rating INTEGER CHECK (source_rating IS NULL OR source_rating BETWEEN 0 AND 100),
    first_seen_at INTEGER,
    source_loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(source_loudness_analysis_key) = 32),
    loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(loudness_analysis_key) = 32),
    CHECK (bpm IS NULL OR bpm BETWEEN 0 AND 65535),
    CHECK (musicbrainz_recording_id IS NULL OR musicbrainz_recording_id <> ''),
    CHECK (
        musicbrainz_release_track_id IS NULL OR musicbrainz_release_track_id <> ''
    ),
    CHECK (
        (cue_path IS NULL AND cue_start_millis IS NULL AND cue_end_millis IS NULL)
        OR (
            cue_path IS NOT NULL AND cue_path <> ''
            AND cue_start_millis IS NOT NULL AND cue_start_millis >= 0
            AND cue_end_millis IS NOT NULL AND cue_end_millis > cue_start_millis
        )
    ),
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS tracks_order_idx ON tracks(source_key, sort_text, track_key);

CREATE INDEX IF NOT EXISTS tracks_key_idx ON tracks(source_key, track_key);
CREATE INDEX IF NOT EXISTS tracks_source_path_idx ON tracks(source_key, source_path, album_key);

CREATE INDEX IF NOT EXISTS tracks_source_cue_idx ON tracks(source_key, cue_path, object_id) WHERE cue_path IS NOT NULL;

CREATE INDEX IF NOT EXISTS tracks_artwork_idx ON tracks(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE INDEX IF NOT EXISTS tracks_album_idx ON tracks(source_key, album_key, disc_number, track_number, track_key);

CREATE TABLE IF NOT EXISTS artists (
    artist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    musicbrainz_artist_id TEXT,
    artwork_binding BLOB,
    source_favorite INTEGER NOT NULL DEFAULT 0 CHECK (source_favorite IN (0, 1)),
    source_rating INTEGER CHECK (source_rating IS NULL OR source_rating BETWEEN 0 AND 100),
    CHECK (musicbrainz_artist_id IS NULL OR musicbrainz_artist_id <> ''),
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS artists_order_idx ON artists(source_key, sort_text, artist_key);

CREATE INDEX IF NOT EXISTS artists_artwork_idx ON artists(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS genres (
    genre_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS genres_order_idx ON genres(source_key, sort_text, genre_key);

CREATE INDEX IF NOT EXISTS genres_artwork_idx ON genres(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS moods (
    mood_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS moods_order_idx ON moods(source_key, sort_text, mood_key);

CREATE TABLE IF NOT EXISTS folders (
    folder_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS folders_order_idx ON folders(source_key, sort_text, folder_key);

CREATE INDEX IF NOT EXISTS folders_artwork_idx ON folders(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS album_artists (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, artist_key)
) STRICT;

CREATE INDEX IF NOT EXISTS album_artists_artist_idx ON album_artists(artist_key, album_key);

CREATE TABLE IF NOT EXISTS track_artists (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, artist_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_artists_artist_idx ON track_artists(artist_key, track_key);

CREATE TABLE IF NOT EXISTS album_genres (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, genre_key)
) STRICT;

CREATE INDEX IF NOT EXISTS album_genres_genre_idx ON album_genres(genre_key, album_key);

CREATE TABLE IF NOT EXISTS track_genres (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, genre_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_genres_genre_idx ON track_genres(genre_key, track_key);

CREATE TABLE IF NOT EXISTS track_moods (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    mood_key INTEGER NOT NULL REFERENCES moods ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, mood_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_moods_mood_idx ON track_moods(mood_key, track_key);

CREATE TABLE IF NOT EXISTS track_folders (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    folder_key INTEGER NOT NULL REFERENCES folders ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, folder_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_folders_folder_idx ON track_folders(folder_key, track_key);

CREATE TABLE IF NOT EXISTS album_release_types (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    release_type TEXT NOT NULL CHECK (release_type <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, release_type)
) STRICT;

CREATE TABLE IF NOT EXISTS native_playlists (
    playlist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS native_playlists_order_idx ON native_playlists(source_key, sort_text, playlist_key);

CREATE INDEX IF NOT EXISTS native_playlists_title_idx ON native_playlists(source_key, sort_text, playlist_key);

CREATE TABLE IF NOT EXISTS native_playlist_entries (
    playlist_entry_key INTEGER PRIMARY KEY,
    playlist_key INTEGER NOT NULL REFERENCES native_playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    title TEXT, artist TEXT, album TEXT, album_display_artist TEXT,
    snapshot_at INTEGER NOT NULL DEFAULT (unixepoch()),
    duration_millis INTEGER, disc_number INTEGER,
    track_number INTEGER, year INTEGER, release_date TEXT,
    source_format TEXT, musicbrainz_recording_id TEXT,
    musicbrainz_release_track_id TEXT,
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (playlist_key, object_id),
    UNIQUE (playlist_key, position)
) STRICT;

CREATE INDEX IF NOT EXISTS native_playlist_entries_order_idx ON native_playlist_entries(playlist_key, position);

CREATE INDEX IF NOT EXISTS native_playlist_entries_media_idx ON native_playlist_entries(media_uri,(title IS NULL),snapshot_at DESC,playlist_entry_key ASC);

CREATE TABLE IF NOT EXISTS home_entries (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    section_id TEXT NOT NULL CHECK (section_id <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album', 'artist', 'playlist')),
    entity_key INTEGER NOT NULL,
    title TEXT NOT NULL,
    subtitle TEXT NOT NULL,
    PRIMARY KEY (source_key, section_id, position)
) STRICT;

CREATE TABLE IF NOT EXISTS activity_baseline (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    period TEXT NOT NULL DEFAULT 'lifetime',
    item_kind TEXT NOT NULL DEFAULT 'track' CHECK (item_kind IN ('track','artist','genre')),
    track_object_id TEXT NOT NULL CHECK (track_object_id <> ''),
    play_count INTEGER NOT NULL CHECK (play_count >= 0),
    skip_count INTEGER NOT NULL CHECK (skip_count >= 0),
    last_played_at INTEGER,
    PRIMARY KEY (source_key, period, item_kind, track_object_id)
) STRICT;

CREATE TABLE IF NOT EXISTS loudness_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album')),
    entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    integrated_lufs REAL,
    true_peak REAL CHECK (true_peak IS NULL OR true_peak >= 0),
    origin TEXT NOT NULL DEFAULT 'source' CHECK (origin IN ('source', 'analysis')),
    PRIMARY KEY (source_key, entity_kind, entity_key)
) STRICT;

CREATE TABLE IF NOT EXISTS replay_gain_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album')),
    entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    gain_db REAL NOT NULL,
    peak REAL CHECK (peak IS NULL OR peak >= 0),
    PRIMARY KEY (source_key, entity_kind, entity_key)
) STRICT;

CREATE TABLE IF NOT EXISTS lyrics_cache (
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    authority TEXT NOT NULL CHECK (authority <> ''),
    role TEXT NOT NULL CHECK (role <> ''),
    language TEXT NOT NULL DEFAULT '',
    script TEXT NOT NULL DEFAULT '',
    cache_input_digest BLOB NOT NULL CHECK (length(cache_input_digest) = 32),
    lyrics TEXT NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY (media_uri, authority, role, language, script)
) STRICT;

CREATE TABLE IF NOT EXISTS local_files (
    local_file_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    relative_path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('media', 'cue', 'image', 'directory')),
    size_bytes INTEGER,
    mtime_ns INTEGER NOT NULL,
    device_id INTEGER,
    inode INTEGER,
    native_id TEXT,
    picture_index INTEGER,
    revision TEXT,
    parse_version INTEGER,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'rejected', 'unreadable', 'observed')),
    UNIQUE (source_key, path)
) STRICT;

CREATE INDEX IF NOT EXISTS local_files_identity_idx ON local_files(source_key, device_id, inode);

CREATE INDEX IF NOT EXISTS local_files_native_id_idx ON local_files(source_key, native_id) WHERE native_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS local_files_kind_path_idx ON local_files(source_key, kind, path);

CREATE TABLE IF NOT EXISTS local_file_dependencies (
    local_file_key INTEGER NOT NULL REFERENCES local_files ON DELETE CASCADE,
    dependency_path TEXT NOT NULL CHECK (dependency_path <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (local_file_key, position),
    UNIQUE (local_file_key, dependency_path)
) STRICT;

CREATE TABLE IF NOT EXISTS local_access_metadata (
    access_uri TEXT PRIMARY KEY,
    size_bytes INTEGER NOT NULL, mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
    parser_version INTEGER NOT NULL, title TEXT NOT NULL, normalized_title TEXT NOT NULL,
    album TEXT NOT NULL, normalized_album TEXT NOT NULL, artist TEXT NOT NULL,
    normalized_artist TEXT NOT NULL, disc_number INTEGER NOT NULL, track_number INTEGER NOT NULL,
    duration_millis INTEGER NOT NULL, loudness_analysis_key BLOB
) STRICT;

CREATE INDEX IF NOT EXISTS local_access_match_uri_idx ON local_access_metadata(normalized_title,normalized_album,normalized_artist,disc_number,track_number,duration_millis,access_uri);
"#;

pub(crate) async fn initialize_durable(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut transaction = connection.begin().await?;
    sqlx::raw_sql(STORE_SCHEMA)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}
pub(crate) async fn initialize_catalog(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut transaction = connection.begin().await?;
    // Preserve the accepted Local inventory when adding remote-file observations.
    let has_files: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='local_files' AND type='table')",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if has_files {
        let has_revision: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('local_files') WHERE name='revision')",
        )
        .fetch_one(&mut *transaction)
        .await?;
        if !has_revision {
            sqlx::raw_sql("ALTER TABLE local_files ADD COLUMN native_id TEXT; ALTER TABLE local_files ADD COLUMN revision TEXT;")
                .execute(&mut *transaction).await?;
        }
        let has_picture: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pragma_table_info('local_files') WHERE name='picture_index')").fetch_one(&mut *transaction).await?;
        if !has_picture {
            sqlx::raw_sql("ALTER TABLE local_files ADD COLUMN picture_index INTEGER;")
                .execute(&mut *transaction)
                .await?;
        }
    }
    sqlx::raw_sql(CATALOG_SCHEMA)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}
pub(crate) async fn attach_catalog(
    connection: &mut SqliteConnection,
    path: &std::path::Path,
) -> LibraryResult<()> {
    sqlx::query("ATTACH DATABASE ?1 AS catalog")
        .bind(path.to_string_lossy().as_ref())
        .execute(&mut *connection)
        .await?;
    sqlx::raw_sql(CONNECTION_VIEWS)
        .execute(&mut *connection)
        .await?;
    Ok(())
}

const CONNECTION_VIEWS: &str = r#"CREATE TEMP VIEW playlists AS
          SELECT owned.playlist_key,source.source_key,owned.object_id,owned.name,
                 owned.normalized_name,owned.sort_text,owned.position,NULL artwork_binding
          FROM main.playlists owned
          LEFT JOIN main.source_ids identity ON identity.source_key=owned.source_key
          LEFT JOIN catalog.sources source ON source.object_id=identity.object_id
          WHERE owned.name IS NOT NULL
          UNION ALL
          SELECT -observed.playlist_key,observed.source_key,observed.object_id,
                 observed.name,observed.normalized_name,observed.sort_text,
                 COALESCE(identity.position,(SELECT COALESCE(max(position),0) FROM main.playlists)+observed.playlist_key),
                 observed.artwork_binding
          FROM catalog.native_playlists observed
          JOIN catalog.sources source ON source.source_key=observed.source_key
          LEFT JOIN main.source_ids durable ON durable.object_id=source.object_id
          LEFT JOIN main.playlists identity ON identity.source_key=durable.source_key
            AND identity.object_id=observed.object_id AND identity.name IS NULL;
        CREATE TEMP VIEW playlist_entries AS
          SELECT * FROM main.playlist_entries
          UNION ALL
          SELECT -playlist_entry_key,-playlist_key,object_id,media_uri,title,artist,album,
                 album_display_artist,snapshot_at,duration_millis,disc_number,track_number,
                 year,release_date,source_format,musicbrainz_recording_id,
                 musicbrainz_release_track_id,position FROM catalog.native_playlist_entries;
        CREATE TEMP VIEW local_access_files AS
          SELECT locator.local_access_file_key,source.source_key,locator.media_uri,
                 locator.origin,locator.path,locator.root,locator.relative_path,
                 COALESCE(metadata.size_bytes,0) size_bytes,COALESCE(metadata.mtime_ns,0) mtime_ns,
                 metadata.device_id,metadata.inode,COALESCE(metadata.parser_version,1) parser_version,
                 COALESCE(metadata.title,'') title,COALESCE(metadata.normalized_title,'') normalized_title,
                 COALESCE(metadata.album,'') album,COALESCE(metadata.normalized_album,'') normalized_album,
                 COALESCE(metadata.artist,'') artist,COALESCE(metadata.normalized_artist,'') normalized_artist,
                 COALESCE(metadata.disc_number,0) disc_number,COALESCE(metadata.track_number,0) track_number,
                 COALESCE(metadata.duration_millis,0) duration_millis,locator.access_uri,
                 metadata.loudness_analysis_key
          FROM main.local_locators locator
          LEFT JOIN main.source_ids identity ON identity.source_key=locator.source_key
          LEFT JOIN catalog.sources source ON source.object_id=identity.object_id
          LEFT JOIN catalog.local_access_metadata metadata USING(access_uri);
CREATE TEMP VIEW activity_baseline AS
  SELECT * FROM catalog.activity_baseline
  UNION ALL
  SELECT source.source_key,old.period,old.item_kind,old.track_object_id,old.play_count,
         old.skip_count,old.last_played_at
  FROM main.legacy_activity old JOIN catalog.sources source ON source.object_id=old.source_id
  WHERE NOT EXISTS (SELECT 1 FROM catalog.activity_baseline current
    WHERE current.source_key=source.source_key AND current.period=old.period
      AND current.item_kind=old.item_kind AND current.track_object_id=old.track_object_id);
"#;
