//! Owns the fresh Library schema, current-format migrations, and the old-format migration chain.

use std::collections::BTreeSet;

use sqlx::{Connection, Sqlite, SqliteConnection, Transaction};

use crate::{LibraryError, LibraryResult};

pub(crate) const APPLICATION_ID: i64 = 1_381_320_270;
pub(crate) const SCHEMA_VERSION: i64 = 42;
pub(crate) const LAST_LEGACY_SCHEMA_VERSION: i64 = 40;
const FIRST_LEGACY_SCHEMA_VERSION: i64 = 32;
const FIRST_CURRENT_FORMAT_SCHEMA_VERSION: i64 = 41;
const MUSIC_FOLDER_ARTWORK_SCHEMA_VERSION: i64 = 33;
const FILESYSTEM_IDENTITY_SCHEMA_VERSION: i64 = 34;
const RECENT_PLAYS_SCHEMA_VERSION: i64 = 35;
const PENDING_FAVORITES_SCHEMA_VERSION: i64 = 36;
const LOUDNESS_MEASUREMENTS_SCHEMA_VERSION: i64 = 37;
const MEDIA_STATE_SCHEMA_VERSION: i64 = 38;
const USER_RATINGS_SCHEMA_VERSION: i64 = 39;
const CATALOG_OWNERS_SCHEMA_VERSION: i64 = 41;
const REPLAY_GAIN_SCHEMA_VERSION: i64 = 42;

struct SchemaMigration {
    from_version: i64,
    to_version: i64,
}

const LEGACY_MIGRATIONS: &[SchemaMigration] = &[
    SchemaMigration {
        from_version: FIRST_LEGACY_SCHEMA_VERSION,
        to_version: MUSIC_FOLDER_ARTWORK_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: MUSIC_FOLDER_ARTWORK_SCHEMA_VERSION,
        to_version: FILESYSTEM_IDENTITY_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: FILESYSTEM_IDENTITY_SCHEMA_VERSION,
        to_version: RECENT_PLAYS_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: RECENT_PLAYS_SCHEMA_VERSION,
        to_version: PENDING_FAVORITES_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: PENDING_FAVORITES_SCHEMA_VERSION,
        to_version: LOUDNESS_MEASUREMENTS_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: LOUDNESS_MEASUREMENTS_SCHEMA_VERSION,
        to_version: MEDIA_STATE_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: MEDIA_STATE_SCHEMA_VERSION,
        to_version: USER_RATINGS_SCHEMA_VERSION,
    },
    SchemaMigration {
        from_version: USER_RATINGS_SCHEMA_VERSION,
        to_version: LAST_LEGACY_SCHEMA_VERSION,
    },
];

const CURRENT_MIGRATIONS: &[SchemaMigration] = &[SchemaMigration {
    from_version: CATALOG_OWNERS_SCHEMA_VERSION,
    to_version: REPLAY_GAIN_SCHEMA_VERSION,
}];

pub(crate) const TABLES: &[&str] = &[
    "sources",
    "tracks",
    "albums",
    "artists",
    "genres",
    "moods",
    "folders",
    "album_artists",
    "track_artists",
    "album_genres",
    "track_genres",
    "track_moods",
    "track_folders",
    "album_release_types",
    "playlists",
    "playlist_entries",
    "smart_playlists",
    "home_entries",
    "favorite_outbox",
    "listens",
    "activity_baseline",
    "listen_outbox",
    "queue_state",
    "queue_occurrences",
    "loudness_measurements",
    "replay_gain_measurements",
    "lyrics_cache",
    "local_files",
    "local_file_dependencies",
    "local_access_files",
];

const FRESH_SCHEMA: &str = r###"
BEGIN IMMEDIATE;
PRAGMA application_id = 1381320270;
PRAGMA user_version = 42;

CREATE TABLE sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    display_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    freshness BLOB,
    catalog_digest BLOB NOT NULL CHECK (length(catalog_digest) = 32),
    artwork_digest BLOB NOT NULL CHECK (length(artwork_digest) = 32),
    catalog_revision INTEGER NOT NULL DEFAULT 0 CHECK (catalog_revision >= 0)
) STRICT;

CREATE TABLE albums (
    album_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
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
    user_favorite INTEGER CHECK (user_favorite IS NULL OR user_favorite IN (0, 1)),
    source_rating INTEGER CHECK (source_rating IS NULL OR source_rating BETWEEN 0 AND 100),
    user_rating INTEGER CHECK (user_rating IS NULL OR user_rating BETWEEN 0 AND 100),
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
CREATE INDEX albums_order_idx ON albums(source_key, sort_text, album_key);
CREATE INDEX albums_key_idx ON albums(source_key, album_key);
CREATE INDEX albums_artwork_idx ON albums(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE tracks (
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
    media_uri TEXT,
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
    user_favorite INTEGER CHECK (user_favorite IS NULL OR user_favorite IN (0, 1)),
    source_rating INTEGER CHECK (source_rating IS NULL OR source_rating BETWEEN 0 AND 100),
    user_rating INTEGER CHECK (user_rating IS NULL OR user_rating BETWEEN 0 AND 100),
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
CREATE INDEX tracks_order_idx ON tracks(source_key, sort_text, track_key);
CREATE INDEX tracks_key_idx ON tracks(source_key, track_key);
CREATE INDEX tracks_artwork_idx ON tracks(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;
CREATE INDEX tracks_album_idx ON tracks(source_key, album_key, disc_number, track_number, track_key);

CREATE TABLE artists (
    artist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    musicbrainz_artist_id TEXT,
    artwork_binding BLOB,
    source_favorite INTEGER NOT NULL DEFAULT 0 CHECK (source_favorite IN (0, 1)),
    user_favorite INTEGER CHECK (user_favorite IS NULL OR user_favorite IN (0, 1)),
    source_rating INTEGER CHECK (source_rating IS NULL OR source_rating BETWEEN 0 AND 100),
    user_rating INTEGER CHECK (user_rating IS NULL OR user_rating BETWEEN 0 AND 100),
    CHECK (musicbrainz_artist_id IS NULL OR musicbrainz_artist_id <> ''),
    UNIQUE (source_key, object_id)
) STRICT;
CREATE INDEX artists_order_idx ON artists(source_key, sort_text, artist_key);
CREATE INDEX artists_artwork_idx ON artists(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE genres (
    genre_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, object_id)
) STRICT;
CREATE INDEX genres_order_idx ON genres(source_key, sort_text, genre_key);
CREATE INDEX genres_artwork_idx ON genres(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE moods (
    mood_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    UNIQUE (source_key, object_id)
) STRICT;
CREATE INDEX moods_order_idx ON moods(source_key, sort_text, mood_key);

CREATE TABLE folders (
    folder_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, object_id)
) STRICT;
CREATE INDEX folders_order_idx ON folders(source_key, sort_text, folder_key);
CREATE INDEX folders_artwork_idx ON folders(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE album_artists (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, artist_key)
) STRICT;
CREATE INDEX album_artists_artist_idx ON album_artists(artist_key, album_key);

CREATE TABLE track_artists (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, artist_key)
) STRICT;
CREATE INDEX track_artists_artist_idx ON track_artists(artist_key, track_key);

CREATE TABLE album_genres (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, genre_key)
) STRICT;
CREATE INDEX album_genres_genre_idx ON album_genres(genre_key, album_key);

CREATE TABLE track_genres (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, genre_key)
) STRICT;
CREATE INDEX track_genres_genre_idx ON track_genres(genre_key, track_key);

CREATE TABLE track_moods (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    mood_key INTEGER NOT NULL REFERENCES moods ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, mood_key)
) STRICT;
CREATE INDEX track_moods_mood_idx ON track_moods(mood_key, track_key);

CREATE TABLE track_folders (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    folder_key INTEGER NOT NULL REFERENCES folders ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, folder_key)
) STRICT;
CREATE INDEX track_folders_folder_idx ON track_folders(folder_key, track_key);

CREATE TABLE album_release_types (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    release_type TEXT NOT NULL CHECK (release_type <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, release_type)
) STRICT;

CREATE TABLE playlists (
    playlist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    ownership TEXT NOT NULL CHECK (ownership IN ('source', 'user')),
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, ownership, object_id)
) STRICT;
CREATE INDEX playlists_order_idx ON playlists(source_key, ownership, sort_text, playlist_key);
CREATE INDEX playlists_title_idx ON playlists(source_key, sort_text, playlist_key);
CREATE INDEX playlists_artwork_idx ON playlists(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE playlist_entries (
    playlist_entry_key INTEGER PRIMARY KEY,
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    track_key INTEGER REFERENCES tracks ON DELETE SET NULL,
    track_object_id TEXT NOT NULL CHECK (track_object_id <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (playlist_key, object_id),
    UNIQUE (playlist_key, position)
) STRICT;
CREATE INDEX playlist_entries_order_idx ON playlist_entries(playlist_key, position);

CREATE TABLE smart_playlists (
    smart_playlist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    definition_json TEXT NOT NULL CHECK (json_valid(definition_json)),
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (source_key, object_id),
    UNIQUE (source_key, position)
) STRICT;
CREATE INDEX smart_playlists_title_idx
    ON smart_playlists(source_key, normalized_name, smart_playlist_key);

CREATE TABLE home_entries (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    section_id TEXT NOT NULL CHECK (section_id <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album', 'artist', 'playlist')),
    entity_key INTEGER NOT NULL,
    title TEXT NOT NULL,
    subtitle TEXT NOT NULL,
    artwork_binding BLOB,
    PRIMARY KEY (source_key, section_id, position)
) STRICT;
CREATE INDEX home_entries_artwork_idx ON home_entries(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE favorite_outbox (
    outbox_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album', 'artist')),
    entity_key INTEGER NOT NULL,
    favorite INTEGER NOT NULL CHECK (favorite IN (0, 1)),
    previous_favorite INTEGER NOT NULL CHECK (previous_favorite IN (0, 1)),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0),
    UNIQUE (source_key, entity_kind, entity_key)
) STRICT;
CREATE INDEX favorite_outbox_due_idx ON favorite_outbox(next_attempt_at, outbox_key);

CREATE TABLE listens (
    listen_key INTEGER PRIMARY KEY,
    external_id TEXT UNIQUE,
    source_key INTEGER REFERENCES sources ON DELETE SET NULL,
    track_key INTEGER REFERENCES tracks ON DELETE SET NULL,
    track_object_id TEXT NOT NULL,
    track_title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
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
CREATE INDEX listens_history_idx ON listens(source_key, started_at DESC, listen_key DESC);
CREATE INDEX listens_track_idx ON listens(source_key, track_key, started_at DESC);

CREATE TABLE activity_baseline (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    period TEXT NOT NULL DEFAULT 'lifetime',
    item_kind TEXT NOT NULL DEFAULT 'track' CHECK (item_kind IN ('track','artist','genre')),
    track_object_id TEXT NOT NULL CHECK (track_object_id <> ''),
    play_count INTEGER NOT NULL CHECK (play_count >= 0),
    skip_count INTEGER NOT NULL CHECK (skip_count >= 0),
    last_played_at INTEGER,
    PRIMARY KEY (source_key, period, item_kind, track_object_id)
) STRICT;

CREATE TABLE listen_outbox (
    outbox_key INTEGER PRIMARY KEY,
    listen_key INTEGER NOT NULL REFERENCES listens ON DELETE CASCADE,
    service TEXT NOT NULL CHECK (service <> ''),
    account_id TEXT NOT NULL CHECK (account_id <> ''),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER CHECK (next_attempt_at IS NULL OR next_attempt_at >= 0),
    last_error TEXT,
    UNIQUE (service, account_id, listen_key)
) STRICT;
CREATE INDEX listen_outbox_due_idx ON listen_outbox(next_attempt_at, outbox_key)
    WHERE next_attempt_at IS NOT NULL;

CREATE TABLE queue_state (
    source_key INTEGER PRIMARY KEY REFERENCES sources ON DELETE CASCADE,
    current_occurrence_key INTEGER,
    prepared_next_occurrence_key INTEGER,
    progress_millis INTEGER NOT NULL DEFAULT 0 CHECK (progress_millis >= 0),
    repeat_mode TEXT NOT NULL DEFAULT 'none' CHECK (repeat_mode IN ('none', 'one', 'all')),
    shuffled INTEGER NOT NULL DEFAULT 0 CHECK (shuffled IN (0, 1))
) STRICT;

CREATE TABLE queue_occurrences (
    queue_occurrence_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    traversal_position INTEGER NOT NULL CHECK (traversal_position >= 0),
    provenance_kind TEXT NOT NULL CHECK (
        provenance_kind IN ('context', 'manual', 'random', 'radio', 'auto-dj', 'legacy')
    ),
    provenance_context_id TEXT,
    provenance_source_rank INTEGER,
    track_key INTEGER REFERENCES tracks ON DELETE SET NULL,
    track_object_id TEXT NOT NULL,
    fallback_title TEXT,
    fallback_artist TEXT,
    fallback_album TEXT,
    fallback_album_display_artist TEXT,
    fallback_album_object_id TEXT,
    fallback_primary_artist_object_id TEXT,
    fallback_media_uri TEXT,
    fallback_artwork_binding BLOB,
    fallback_duration_millis INTEGER,
    fallback_disc_number INTEGER,
    fallback_track_number INTEGER,
    fallback_year INTEGER,
    fallback_release_date TEXT,
    fallback_favorite INTEGER CHECK (
        fallback_favorite IS NULL OR fallback_favorite IN (0, 1)
    ),
    fallback_source_format TEXT,
    fallback_musicbrainz_recording_id TEXT,
    fallback_musicbrainz_release_track_id TEXT,
    fallback_musicbrainz_album_id TEXT,
    fallback_musicbrainz_release_group_id TEXT,
    fallback_primary_artist_musicbrainz_id TEXT,
    fallback_cue_path TEXT,
    fallback_cue_start_millis INTEGER,
    fallback_cue_end_millis INTEGER,
    CHECK (fallback_duration_millis IS NULL OR fallback_duration_millis >= 0),
    CHECK (fallback_disc_number IS NULL OR fallback_disc_number >= 0),
    CHECK (fallback_track_number IS NULL OR fallback_track_number >= 0),
    CHECK (
        fallback_musicbrainz_recording_id IS NULL
        OR fallback_musicbrainz_recording_id <> ''
    ),
    CHECK (
        (
            fallback_cue_path IS NULL
            AND fallback_cue_start_millis IS NULL
            AND fallback_cue_end_millis IS NULL
        )
        OR (
            fallback_cue_path IS NOT NULL AND fallback_cue_path <> ''
            AND fallback_cue_start_millis IS NOT NULL
            AND fallback_cue_start_millis >= 0
            AND fallback_cue_end_millis IS NOT NULL
            AND fallback_cue_end_millis > fallback_cue_start_millis
        )
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
    UNIQUE (source_key, object_id)
) STRICT;
CREATE UNIQUE INDEX queue_occurrences_page_idx
    ON queue_occurrences(source_key, position);
CREATE UNIQUE INDEX queue_occurrences_traversal_idx
    ON queue_occurrences(source_key, traversal_position);
CREATE INDEX queue_occurrences_track_idx
    ON queue_occurrences(source_key, track_key)
    WHERE track_key IS NOT NULL;
CREATE INDEX queue_occurrences_artwork_idx
    ON queue_occurrences(source_key, fallback_artwork_binding)
    WHERE fallback_artwork_binding IS NOT NULL;

CREATE TABLE loudness_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album')),
    entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    integrated_lufs REAL,
    true_peak REAL CHECK (true_peak IS NULL OR true_peak >= 0),
    origin TEXT NOT NULL DEFAULT 'source' CHECK (origin IN ('source', 'analysis')),
    PRIMARY KEY (source_key, entity_kind, entity_key)
) STRICT;

CREATE TABLE replay_gain_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album')),
    entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    gain_db REAL NOT NULL,
    peak REAL CHECK (peak IS NULL OR peak >= 0),
    PRIMARY KEY (source_key, entity_kind, entity_key)
) STRICT;

CREATE TABLE lyrics_cache (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    authority TEXT NOT NULL CHECK (authority <> ''),
    role TEXT NOT NULL CHECK (role <> ''),
    language TEXT NOT NULL DEFAULT '',
    script TEXT NOT NULL DEFAULT '',
    cache_input_digest BLOB NOT NULL CHECK (length(cache_input_digest) = 32),
    lyrics TEXT NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY (source_key, track_key, authority, role, language, script)
) STRICT;

CREATE TABLE local_files (
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
    parse_version INTEGER,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'rejected', 'unreadable', 'observed')),
    UNIQUE (source_key, path)
) STRICT;
CREATE INDEX local_files_identity_idx ON local_files(source_key, device_id, inode);
CREATE INDEX local_files_kind_path_idx ON local_files(source_key, kind, path);

CREATE TABLE local_file_dependencies (
    local_file_key INTEGER NOT NULL REFERENCES local_files ON DELETE CASCADE,
    dependency_path TEXT NOT NULL CHECK (dependency_path <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (local_file_key, position),
    UNIQUE (local_file_key, dependency_path)
) STRICT;

CREATE TABLE local_access_files (
    local_access_file_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    track_object_id TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('local', 'mapping', 'download')),
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    relative_path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    mtime_ns INTEGER NOT NULL,
    device_id INTEGER,
    inode INTEGER,
    parser_version INTEGER NOT NULL CHECK (parser_version >= 1),
    title TEXT NOT NULL,
    normalized_title TEXT NOT NULL,
    album TEXT NOT NULL,
    normalized_album TEXT NOT NULL,
    artist TEXT NOT NULL,
    normalized_artist TEXT NOT NULL,
    disc_number INTEGER NOT NULL CHECK (disc_number >= 0),
    track_number INTEGER NOT NULL CHECK (track_number >= 0),
    duration_millis INTEGER NOT NULL CHECK (duration_millis >= 0),
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    loudness_analysis_key BLOB CHECK (
        loudness_analysis_key IS NULL OR length(loudness_analysis_key) = 32
    ),
    UNIQUE (source_key, path)
) STRICT;
CREATE UNIQUE INDEX local_access_remote_idx
    ON local_access_files(source_key, track_object_id, origin)
    WHERE track_object_id IS NOT NULL;
CREATE INDEX local_access_precedence_idx ON local_access_files(
    source_key, track_object_id,
    (CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END),
    local_access_file_key
) WHERE track_object_id IS NOT NULL;
CREATE INDEX local_access_match_idx ON local_access_files(
    source_key, normalized_title, normalized_album, normalized_artist,
    disc_number, track_number, duration_millis
);

COMMIT;
"###;

pub(crate) async fn initialize(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let application_id = pragma(connection, "application_id").await?;
    let user_version = pragma(connection, "user_version").await?;
    let has_schema = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM sqlite_schema
         WHERE type='table' AND name NOT LIKE 'sqlite_%' LIMIT 1",
    )
    .fetch_optional(&mut *connection)
    .await?
    .is_some();

    if (application_id, user_version, has_schema) == (0, 0, false) {
        sqlx::raw_sql(FRESH_SCHEMA)
            .execute(&mut *connection)
            .await?;
    } else if application_id == APPLICATION_ID
        && (FIRST_CURRENT_FORMAT_SCHEMA_VERSION..SCHEMA_VERSION).contains(&user_version)
    {
        upgrade_current_format(connection, user_version).await?;
    } else if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
        return Err(LibraryError::UnsupportedStore {
            application_id,
            user_version,
        });
    }
    validate(connection).await
}

async fn upgrade_current_format(
    connection: &mut SqliteConnection,
    mut user_version: i64,
) -> LibraryResult<()> {
    while user_version < SCHEMA_VERSION {
        let migration = CURRENT_MIGRATIONS
            .iter()
            .find(|migration| migration.from_version == user_version)
            .ok_or_else(|| {
                LibraryError::InvalidStore(format!(
                    "schema migration chain is incomplete at version {user_version}"
                ))
            })?;
        match user_version {
            CATALOG_OWNERS_SCHEMA_VERSION => migrate_schema_41(connection).await?,
            _ => {
                return Err(LibraryError::InvalidStore(format!(
                    "schema migration is not implemented for version {user_version}"
                )));
            }
        }
        user_version = pragma(connection, "user_version").await?;
        if user_version != migration.to_version {
            return Err(LibraryError::InvalidStore(format!(
                "schema migration from {} produced version {user_version} instead of {}",
                migration.from_version, migration.to_version
            )));
        }
    }
    Ok(())
}

async fn migrate_schema_41(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        r###"BEGIN IMMEDIATE;

CREATE TABLE replay_gain_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album')),
    entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    gain_db REAL NOT NULL,
    peak REAL CHECK (peak IS NULL OR peak >= 0),
    PRIMARY KEY (source_key, entity_kind, entity_key)
) STRICT;

PRAGMA user_version = 42;

COMMIT;
"###,
    )
    .execute(connection)
    .await?;
    Ok(())
}

pub(crate) async fn validate(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let application_id = pragma(connection, "application_id").await?;
    let user_version = pragma(connection, "user_version").await?;
    if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
        return Err(LibraryError::UnsupportedStore {
            application_id,
            user_version,
        });
    }
    let foreign_keys = pragma(connection, "foreign_keys").await?;
    if foreign_keys != 1 {
        return Err(LibraryError::InvalidStore(
            "foreign keys are disabled".to_string(),
        ));
    }
    let actual = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema
         WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let expected = TABLES.iter().map(|name| (*name).to_string()).collect();
    if actual != expected {
        return Err(LibraryError::InvalidStore(format!(
            "table inventory differs: expected {expected:?}, found {actual:?}"
        )));
    }
    let foreign_key_failure = sqlx::query("PRAGMA foreign_key_check")
        .fetch_optional(&mut *connection)
        .await?
        .is_some();
    if foreign_key_failure {
        return Err(LibraryError::InvalidStore(
            "foreign key check failed".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn is_legacy_schema(application_id: i64, user_version: i64) -> bool {
    application_id == APPLICATION_ID
        && (FIRST_LEGACY_SCHEMA_VERSION..=LAST_LEGACY_SCHEMA_VERSION).contains(&user_version)
}

const MIGRATE_SCHEMA_32: &str = r###"
BEGIN IMMEDIATE;

ALTER TABLE music_folders RENAME TO schema_32_music_folders;

CREATE TABLE music_folders (
    library_id INTEGER NOT NULL
        REFERENCES source_libraries(library_id) ON DELETE NO ACTION,
    folder_id TEXT NOT NULL CHECK (folder_id <> ''),
    name TEXT NOT NULL CHECK (name <> ''),
    image_item_id TEXT CHECK (image_item_id IS NULL OR image_item_id <> ''),
    image_tag TEXT CHECK (image_tag IS NULL OR image_tag <> ''),
    PRIMARY KEY (library_id, folder_id),
    CHECK (image_tag IS NULL OR image_item_id IS NOT NULL)
) STRICT;

INSERT INTO music_folders(library_id, folder_id, name)
SELECT library_id, folder_id, name
FROM schema_32_music_folders;

DROP TABLE schema_32_music_folders;

PRAGMA user_version = 33;

COMMIT;
"###;

const MIGRATE_SCHEMA_33: &str = r###"
BEGIN IMMEDIATE;

ALTER TABLE local_files RENAME TO schema_33_local_files;

CREATE TABLE local_files (
    library_id INTEGER NOT NULL
        REFERENCES source_libraries(library_id) ON DELETE NO ACTION,
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    relative_path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN ('audio', 'cue', 'image', 'directory')
    ),
    size_bytes INTEGER CHECK (size_bytes IS NULL OR size_bytes >= 0),
    mtime_ns INTEGER NOT NULL,
    device_id INTEGER,
    inode INTEGER,
    -- The current audio/CUE parser writes version 1. Images and directories are
    -- observed without parsing and therefore keep this null.
    parse_version INTEGER CHECK (
        parse_version IS NULL OR parse_version >= 1
    ),
    read_state TEXT NOT NULL CHECK (
        read_state IN (
            'parsed',
            'metadata-fallback',
            'unreadable',
            'invalid',
            'observed'
        )
    ),
    dependencies_json TEXT NOT NULL CHECK (
        length(CAST(dependencies_json AS BLOB)) <= 2097152
        AND CASE
            WHEN json_valid(dependencies_json)
            THEN json_type(dependencies_json) = 'array'
            ELSE 0
        END
    ),
    PRIMARY KEY (library_id, path),
    CHECK (
        (kind = 'directory' AND size_bytes IS NULL)
        OR (kind <> 'directory' AND size_bytes IS NOT NULL)
    ),
    CHECK (
        (
            kind = 'audio'
            AND parse_version IS NOT NULL
            AND read_state IN ('parsed', 'metadata-fallback', 'unreadable')
        )
        OR (
            kind = 'cue'
            AND parse_version IS NOT NULL
            AND read_state IN ('parsed', 'invalid')
        )
        OR (
            kind IN ('image', 'directory')
            AND parse_version IS NULL
            AND read_state = 'observed'
        )
    ),
    CHECK (kind = 'cue' OR dependencies_json = '[]')
) STRICT;

INSERT INTO local_files(
    library_id, path, root, relative_path, kind, size_bytes, mtime_ns,
    device_id, inode, parse_version, read_state, dependencies_json
)
SELECT
    library_id, path, root, relative_path, kind, size_bytes, mtime_ns,
    device_id, inode, parse_version, read_state, dependencies_json
FROM schema_33_local_files;

DROP TABLE schema_33_local_files;

ALTER TABLE local_access_files RENAME TO schema_33_local_access_files;

CREATE TABLE local_access_files (
    source_id TEXT NOT NULL CHECK (source_id <> ''),
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    relative_path TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    mtime_ns INTEGER NOT NULL,
    device_id INTEGER,
    inode INTEGER,
    parser_version INTEGER NOT NULL CHECK (parser_version >= 1),
    title TEXT NOT NULL,
    album TEXT NOT NULL,
    artist TEXT NOT NULL,
    disc_number INTEGER NOT NULL CHECK (disc_number BETWEEN 0 AND 65535),
    track_number INTEGER NOT NULL CHECK (track_number BETWEEN 0 AND 65535),
    duration_seconds INTEGER NOT NULL CHECK (
        duration_seconds BETWEEN 0 AND 4294967295
    ),
    PRIMARY KEY (source_id, path)
) STRICT;

INSERT INTO local_access_files(
    source_id, path, root, relative_path, size_bytes, mtime_ns,
    device_id, inode, parser_version, title, album, artist,
    disc_number, track_number, duration_seconds
)
SELECT
    source_id, path, root, relative_path, size_bytes, mtime_ns,
    device_id, inode, parser_version, title, album, artist,
    disc_number, track_number, duration_seconds
FROM schema_33_local_access_files;

DROP TABLE schema_33_local_access_files;

PRAGMA user_version = 34;

COMMIT;
"###;

async fn migrate_schema_32(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(MIGRATE_SCHEMA_32).execute(connection).await?;
    Ok(())
}

async fn migrate_schema_33(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(MIGRATE_SCHEMA_33).execute(connection).await?;
    Ok(())
}

async fn migrate_schema_34(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut transaction = connection.begin().await?;
    backfill_recent_plays(&mut transaction).await?;
    sqlx::raw_sql("PRAGMA user_version=35")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn migrate_schema_35(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        r###"BEGIN IMMEDIATE;

CREATE TABLE pending_favorites (
    source_id TEXT NOT NULL CHECK (source_id <> ''),
    item_kind TEXT NOT NULL CHECK (
        item_kind IN ('album', 'track', 'artist')
    ),
    item_id TEXT NOT NULL CHECK (item_id <> ''),
    favorite INTEGER NOT NULL CHECK (favorite IN (0, 1)),
    previous_favorite INTEGER NOT NULL CHECK (previous_favorite IN (0, 1)),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0),
    PRIMARY KEY (source_id, item_kind, item_id)
) STRICT;

CREATE INDEX pending_favorites_due_idx
    ON pending_favorites(source_id, next_attempt_at, item_kind, item_id);

PRAGMA user_version = 36;

COMMIT;
"###,
    )
    .execute(connection)
    .await?;
    Ok(())
}

async fn migrate_schema_36(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        r###"BEGIN IMMEDIATE;

CREATE TABLE loudness_measurements (
    source_id TEXT NOT NULL CHECK (source_id <> ''),
    scope TEXT NOT NULL CHECK (scope IN ('track', 'album')),
    item_id TEXT NOT NULL CHECK (item_id <> ''),
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    integrated_lufs REAL CHECK (
        integrated_lufs IS NULL
        OR integrated_lufs BETWEEN -200.0 AND 100.0
    ),
    true_peak REAL NOT NULL CHECK (
        true_peak >= 0.0 AND true_peak <= 1000.0
    ),
    PRIMARY KEY (source_id, scope, item_id)
) STRICT;

PRAGMA user_version = 37;

COMMIT;
"###,
    )
    .execute(connection)
    .await?;
    Ok(())
}

async fn migrate_schema_37(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut transaction = connection.begin().await?;
    sqlx::raw_sql(
        r###"ALTER TABLE playback_state RENAME TO schema_37_playback_state;
ALTER TABLE playback_queues RENAME TO schema_37_playback_queues;

CREATE TABLE playback_queues (
    source_id TEXT PRIMARY KEY CHECK (source_id <> ''),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    rows_json TEXT NOT NULL CHECK (
        length(CAST(rows_json AS BLOB)) <= 268435456
        AND CASE
            WHEN json_valid(rows_json)
            THEN json_type(rows_json) = 'object'
            ELSE 0
        END
    ),
    traversal_json TEXT NOT NULL CHECK (
        length(CAST(traversal_json AS BLOB)) <= 268435456
        AND CASE
            WHEN json_valid(traversal_json)
            THEN 1
            ELSE 0
        END
    ),
    -- SQLite requires this exact unique parent key for playback_state's
    -- deferred two-column foreign key, even though source_id is already unique.
    UNIQUE (source_id, revision)
) STRICT;

CREATE TABLE playback_state (
    source_id TEXT PRIMARY KEY CHECK (source_id <> ''),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    selected_occurrence_id TEXT CHECK (
        selected_occurrence_id IS NULL OR selected_occurrence_id <> ''
    ),
    progress_millis INTEGER NOT NULL CHECK (progress_millis >= 0),
    FOREIGN KEY (source_id, revision)
        REFERENCES playback_queues(source_id, revision)
        ON DELETE NO ACTION
        DEFERRABLE INITIALLY DEFERRED
) STRICT;
"###,
    )
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO playback_queues(
             source_id, revision, rows_json, traversal_json
         )
         SELECT source_id, revision,
                json_object(
                    'occurrences',
                    json(COALESCE(json_extract(payload_json, '$.occurrences'), 'null')),
                    'fallback_tracks',
                    json(COALESCE(json_extract(payload_json, '$.fallback_tracks'), 'null'))
                ),
                json(COALESCE(json_extract(payload_json, '$.traversal'), 'null'))
         FROM schema_37_playback_queues",
    )
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO playback_state(
            source_id, revision, selected_occurrence_id, progress_millis
         )
         SELECT state.source_id, state.revision,
                state.selected_occurrence_id, state.progress_millis
         FROM schema_37_playback_state AS state
         JOIN schema_37_playback_queues AS queue
           ON queue.source_id = state.source_id
          AND queue.revision = state.revision",
    )
    .execute(&mut *transaction)
    .await?;
    sqlx::raw_sql(
        "DROP TABLE schema_37_playback_state;
         DROP TABLE schema_37_playback_queues;
         PRAGMA user_version = 38;",
    )
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

async fn migrate_schema_38(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        r###"ALTER TABLE local_files RENAME TO schema_38_local_files;

CREATE TABLE local_files (
    library_id INTEGER NOT NULL
        REFERENCES source_libraries(library_id) ON DELETE NO ACTION,
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    relative_path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (
        kind IN ('media', 'cue', 'image', 'directory')
    ),
    size_bytes INTEGER CHECK (size_bytes IS NULL OR size_bytes >= 0),
    mtime_ns INTEGER NOT NULL,
    device_id INTEGER,
    inode INTEGER,
    -- The current media/CUE parser writes a version. Images and directories are
    -- observed without parsing and therefore keep this null.
    parse_version INTEGER CHECK (
        parse_version IS NULL OR parse_version >= 1
    ),
    state TEXT NOT NULL CHECK (
        state IN (
            'accepted',
            'rejected',
            'unreadable',
            'observed'
        )
    ),
    dependencies_json TEXT NOT NULL CHECK (
        length(CAST(dependencies_json AS BLOB)) <= 2097152
        AND CASE
            WHEN json_valid(dependencies_json)
            THEN json_type(dependencies_json) = 'array'
            ELSE 0
        END
    ),
    PRIMARY KEY (library_id, path),
    CHECK (
        (kind = 'directory' AND size_bytes IS NULL)
        OR (kind <> 'directory' AND size_bytes IS NOT NULL)
    ),
    CHECK (
        (
            kind = 'media'
            AND parse_version IS NOT NULL
            AND state IN ('accepted', 'rejected', 'unreadable')
        )
        OR (
            kind = 'cue'
            AND parse_version IS NOT NULL
            AND state IN ('accepted', 'rejected')
        )
        OR (
            kind IN ('image', 'directory')
            AND parse_version IS NULL
            AND state = 'observed'
        )
    ),
    CHECK (kind = 'cue' OR dependencies_json = '[]')
) STRICT;

INSERT INTO local_files(
    library_id, path, root, relative_path, kind, size_bytes, mtime_ns,
    device_id, inode, parse_version, state, dependencies_json
)
SELECT
    library_id, path, root, relative_path,
    CASE kind WHEN 'audio' THEN 'media' ELSE kind END,
    size_bytes, mtime_ns, device_id, inode, parse_version,
    CASE read_state
        WHEN 'parsed' THEN 'accepted'
        WHEN 'metadata-fallback' THEN 'accepted'
        WHEN 'invalid' THEN 'rejected'
        ELSE read_state
    END,
    dependencies_json
FROM schema_38_local_files;

DROP TABLE schema_38_local_files;
PRAGMA user_version = 39;
"###,
    )
    .execute(connection)
    .await?;
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LegacySmartPlaylistDefinition {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    match_all: Vec<LegacySmartPlaylistRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    match_any: Vec<LegacySmartPlaylistRule>,
    sort_field: LegacySmartPlaylistSortField,
    descending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LegacySmartPlaylistRule {
    field: LegacySmartPlaylistRuleField,
    operator: LegacySmartPlaylistRuleOperator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<LegacySmartPlaylistRuleValue>,
}

#[derive(serde::Deserialize, serde::Serialize, PartialEq)]
enum LegacySmartPlaylistRuleField {
    Title,
    Artist,
    Album,
    Comment,
    Genre,
    Mood,
    Bpm,
    Rating,
    Year,
    Favorite,
    Played,
    PlayCount,
    SkipCount,
    LastPlayed,
    DateAdded,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum LegacySmartPlaylistRuleOperator {
    Contains,
    NotContains,
    Equals,
    NotEquals,
    Above,
    Below,
    Between,
    Is,
    IsNot,
    Before,
    After,
    IsEmpty,
    IsNotEmpty,
}

#[derive(serde::Deserialize, serde::Serialize)]
enum LegacySmartPlaylistRuleValue {
    Text(String),
    Number(i64),
    NumberRange { min: i64, max: i64 },
    Bool(bool),
    Date(String),
    DateRange { start: String, end: String },
}

#[derive(serde::Deserialize, serde::Serialize)]
enum LegacySmartPlaylistSortField {
    Title,
    Artist,
    Album,
    Year,
    DateAdded,
    LastPlayed,
    PlayCount,
    SkipCount,
    Bpm,
    Rating,
    Duration,
}

async fn migrate_schema_39(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let mut transaction = connection.begin().await?;
    sqlx::raw_sql(
        "CREATE TABLE user_ratings (
    source_id TEXT NOT NULL CHECK (source_id <> ''),
    item_kind TEXT NOT NULL CHECK (item_kind IN ('album', 'track', 'artist')),
    item_id TEXT NOT NULL CHECK (item_id <> ''),
    rating INTEGER NOT NULL CHECK (rating BETWEEN 0 AND 10),
    PRIMARY KEY (source_id, item_kind, item_id)
) STRICT;",
    )
    .execute(&mut *transaction)
    .await?;
    let mut cursor = (String::new(), String::new());
    loop {
        let definition = sqlx::query_as::<_, (String, String, String)>(
            "SELECT source_id, smart_playlist_id, definition_json
                 FROM smart_playlists
                 WHERE (source_id, smart_playlist_id) > (?1, ?2)
                 ORDER BY source_id, smart_playlist_id LIMIT 1",
        )
        .bind(&cursor.0)
        .bind(&cursor.1)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some((source_id, playlist_id, json)) = definition else {
            break;
        };
        let mut definition: LegacySmartPlaylistDefinition = serde_json::from_str(&json)?;
        for rule in definition
            .match_all
            .iter_mut()
            .chain(definition.match_any.iter_mut())
            .filter(|rule| rule.field == LegacySmartPlaylistRuleField::Rating)
        {
            match rule.value.as_mut() {
                Some(LegacySmartPlaylistRuleValue::Number(value)) => *value *= 2,
                Some(LegacySmartPlaylistRuleValue::NumberRange { min, max }) => {
                    *min *= 2;
                    *max *= 2;
                }
                _ => {}
            }
        }
        sqlx::query(
            "UPDATE smart_playlists SET definition_json = ?3
             WHERE source_id = ?1 AND smart_playlist_id = ?2",
        )
        .bind(&source_id)
        .bind(&playlist_id)
        .bind(serde_json::to_string(&definition)?)
        .execute(&mut *transaction)
        .await?;
        cursor = (source_id, playlist_id);
    }
    sqlx::raw_sql("PRAGMA user_version=40")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn backfill_recent_plays(connection: &mut Transaction<'_, Sqlite>) -> LibraryResult<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO recent_plays(
             play_id, source_id, track_id, track_title, artist_name,
             album_title, played_at
         )
         SELECT printf(
                    'activity-last-play:%d:%s:%d:%s:%d',
                    length(source_id), source_id,
                    length(item_id), item_id,
                    last_played_at
                ),
                source_id, item_id, display_name,
                COALESCE(display_context, ''), NULL, last_played_at
         FROM (
             SELECT source_id, item_id, display_name, display_context,
                    last_played_at,
                    row_number() OVER (
                        PARTITION BY source_id
                        ORDER BY last_played_at DESC, item_id DESC
                    ) AS position
             FROM listening_aggregates
             WHERE period = 'lifetime'
               AND item_kind = 'track'
               AND last_played_at IS NOT NULL
         ) AS activity
         WHERE position <= 100
           AND NOT EXISTS (
               SELECT 1 FROM recent_plays
               WHERE recent_plays.source_id = activity.source_id
                 AND recent_plays.track_id = activity.item_id
                 AND recent_plays.played_at = activity.last_played_at
           )",
    )
    .execute(&mut **connection)
    .await?;
    sqlx::query(
        "DELETE FROM recent_plays
         WHERE play_id IN (
             SELECT play_id
             FROM (
                 SELECT play_id,
                        row_number() OVER (
                            PARTITION BY source_id
                            ORDER BY played_at DESC, play_id DESC
                        ) AS position
                 FROM recent_plays
             )
             WHERE position > 100
        )",
    )
    .execute(&mut **connection)
    .await?;
    Ok(())
}

pub(crate) async fn upgrade_legacy_schema(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let application_id = pragma(connection, "application_id").await?;
    let mut user_version = pragma(connection, "user_version").await?;
    if application_id != APPLICATION_ID
        || !(FIRST_LEGACY_SCHEMA_VERSION..=LAST_LEGACY_SCHEMA_VERSION).contains(&user_version)
    {
        return Err(LibraryError::UnsupportedStore {
            application_id,
            user_version,
        });
    }
    while user_version < LAST_LEGACY_SCHEMA_VERSION {
        let migration = LEGACY_MIGRATIONS
            .iter()
            .find(|migration| migration.from_version == user_version)
            .ok_or_else(|| {
                LibraryError::InvalidStore(format!(
                    "legacy migration chain is incomplete at schema {user_version}"
                ))
            })?;
        match user_version {
            32 => migrate_schema_32(connection).await?,
            33 => migrate_schema_33(connection).await?,
            34 => migrate_schema_34(connection).await?,
            35 => migrate_schema_35(connection).await?,
            36 => migrate_schema_36(connection).await?,
            37 => migrate_schema_37(connection).await?,
            38 => migrate_schema_38(connection).await?,
            39 => migrate_schema_39(connection).await?,
            _ => {
                return Err(LibraryError::InvalidStore(format!(
                    "legacy migration chain is incomplete at schema {user_version}"
                )));
            }
        }
        user_version = pragma(connection, "user_version").await?;
        if user_version != migration.to_version {
            return Err(LibraryError::InvalidStore(format!(
                "legacy schema {} migration produced schema {user_version} instead of {}",
                migration.from_version, migration.to_version
            )));
        }
    }
    Ok(())
}

pub(crate) async fn pragma(
    connection: &mut SqliteConnection,
    name: &str,
) -> Result<i64, sqlx::Error> {
    let sql = match name {
        "application_id" => "PRAGMA application_id",
        "user_version" => "PRAGMA user_version",
        "foreign_keys" => "PRAGMA foreign_keys",
        _ => {
            return Err(sqlx::Error::InvalidArgument(format!(
                "unknown PRAGMA {name}"
            )));
        }
    };
    sqlx::query_scalar(sql).fetch_one(connection).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_migration_registry_is_contiguous() {
        let mut version = FIRST_LEGACY_SCHEMA_VERSION;
        for migration in LEGACY_MIGRATIONS {
            assert_eq!(migration.from_version, version);
            assert_eq!(migration.to_version, version + 1);
            version = migration.to_version;
        }
        assert_eq!(version, LAST_LEGACY_SCHEMA_VERSION);
    }

    #[test]
    fn current_migration_registry_reaches_the_fresh_schema() {
        let mut version = FIRST_CURRENT_FORMAT_SCHEMA_VERSION;
        for migration in CURRENT_MIGRATIONS {
            assert_eq!(migration.from_version, version);
            assert_eq!(migration.to_version, version + 1);
            version = migration.to_version;
        }
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn legacy_schema_39_rating_meaning_reaches_schema_40() {
        let mut connection = SqliteConnection::connect("sqlite::memory:")
            .await
            .expect("open legacy fixture");
        sqlx::raw_sql(
                "PRAGMA application_id=1381320270;
                 PRAGMA user_version=39;
                 CREATE TABLE smart_playlists(
                     source_id TEXT NOT NULL,
                     smart_playlist_id TEXT NOT NULL,
                     definition_json TEXT NOT NULL,
                     PRIMARY KEY(source_id, smart_playlist_id)
                 ) STRICT;
                 INSERT INTO smart_playlists VALUES(
                     'source', 'rated',
                     '{\"match_all\":[{\"field\":\"Rating\",\"operator\":\"Above\",\"value\":{\"Number\":4}}],\"match_any\":[],\"sort_field\":\"Title\",\"descending\":false}'
                 );",
            )
            .execute(&mut connection)
            .await
            .expect("create schema 39 fixture");
        upgrade_legacy_schema(&mut connection)
            .await
            .expect("run legacy migration");
        assert_eq!(
            pragma(&mut connection, "user_version")
                .await
                .expect("read migrated version"),
            LAST_LEGACY_SCHEMA_VERSION
        );
        let definition: serde_json::Value = serde_json::from_str(
            &sqlx::query_scalar::<_, String>("SELECT definition_json FROM smart_playlists")
                .fetch_one(&mut connection)
                .await
                .expect("read migrated definition"),
        )
        .expect("parse migrated definition");
        assert_eq!(definition["match_all"][0]["value"]["Number"], 8);
    }

    #[tokio::test]
    async fn legacy_schema_37_splits_queue_without_a_rust_queue_snapshot() {
        let mut connection = SqliteConnection::connect("sqlite::memory:")
            .await
            .expect("open legacy queue fixture");
        sqlx::raw_sql(
                "CREATE TABLE playback_queues(
                     source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
                     payload_json TEXT NOT NULL
                 ) STRICT;
                 CREATE TABLE playback_state(
                     source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
                     selected_occurrence_id TEXT, progress_millis INTEGER NOT NULL
                 ) STRICT;
                 INSERT INTO playback_queues VALUES(
                     'source', 3,
                     '{\"occurrences\":[{\"id\":\"one\"}],\"fallback_tracks\":[],\"traversal\":[\"one\"]}'
                 );
                 INSERT INTO playback_state VALUES('source', 3, 'one', 42);",
            )
            .execute(&mut connection)
            .await
            .expect("create schema 37 queue fixture");
        migrate_schema_37(&mut connection)
            .await
            .expect("migrate schema 37 queue");
        let (rows, traversal, selected) = sqlx::query_as::<_, (String, String, String)>(
            "SELECT queue.rows_json, queue.traversal_json, state.selected_occurrence_id
                 FROM playback_queues AS queue
                 JOIN playback_state AS state USING(source_id, revision)",
        )
        .fetch_one(&mut connection)
        .await
        .expect("read migrated queue");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rows).expect("parse queue rows"),
            serde_json::json!({
                "occurrences": [{"id": "one"}],
                "fallback_tracks": [],
            })
        );
        assert_eq!(traversal, "[\"one\"]");
        assert_eq!(selected, "one");
    }
}
