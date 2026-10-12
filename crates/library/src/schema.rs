//! The durable library schema owns source facts and application references.
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

PRAGMA user_version = 58;

CREATE TABLE IF NOT EXISTS queue_contexts (
    context_key INTEGER PRIMARY KEY,
    context TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE IF NOT EXISTS queue_entries (
    entry_id INTEGER PRIMARY KEY,
    track_key INTEGER NOT NULL REFERENCES tracks,
    provenance TEXT,
    context_key INTEGER REFERENCES queue_contexts,
    source_rank INTEGER,
    received_from_connect INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX IF NOT EXISTS queue_entries_track_idx ON queue_entries(track_key);
CREATE INDEX IF NOT EXISTS queue_entries_context_idx ON queue_entries(context_key) WHERE context_key IS NOT NULL;
CREATE TABLE IF NOT EXISTS queue_positions (
    order_kind INTEGER NOT NULL,
    position INTEGER NOT NULL,
    entry_id INTEGER NOT NULL,
    PRIMARY KEY(order_kind,position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS playlists (
    track_count INTEGER NOT NULL DEFAULT 0,
    duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0,
    playlist_key INTEGER PRIMARY KEY,
    owner TEXT NOT NULL DEFAULT 'rufin' CHECK(owner IN ('rufin','source')),
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
    source_key INTEGER REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT,
    normalized_name TEXT,
    sort_text TEXT,
    position INTEGER NOT NULL CHECK (position >= 0),
    artwork_bytes BLOB,
    artwork_mime TEXT,
    artwork_revision TEXT,
    artwork_binding BLOB,
    writable INTEGER NOT NULL DEFAULT 1,
    metadata_writable INTEGER NOT NULL DEFAULT 1,
    provider_revision TEXT,
    valid_until INTEGER,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS playlists_global_object_idx ON playlists(object_id)
    WHERE source_key IS NULL;

CREATE INDEX IF NOT EXISTS playlists_order_idx ON playlists(source_key, sort_text, playlist_key);

CREATE INDEX IF NOT EXISTS playlists_title_idx ON playlists(source_key, sort_text, playlist_key);

CREATE UNIQUE INDEX IF NOT EXISTS playlists_position_idx ON playlists(position);

CREATE TABLE IF NOT EXISTS playlist_file_links (
    playlist_key INTEGER PRIMARY KEY REFERENCES playlists ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    path TEXT NOT NULL,
    auto_refresh INTEGER NOT NULL,
    auto_save INTEGER,
    path_mode TEXT NOT NULL,
    revision TEXT,
    dirty INTEGER NOT NULL,
    error TEXT,
    UNIQUE(source_id, path)
) STRICT;

CREATE TABLE IF NOT EXISTS playlist_file_ignored (
    source_id TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY(source_id,path)
) STRICT;

CREATE TABLE IF NOT EXISTS playlist_file_settings (
    source_id TEXT PRIMARY KEY,
    auto_save INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS playlist_entries (
    playlist_entry_key INTEGER PRIMARY KEY,
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    track_key INTEGER NOT NULL REFERENCES tracks,
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (playlist_key, object_id),
    UNIQUE (playlist_key, position)
) STRICT;
CREATE INDEX IF NOT EXISTS playlist_entries_order_idx ON playlist_entries(playlist_key, position);
CREATE INDEX IF NOT EXISTS playlist_entries_shuffle_idx ON playlist_entries(playlist_key,((playlist_entry_key*1103515245)%2147483647),playlist_entry_key);
CREATE INDEX IF NOT EXISTS playlist_entries_track_idx ON playlist_entries(track_key,playlist_key);

CREATE TABLE IF NOT EXISTS smart_playlists (
    smart_playlist_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    definition_json TEXT NOT NULL CHECK (json_valid(definition_json)),
    artwork_bytes BLOB,
    artwork_revision TEXT,
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
    captured_media_uri TEXT,
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
CREATE INDEX IF NOT EXISTS listens_period_idx ON listens(local_period) WHERE skipped=0;

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
    current_index INTEGER,
    progress_millis INTEGER NOT NULL DEFAULT 0 CHECK (progress_millis >= 0),
    repeat_mode TEXT NOT NULL DEFAULT 'none' CHECK (repeat_mode IN ('none', 'one', 'all')),
    shuffled INTEGER NOT NULL DEFAULT 0 CHECK (shuffled IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS queue_transfer_pages (
    transfer TEXT NOT NULL,
    page_offset INTEGER NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (transfer, page_offset)
) STRICT;

CREATE TABLE IF NOT EXISTS connect_media_files (
    media_uri TEXT NOT NULL,
    encoding TEXT NOT NULL,
    revision TEXT NOT NULL,
    path TEXT NOT NULL,
    managed INTEGER NOT NULL,
    hash TEXT,
    backing_uri TEXT,
    PRIMARY KEY (media_uri, encoding)
) STRICT;

CREATE INDEX IF NOT EXISTS connect_media_files_path_idx ON connect_media_files(path,encoding,revision);
CREATE INDEX IF NOT EXISTS connect_media_files_hash_idx ON connect_media_files(hash) WHERE hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS connect_media_files_backing_idx ON connect_media_files(backing_uri,encoding,revision);
CREATE INDEX IF NOT EXISTS connect_media_files_missing_backing_idx ON connect_media_files(media_uri) WHERE backing_uri IS NULL;

CREATE TABLE IF NOT EXISTS local_locators (
    local_access_file_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES sources ON DELETE CASCADE,
    media_uri TEXT NOT NULL CHECK(media_uri<>''),
    origin TEXT NOT NULL CHECK(origin IN ('local','mapping','download','import')),
    path TEXT NOT NULL CHECK(path<>''), root TEXT NOT NULL, relative_path TEXT NOT NULL,
    access_uri TEXT NOT NULL CHECK(access_uri<>''),
    UNIQUE(media_uri,origin)
) STRICT;
CREATE INDEX IF NOT EXISTS local_locators_source_idx ON local_locators(source_key,origin);
CREATE INDEX IF NOT EXISTS local_locators_path_idx ON local_locators(source_key,path);
CREATE INDEX IF NOT EXISTS local_locators_access_idx ON local_locators(media_uri,origin,local_access_file_key);


CREATE TABLE IF NOT EXISTS legacy_activity (
    source_id TEXT NOT NULL, period TEXT NOT NULL, item_kind TEXT NOT NULL,
    track_object_id TEXT NOT NULL, play_count INTEGER NOT NULL, skip_count INTEGER NOT NULL,
    last_played_at INTEGER,
    PRIMARY KEY(source_id,period,item_kind,track_object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS local_locators_precedence_idx ON local_locators(media_uri,CASE origin WHEN 'download' THEN 0 WHEN 'mapping' THEN 1 ELSE 2 END,local_access_file_key);

CREATE TABLE IF NOT EXISTS sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    display_name TEXT NOT NULL DEFAULT '', normalized_name TEXT NOT NULL DEFAULT '', freshness BLOB,
    kind TEXT,
    artwork_digest BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000' CHECK (length(artwork_digest) = 32),
    distinct_track_covers INTEGER NOT NULL DEFAULT 0 CHECK (distinct_track_covers IN (0,1)),
    catalog_revision INTEGER NOT NULL DEFAULT 0 CHECK (catalog_revision >= 0),
    catalog_complete INTEGER NOT NULL DEFAULT 0 CHECK (catalog_complete IN (0,1)),
    track_count INTEGER NOT NULL DEFAULT 0 CHECK (track_count >= 0)
) STRICT;

CREATE TABLE IF NOT EXISTS albums (
    derived_source_play_count INTEGER NOT NULL DEFAULT 0,
    activity_play_count INTEGER NOT NULL DEFAULT 0,
    derived_source_last_played INTEGER,
    activity_last_played INTEGER,
    track_count INTEGER NOT NULL DEFAULT 0,
    duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0,
    album_key INTEGER PRIMARY KEY,
    source_play_count INTEGER,
    source_skip_count INTEGER,
    source_last_played INTEGER,
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    title TEXT NOT NULL,
    normalized_title TEXT NOT NULL,
    display_artist TEXT NOT NULL,
    album_artist_sort TEXT,
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

CREATE INDEX IF NOT EXISTS albums_artist_order_idx ON albums(source_key,(album_artist_sort IS NULL),album_artist_sort,(year IS NULL),year,sort_text,album_key);
CREATE INDEX IF NOT EXISTS albums_artist_desc_order_idx ON albums(source_key,(album_artist_sort IS NULL),album_artist_sort DESC,(year IS NULL),year,sort_text,album_key);
CREATE INDEX IF NOT EXISTS albums_year_order_idx ON albums(source_key,year,sort_text,album_key);
CREATE INDEX IF NOT EXISTS albums_release_order_idx ON albums(source_key,release_date DESC,year DESC,sort_text,album_key) WHERE indexed=1;
CREATE INDEX IF NOT EXISTS albums_order_idx ON albums(source_key, sort_text, album_key);
CREATE INDEX IF NOT EXISTS albums_source_favorites_idx ON albums(source_key, object_id) WHERE source_favorite=1;

CREATE INDEX IF NOT EXISTS albums_key_idx ON albums(source_key, album_key);
CREATE INDEX IF NOT EXISTS albums_shuffle_idx ON albums(source_key,((album_key*1103515245)%2147483647),album_key);

CREATE INDEX IF NOT EXISTS albums_artwork_idx ON albums(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS tracks (
    track_key INTEGER PRIMARY KEY,
    source_play_count INTEGER,
    source_skip_count INTEGER,
    source_last_played INTEGER,
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
    source_key INTEGER REFERENCES sources ON DELETE SET NULL,
    object_id TEXT CHECK (object_id <> ''),
    album_key INTEGER REFERENCES albums ON DELETE SET NULL,
    title TEXT,
    normalized_search TEXT,
    display_album TEXT,
    display_artist TEXT,
    album_display_artist TEXT,
    musicbrainz_album_id TEXT,
    musicbrainz_release_group_id TEXT,
    primary_artist_musicbrainz_id TEXT,
    sort_text TEXT,
    sort_album_artist TEXT,
    sort_album_year INTEGER,
    sort_album TEXT,
    duration_millis INTEGER CHECK (duration_millis >= 0),
    disc_number INTEGER CHECK (disc_number >= 0),
    track_number INTEGER CHECK (track_number >= 0),
    year INTEGER,
    release_date TEXT,
    date_added TEXT,
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    source_path TEXT,
    source_format TEXT,
    audio_properties TEXT NOT NULL DEFAULT '{}',
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
    local_play_count INTEGER NOT NULL DEFAULT 0 CHECK (local_play_count >= 0),
    source_loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(source_loudness_analysis_key) = 32),
    loudness_analysis_key BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(loudness_analysis_key) = 32),
    CHECK (bpm IS NULL OR bpm BETWEEN 0 AND 65535),
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
CREATE INDEX IF NOT EXISTS tracks_source_favorites_idx ON tracks(source_key, object_id) WHERE source_favorite=1;

CREATE INDEX IF NOT EXISTS tracks_key_idx ON tracks(source_key, track_key);
CREATE INDEX IF NOT EXISTS tracks_source_path_idx ON tracks(source_key, source_path, album_key);

CREATE INDEX IF NOT EXISTS tracks_source_cue_idx ON tracks(source_key, cue_path, object_id) WHERE cue_path IS NOT NULL;

CREATE INDEX IF NOT EXISTS tracks_artwork_idx ON tracks(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE INDEX IF NOT EXISTS tracks_source_album_order_idx ON tracks(source_key,album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_order_idx ON tracks(album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_artist_sort_asc_idx ON tracks(source_key,display_artist,(sort_album_year IS NULL),sort_album_year,sort_album,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_artist_sort_desc_idx ON tracks(source_key,display_artist DESC,(sort_album_year IS NULL),sort_album_year,sort_album,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_artist_sort_asc_idx ON tracks(source_key,(sort_album_artist IS NULL),sort_album_artist,(sort_album_year IS NULL),sort_album_year,sort_album,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_artist_sort_desc_idx ON tracks(source_key,(sort_album_artist IS NULL),sort_album_artist DESC,(sort_album_year IS NULL),sort_album_year,sort_album,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_sort_asc_idx ON tracks(source_key,sort_album,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_album_sort_desc_idx ON tracks(source_key,sort_album DESC,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);

CREATE TABLE IF NOT EXISTS artists (
    derived_source_play_count INTEGER NOT NULL DEFAULT 0,
    activity_play_count INTEGER NOT NULL DEFAULT 0,
    derived_source_last_played INTEGER,
    activity_last_played INTEGER,
    album_artist_derived_source_play_count INTEGER NOT NULL DEFAULT 0,
    album_artist_activity_play_count INTEGER NOT NULL DEFAULT 0,
    album_artist_derived_source_last_played INTEGER,
    album_artist_activity_last_played INTEGER,
    album_count INTEGER NOT NULL DEFAULT 0,
    track_count INTEGER NOT NULL DEFAULT 0,
    duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0,
    album_artist_album_count INTEGER NOT NULL DEFAULT 0,
    album_artist_track_count INTEGER NOT NULL DEFAULT 0,
    album_artist_duration_millis INTEGER NOT NULL DEFAULT 0,
    album_artist_downloaded_count INTEGER NOT NULL DEFAULT 0,
    artist_key INTEGER PRIMARY KEY,
    source_play_count INTEGER,
    source_skip_count INTEGER,
    source_last_played INTEGER,
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
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
CREATE INDEX IF NOT EXISTS artists_source_favorites_idx ON artists(source_key, object_id) WHERE source_favorite=1;

CREATE INDEX IF NOT EXISTS artists_artwork_idx ON artists(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS genres (
    album_count INTEGER NOT NULL DEFAULT 0,
    track_count INTEGER NOT NULL DEFAULT 0,
    duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0,
    genre_key INTEGER PRIMARY KEY,
    source_play_count INTEGER,
    source_skip_count INTEGER,
    source_last_played INTEGER,
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    sort_text TEXT NOT NULL,
    artwork_binding BLOB,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS genres_order_idx ON genres(source_key, sort_text, genre_key);
CREATE INDEX IF NOT EXISTS genres_track_count_order_idx ON genres(source_key,track_count DESC,sort_text,genre_key) WHERE indexed=1;

CREATE INDEX IF NOT EXISTS genres_artwork_idx ON genres(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE TABLE IF NOT EXISTS moods (
    track_count INTEGER NOT NULL DEFAULT 0,
    duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0,
    mood_key INTEGER PRIMARY KEY,
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
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
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
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

CREATE TABLE IF NOT EXISTS home_entries (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    section_id TEXT NOT NULL CHECK (section_id <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album', 'artist', 'playlist')),
    entity_key INTEGER NOT NULL,
    title TEXT NOT NULL,
    subtitle TEXT NOT NULL,
    section_title TEXT,
    PRIMARY KEY (source_key, section_id, position)
) STRICT;

CREATE TABLE IF NOT EXISTS loudness_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album')),
    entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL CHECK (length(analysis_key) = 32),
    integrated_lufs REAL,
    true_peak REAL CHECK (true_peak IS NULL OR true_peak >= 0),
    origin TEXT NOT NULL DEFAULT 'source' CHECK (origin IN ('source', 'analysis')),
    PRIMARY KEY (source_key, entity_kind, entity_key, origin)
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
    artist_pictures TEXT,
    revision TEXT,
    parse_version INTEGER,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'rejected', 'unreadable', 'observed')),
    UNIQUE (source_key, path)
) STRICT;

CREATE INDEX IF NOT EXISTS local_files_identity_idx ON local_files(source_key, device_id, inode);

CREATE INDEX IF NOT EXISTS local_files_native_id_idx ON local_files(source_key, native_id) WHERE native_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS local_files_kind_path_idx ON local_files(source_key, kind, path);

CREATE INDEX IF NOT EXISTS local_files_playlist_idx ON local_files(source_key, local_file_key)
    WHERE lower(path) LIKE '%.m3u' OR lower(path) LIKE '%.m3u8' OR lower(path) LIKE '%.pls' OR lower(path) LIKE '%.xspf';

CREATE TABLE IF NOT EXISTS local_file_dependencies (
    local_file_key INTEGER NOT NULL REFERENCES local_files ON DELETE CASCADE,
    dependency_path TEXT NOT NULL CHECK (dependency_path <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (local_file_key, position),
    UNIQUE (local_file_key, dependency_path)
) STRICT;

CREATE INDEX IF NOT EXISTS local_file_dependencies_path_idx ON local_file_dependencies(dependency_path, local_file_key);

CREATE TABLE IF NOT EXISTS local_access_metadata (
    access_uri TEXT PRIMARY KEY,
    size_bytes INTEGER NOT NULL, mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
    parser_version INTEGER NOT NULL, title TEXT NOT NULL, normalized_title TEXT NOT NULL,
    album TEXT NOT NULL, normalized_album TEXT NOT NULL, artist TEXT NOT NULL,
    normalized_artist TEXT NOT NULL, disc_number INTEGER NOT NULL, track_number INTEGER NOT NULL,
    duration_millis INTEGER NOT NULL, loudness_analysis_key BLOB
) STRICT;

CREATE INDEX IF NOT EXISTS local_access_match_uri_idx ON local_access_metadata(normalized_title,normalized_album,normalized_artist,disc_number,track_number,duration_millis,access_uri);

CREATE TRIGGER IF NOT EXISTS album_artist_insert AFTER INSERT ON album_artists BEGIN
    UPDATE albums SET album_artist_sort=credit.sort_text
    FROM (SELECT (SELECT artist.sort_text FROM album_artists relation JOIN artists artist USING(artist_key)
        WHERE relation.album_key=NEW.album_key ORDER BY relation.position LIMIT 1) sort_text) credit
    WHERE album_key=NEW.album_key AND album_artist_sort IS NOT credit.sort_text;
END;
CREATE TRIGGER IF NOT EXISTS album_artist_delete AFTER DELETE ON album_artists BEGIN
    UPDATE albums SET album_artist_sort=credit.sort_text
    FROM (SELECT (SELECT artist.sort_text FROM album_artists relation JOIN artists artist USING(artist_key)
        WHERE relation.album_key=OLD.album_key ORDER BY relation.position LIMIT 1) sort_text) credit
    WHERE album_key=OLD.album_key AND album_artist_sort IS NOT credit.sort_text;
END;
CREATE TRIGGER IF NOT EXISTS album_artist_update AFTER UPDATE OF artist_key,album_key,position ON album_artists BEGIN
    UPDATE albums SET album_artist_sort=(SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key)
        WHERE credit.album_key=albums.album_key ORDER BY credit.position LIMIT 1)
    WHERE album_key IN (OLD.album_key,NEW.album_key) AND album_artist_sort IS NOT (
        SELECT artist.sort_text FROM album_artists credit JOIN artists artist USING(artist_key)
        WHERE credit.album_key=albums.album_key ORDER BY credit.position LIMIT 1);
END;
CREATE TRIGGER IF NOT EXISTS album_artist_name AFTER UPDATE OF sort_text ON artists WHEN OLD.sort_text IS NOT NEW.sort_text BEGIN
    UPDATE albums SET album_artist_sort=NEW.sort_text
    WHERE album_key IN (SELECT album_key FROM album_artists WHERE artist_key=NEW.artist_key)
      AND NEW.artist_key=(SELECT artist_key FROM album_artists WHERE album_key=albums.album_key ORDER BY position LIMIT 1);
END;

CREATE TRIGGER IF NOT EXISTS track_sort_insert AFTER INSERT ON tracks BEGIN
    UPDATE tracks SET sort_album_artist=album.album_artist_sort,sort_album_year=album.year,
        sort_album=COALESCE(album.sort_text,NEW.display_album)
    FROM (SELECT 1) LEFT JOIN albums album ON album.album_key=NEW.album_key
    WHERE track_key=NEW.track_key AND (tracks.sort_album_artist,tracks.sort_album_year,tracks.sort_album)
        IS NOT (album.album_artist_sort,album.year,COALESCE(album.sort_text,NEW.display_album));
END;
CREATE TRIGGER IF NOT EXISTS track_sort_update AFTER UPDATE OF album_key,display_album ON tracks
WHEN (OLD.album_key,OLD.display_album) IS NOT (NEW.album_key,NEW.display_album) BEGIN
    UPDATE tracks SET sort_album_artist=album.album_artist_sort,sort_album_year=album.year,
        sort_album=COALESCE(album.sort_text,NEW.display_album)
    FROM (SELECT 1) LEFT JOIN albums album ON album.album_key=NEW.album_key
    WHERE track_key=NEW.track_key AND (tracks.sort_album_artist,tracks.sort_album_year,tracks.sort_album)
        IS NOT (album.album_artist_sort,album.year,COALESCE(album.sort_text,NEW.display_album));
END;
CREATE TRIGGER IF NOT EXISTS album_track_sort_update AFTER UPDATE OF album_artist_sort,year,sort_text ON albums
WHEN (OLD.album_artist_sort,OLD.year,OLD.sort_text) IS NOT (NEW.album_artist_sort,NEW.year,NEW.sort_text) BEGIN
    UPDATE tracks SET sort_album_artist=NEW.album_artist_sort,sort_album_year=NEW.year,sort_album=NEW.sort_text
    WHERE album_key=NEW.album_key AND (sort_album_artist,sort_album_year,sort_album)
        IS NOT (NEW.album_artist_sort,NEW.year,NEW.sort_text);
END;
CREATE TRIGGER IF NOT EXISTS source_track_insert AFTER INSERT ON tracks WHEN NEW.indexed=1 BEGIN
    UPDATE sources SET track_count=track_count+1 WHERE source_key=NEW.source_key;
END;
CREATE TRIGGER IF NOT EXISTS source_track_delete AFTER DELETE ON tracks WHEN OLD.indexed=1 BEGIN
    UPDATE sources SET track_count=track_count-1 WHERE source_key=OLD.source_key;
END;
CREATE TRIGGER IF NOT EXISTS source_track_update AFTER UPDATE OF source_key,indexed ON tracks
WHEN (OLD.source_key,OLD.indexed) IS NOT (NEW.source_key,NEW.indexed) BEGIN
    UPDATE sources SET track_count=track_count-OLD.indexed WHERE source_key=OLD.source_key;
    UPDATE sources SET track_count=track_count+NEW.indexed WHERE source_key=NEW.source_key;
END;

CREATE INDEX IF NOT EXISTS tracks_source_play_count_idx ON tracks(source_key,source_play_count DESC,sort_text,track_key) WHERE indexed=1 AND source_play_count>0;
CREATE INDEX IF NOT EXISTS tracks_source_last_played_idx ON tracks(source_key,source_last_played DESC,track_key) WHERE indexed=1 AND source_last_played IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_local_play_order_idx ON tracks(source_key,indexed,local_play_count DESC,sort_text,media_uri);
CREATE INDEX IF NOT EXISTS tracks_global_play_order_idx ON tracks(indexed,local_play_count DESC,sort_text,media_uri);
DROP INDEX IF EXISTS tracks_local_play_count_idx;
DROP INDEX IF EXISTS tracks_global_local_play_count_idx;
CREATE INDEX IF NOT EXISTS tracks_local_shuffle_idx ON tracks(source_key,local_play_count,((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_global_local_shuffle_idx ON tracks(local_play_count,((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_shuffle_idx ON tracks(source_key,((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_global_shuffle_idx ON tracks(((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_indexed_idx ON tracks(indexed,source_key,track_key);

CREATE TRIGGER IF NOT EXISTS local_play_count_listen_insert AFTER INSERT ON listens BEGIN
             UPDATE tracks SET local_play_count=local_play_count+1 WHERE media_uri=NEW.media_uri;
         END;
         CREATE TRIGGER IF NOT EXISTS local_play_count_listen_delete AFTER DELETE ON listens BEGIN
             UPDATE tracks SET local_play_count=local_play_count-1 WHERE media_uri=OLD.media_uri;
         END;
         CREATE TRIGGER IF NOT EXISTS local_play_count_track_insert AFTER INSERT ON tracks
         WHEN NEW.local_play_count<>0 OR EXISTS(SELECT 1 FROM listens WHERE media_uri=NEW.media_uri) BEGIN
             UPDATE tracks SET local_play_count=(SELECT count(*) FROM listens WHERE media_uri=NEW.media_uri)
             WHERE track_key=NEW.track_key;
         END;
         CREATE TRIGGER IF NOT EXISTS local_play_count_track_uri AFTER UPDATE OF media_uri ON tracks
         WHEN OLD.media_uri IS NOT NEW.media_uri BEGIN
             UPDATE tracks SET local_play_count=(SELECT count(*) FROM listens WHERE media_uri=NEW.media_uri)
             WHERE track_key=NEW.track_key;
         END;
CREATE VIEW IF NOT EXISTS local_access_files AS
          SELECT locator.local_access_file_key,locator.source_key,locator.media_uri,
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
          LEFT JOIN local_access_metadata metadata USING(access_uri);

"#;

pub(crate) async fn initialize_durable(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let version = pragma(connection, "user_version").await?;
    if !matches!(version, 0 | 56 | 57 | 58) {
        return Err(crate::LibraryError::Migration(format!(
            "unsupported library schema {version}"
        )));
    }
    let mut transaction = connection.begin().await?;
    if matches!(version, 56 | 57) {
        sqlx::query("ALTER TABLE loudness_measurements RENAME TO previous_loudness_measurements")
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::raw_sql(STORE_SCHEMA)
        .execute(&mut *transaction)
        .await?;
    if matches!(version, 56 | 57) {
        sqlx::raw_sql(
            "INSERT INTO loudness_measurements SELECT * FROM previous_loudness_measurements;
            DROP TABLE previous_loudness_measurements;",
        )
        .execute(&mut *transaction)
        .await?;
    }
    if version == 56 {
        // Connect accepted listens without admitting their songs in version 56.
        let mut after = String::new();
        loop {
            let missing = sqlx::query_as::<_, crate::ListenWrite>(
                "SELECT listen.external_id,listen.media_uri,listen.track_title title,listen.artist_name artist,listen.album_title album,
                    listen.duration_millis,listen.disc_number,listen.track_number,listen.year,listen.release_date,listen.source_format,
                    listen.musicbrainz_recording_id,listen.musicbrainz_release_track_id,listen.started_at,listen.local_period,
                    listen.listened_millis,listen.skipped
                 FROM listens listen WHERE listen.media_uri>?1
                    AND NOT EXISTS(SELECT 1 FROM tracks WHERE tracks.media_uri=listen.media_uri)
                    AND listen.listen_key=(SELECT latest.listen_key FROM listens latest WHERE latest.media_uri=listen.media_uri
                        ORDER BY latest.started_at DESC,latest.listen_key DESC LIMIT 1)
                 ORDER BY listen.media_uri LIMIT 256",
            ).bind(&after).fetch_all(&mut *transaction).await?;
            if missing.is_empty() {
                break;
            }
            for listen in missing {
                crate::tracks::remember_song(
                    &mut transaction,
                    &listen.media_uri,
                    crate::tracks::SongMetadata {
                        title: Some(&listen.title),
                        artist: Some(&listen.artist),
                        album: Some(&listen.album),
                        duration_millis: Some(listen.duration_millis),
                        disc_number: listen.disc_number,
                        track_number: listen.track_number,
                        year: listen.year,
                        release_date: listen.release_date.as_deref(),
                        source_format: listen.source_format.as_deref(),
                        musicbrainz_recording_id: listen.musicbrainz_recording_id.as_deref(),
                        musicbrainz_release_track_id: listen
                            .musicbrainz_release_track_id
                            .as_deref(),
                        ..Default::default()
                    },
                )
                .await?;
                after = listen.media_uri;
            }
        }
    }
    transaction.commit().await?;
    crate::collections::install_collection_facts_on(connection).await?;
    crate::playlists::install_playlist_facts_on(connection).await?;
    Ok(())
}
