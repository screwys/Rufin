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

PRAGMA user_version = 56;

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
    playlist_key INTEGER PRIMARY KEY,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
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
    playlist_folder_key INTEGER REFERENCES playlist_folders(folder_key) ON DELETE SET NULL,
    provider_revision TEXT,
    valid_until INTEGER,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS playlists_global_object_idx ON playlists(object_id)
    WHERE source_key IS NULL;

CREATE INDEX IF NOT EXISTS playlists_order_idx ON playlists(source_key, sort_text, playlist_key);
CREATE INDEX IF NOT EXISTS playlists_membership_idx ON playlists(source_key,indexed,playlist_key);

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
    playlist_folder_key INTEGER REFERENCES playlist_folders(folder_key) ON DELETE SET NULL,
    definition_json TEXT NOT NULL CHECK (json_valid(definition_json)),
    artwork_bytes BLOB,
    artwork_revision TEXT,
    artwork_binding BLOB,
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (object_id),
    UNIQUE (position)
) STRICT;

CREATE INDEX IF NOT EXISTS smart_playlists_title_idx
    ON smart_playlists(normalized_name, smart_playlist_key);

CREATE TABLE IF NOT EXISTS playlist_folders (
    folder_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    smart INTEGER NOT NULL CHECK(smart IN(0,1)),
    playlist_cover_key INTEGER REFERENCES playlists ON DELETE SET NULL,
    smart_cover_key INTEGER REFERENCES smart_playlists ON DELETE SET NULL
) STRICT;
CREATE INDEX IF NOT EXISTS playlists_folder_idx ON playlists(playlist_folder_key,playlist_key);
CREATE TRIGGER IF NOT EXISTS playlists_folder_cover_update AFTER UPDATE OF playlist_folder_key,name,indexed ON playlists BEGIN UPDATE playlist_folders SET playlist_cover_key=(SELECT child.playlist_key FROM playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed) ORDER BY child.playlist_key LIMIT 1) WHERE folder_key IN(OLD.playlist_folder_key,NEW.playlist_folder_key) AND smart=0 AND NOT EXISTS(SELECT 1 FROM playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key AND child.playlist_key=playlist_folders.playlist_cover_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed)); END;
CREATE TRIGGER IF NOT EXISTS playlists_folder_cover_delete AFTER DELETE ON playlists BEGIN UPDATE playlist_folders SET playlist_cover_key=(SELECT child.playlist_key FROM playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed) ORDER BY child.playlist_key LIMIT 1) WHERE folder_key =OLD.playlist_folder_key AND smart=0 AND NOT EXISTS(SELECT 1 FROM playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key AND child.playlist_key=playlist_folders.playlist_cover_key AND child.name IS NOT NULL AND (child.owner='rufin' OR child.indexed)); END;
CREATE INDEX IF NOT EXISTS smart_playlists_folder_idx ON smart_playlists(playlist_folder_key,smart_playlist_key);
CREATE TRIGGER IF NOT EXISTS smart_playlists_folder_cover_update AFTER UPDATE OF playlist_folder_key ON smart_playlists BEGIN UPDATE playlist_folders SET smart_cover_key=(SELECT child.smart_playlist_key FROM smart_playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key ORDER BY child.smart_playlist_key LIMIT 1) WHERE folder_key IN(OLD.playlist_folder_key,NEW.playlist_folder_key) AND smart=1 AND NOT EXISTS(SELECT 1 FROM smart_playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key AND child.smart_playlist_key=playlist_folders.smart_cover_key); END;
CREATE TRIGGER IF NOT EXISTS smart_playlists_folder_cover_delete AFTER DELETE ON smart_playlists BEGIN UPDATE playlist_folders SET smart_cover_key=(SELECT child.smart_playlist_key FROM smart_playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key ORDER BY child.smart_playlist_key LIMIT 1) WHERE folder_key =OLD.playlist_folder_key AND smart=1 AND NOT EXISTS(SELECT 1 FROM smart_playlists child WHERE child.playlist_folder_key=playlist_folders.folder_key AND child.smart_playlist_key=playlist_folders.smart_cover_key); END;

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
    rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 100),
    extra BLOB
) STRICT;

CREATE INDEX IF NOT EXISTS user_media_favorite_idx ON user_media_state(favorite,media_uri) WHERE favorite=1;

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
CREATE INDEX IF NOT EXISTS listens_connect_key_idx ON listens(coalesce(external_id,'legacy-'||media_uri||'-'||started_at||'-'||listen_key));

CREATE INDEX IF NOT EXISTS listens_media_activity_idx ON listens(media_uri, started_at DESC,listen_key DESC,skipped);
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
    backing_uri TEXT,
    source_id TEXT,
    owners TEXT NOT NULL DEFAULT '[]',
    storage_root TEXT NOT NULL DEFAULT '',
    relative_path TEXT NOT NULL DEFAULT '',
    completed_size INTEGER,
    retired_documents TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (media_uri, encoding)
) STRICT;

CREATE INDEX IF NOT EXISTS connect_media_files_path_idx ON connect_media_files(path,encoding,revision);
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
    observation_stamp INTEGER NOT NULL DEFAULT 0,
    album_count INTEGER NOT NULL DEFAULT 0, track_count INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE IF NOT EXISTS native_media_state (
    media_uri TEXT PRIMARY KEY,
    favorite INTEGER, rating INTEGER, play_count INTEGER, skip_count INTEGER, last_played INTEGER
) STRICT;

CREATE INDEX IF NOT EXISTS native_media_favorite_idx ON native_media_state(favorite,media_uri) WHERE favorite=1;

CREATE TABLE IF NOT EXISTS albums (
    album_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL, media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    input_metadata BLOB,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    title_sort TEXT NOT NULL DEFAULT '', title_search TEXT NOT NULL DEFAULT '', artist_search TEXT NOT NULL DEFAULT '',
    album_artist_sort TEXT, year_sort INTEGER, release_sort TEXT, added_sort INTEGER,
    release_class INTEGER NOT NULL DEFAULT 0,
    track_count INTEGER NOT NULL DEFAULT 0, duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0, play_count INTEGER NOT NULL DEFAULT 0, last_played INTEGER,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    cached_local INTEGER NOT NULL DEFAULT 0,
    normalized_title TEXT,
    native_artwork BLOB,
    artwork_binding BLOB, artwork_track_key INTEGER, artwork_artist_key INTEGER,
    first_seen_at INTEGER,
    release_lookup_identity TEXT, release_identity TEXT,
    source_loudness_analysis_key BLOB NOT NULL DEFAULT (zeroblob(32)),
    loudness_analysis_key BLOB NOT NULL DEFAULT (zeroblob(32)),
    accepted_audio BLOB,
    title TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.title')) VIRTUAL,
    display_artist TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.artist')) VIRTUAL,
    sort_text TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.sort_text')) VIRTUAL,
    year INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.year')) VIRTUAL,
    release_date TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.release_date')) VIRTUAL,
    date_added TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.date_added')) VIRTUAL,
    musicbrainz_release_id TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.musicbrainz_release_id')) VIRTUAL,
    musicbrainz_release_group_id TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.musicbrainz_release_group_id')) VIRTUAL,
    is_compilation INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.is_compilation')) VIRTUAL,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS albums_native_artwork_idx ON albums(source_key,native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS albums_artwork_track_idx ON albums(artwork_track_key) WHERE artwork_track_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS albums_artwork_artist_idx ON albums(artwork_artist_key) WHERE artwork_artist_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS albums_source_idx ON albums(source_key,indexed,album_key);

CREATE TABLE IF NOT EXISTS tracks (
    track_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES sources ON DELETE SET NULL,
    object_id TEXT,
    media_uri TEXT NOT NULL UNIQUE,
    album_key INTEGER REFERENCES albums ON DELETE SET NULL,
    local_file_key INTEGER REFERENCES local_files ON DELETE SET NULL,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    source_path TEXT,
    cue_path TEXT, cue_start_millis INTEGER, cue_end_millis INTEGER,
    first_seen_at INTEGER,
    native_artwork BLOB,
    artwork_kind INTEGER NOT NULL DEFAULT 0, artwork_artist_key INTEGER,
    source_loudness_analysis_key BLOB NOT NULL DEFAULT (zeroblob(32)),
    loudness_analysis_key BLOB NOT NULL DEFAULT (zeroblob(32)),
    album_title_sort TEXT, album_artist_sort TEXT, album_year_sort INTEGER,
    local_play_count INTEGER NOT NULL DEFAULT 0,
    displayed_play_count INTEGER NOT NULL DEFAULT 0, displayed_last_played INTEGER,
    displayed_skip_count INTEGER NOT NULL DEFAULT 0, is_downloaded INTEGER NOT NULL DEFAULT 0,
    title TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.title')) VIRTUAL,
    display_artist TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.artist')) VIRTUAL,
    display_album TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.album')) VIRTUAL,
    sort_text TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.sort_text')) VIRTUAL,
    normalized_search TEXT NOT NULL DEFAULT '',
    duration_millis INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.duration_millis'),0)) STORED,
    disc_number INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.disc_number'),0)) VIRTUAL,
    track_number INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.track_number'),0)) VIRTUAL,
    year INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.year')) VIRTUAL,
    release_date TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.release_date')) VIRTUAL,
    date_added TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.date_added')) VIRTUAL,
    source_format TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.source_format')) VIRTUAL,
    bpm INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.bpm')) VIRTUAL,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS tracks_native_artwork_idx ON tracks(source_key,native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_artwork_artist_idx ON tracks(artwork_artist_key) WHERE artwork_artist_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_order_idx ON tracks(source_key,indexed,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_duration_idx ON tracks(track_key,duration_millis);
CREATE INDEX IF NOT EXISTS tracks_local_file_idx ON tracks(local_file_key) WHERE local_file_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_source_path_idx ON tracks(source_key,source_path,album_key);
CREATE INDEX IF NOT EXISTS tracks_source_cue_idx ON tracks(source_key,cue_path,object_id) WHERE cue_path IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_album_order_idx ON tracks(album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_artist_order_idx ON tracks(source_key,indexed,display_artist,album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_year_idx ON tracks(source_key,indexed,year,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_release_idx ON tracks(source_key,indexed,release_date,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_added_idx ON tracks(source_key,indexed,date_added,first_seen_at,track_key);
CREATE INDEX IF NOT EXISTS tracks_raw_album_year_idx ON tracks(album_key,coalesce(cue_path,source_path),disc_number,track_number,object_id) WHERE indexed=1 AND year IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_raw_album_release_idx ON tracks(album_key,coalesce(cue_path,source_path),disc_number,track_number,object_id) WHERE indexed=1 AND release_date IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_local_play_order_idx ON tracks(source_key,indexed,local_play_count DESC,sort_text,media_uri);
CREATE INDEX IF NOT EXISTS tracks_global_play_order_idx ON tracks(indexed,local_play_count DESC,sort_text,media_uri);
CREATE INDEX IF NOT EXISTS tracks_local_shuffle_idx ON tracks(source_key,local_play_count,((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_global_local_shuffle_idx ON tracks(local_play_count,((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_shuffle_idx ON tracks(source_key,((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_global_shuffle_idx ON tracks(((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_album_local_audio_idx ON tracks(album_key) WHERE indexed=1 AND source_loudness_analysis_key<>loudness_analysis_key;
CREATE INDEX IF NOT EXISTS tracks_indexed_idx ON tracks(indexed,source_key,track_key);

CREATE TABLE IF NOT EXISTS artists (
    artist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    artwork_binding BLOB, artwork_album_key INTEGER, artwork_track_key INTEGER,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    musicbrainz_artist_id TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.musicbrainz_artist_id')) VIRTUAL,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS artists_native_artwork_idx ON artists(source_key,native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS artists_artwork_album_idx ON artists(artwork_album_key) WHERE artwork_album_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS artists_artwork_track_idx ON artists(artwork_track_key) WHERE artwork_track_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS artists_order_idx ON artists(source_key,sort_text,artist_key);
CREATE INDEX IF NOT EXISTS artists_membership_idx ON artists(source_key,indexed,artist_key);

CREATE TABLE IF NOT EXISTS genres (
    genre_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS genres_native_artwork_idx ON genres(source_key,native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS genres_order_idx ON genres(source_key,sort_text,genre_key);
CREATE INDEX IF NOT EXISTS genres_membership_idx ON genres(source_key,indexed,genre_key);

CREATE TABLE IF NOT EXISTS moods (
    mood_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS moods_order_idx ON moods(source_key,sort_text,mood_key);
CREATE INDEX IF NOT EXISTS moods_membership_idx ON moods(source_key,indexed,mood_key);

CREATE TABLE IF NOT EXISTS folders (
    folder_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS folders_native_artwork_idx ON folders(source_key,native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS folders_order_idx ON folders(source_key,sort_text,folder_key);
CREATE INDEX IF NOT EXISTS folders_membership_idx ON folders(source_key,indexed,folder_key);

CREATE INDEX IF NOT EXISTS albums_title_idx ON albums(source_key,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_artist_idx ON albums(source_key,(album_artist_sort IS NULL),album_artist_sort,(year_sort IS NULL),year_sort,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_year_idx ON albums(source_key,year_sort,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_release_idx ON albums(source_key,release_sort,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_added_idx ON albums(source_key,added_sort,title_sort,album_key);

CREATE TABLE IF NOT EXISTS album_artists (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, artist_key)
) STRICT;

CREATE INDEX IF NOT EXISTS album_artists_artist_idx ON album_artists(artist_key, album_key);

CREATE TABLE IF NOT EXISTS track_album_artists (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY(track_key,artist_key),
    UNIQUE(track_key,position)
) STRICT;
CREATE INDEX IF NOT EXISTS track_album_artists_artist_idx ON track_album_artists(artist_key,track_key);

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
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, folder_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_folders_folder_idx ON track_folders(folder_key, track_key);

CREATE TABLE IF NOT EXISTS album_release_types (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    release_type TEXT NOT NULL CHECK (release_type <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position)
) STRICT;

CREATE TABLE IF NOT EXISTS album_release_observations (
    album_key INTEGER PRIMARY KEY REFERENCES albums ON DELETE CASCADE,
    lookup_identity TEXT NOT NULL,
    release_types BLOB NOT NULL
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

CREATE TABLE IF NOT EXISTS fingerprint_cache (
    media_uri TEXT PRIMARY KEY REFERENCES tracks(media_uri) ON DELETE CASCADE,
    analysis_key BLOB NOT NULL CHECK(length(analysis_key)=32),
    version TEXT NOT NULL, fingerprint TEXT NOT NULL, duration_seconds INTEGER NOT NULL,
    lookup_key TEXT NOT NULL DEFAULT ''
) STRICT;

CREATE TABLE IF NOT EXISTS metadata_lookup_cache (
    key TEXT PRIMARY KEY, payload BLOB NOT NULL,
    written_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS waveform_cache (
    media_uri TEXT PRIMARY KEY REFERENCES tracks(media_uri) ON DELETE CASCADE,
    analysis_key BLOB NOT NULL CHECK(length(analysis_key)=32),
    peaks TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS loudness_measurements (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    entity_kind TEXT NOT NULL, entity_key INTEGER NOT NULL,
    analysis_key BLOB NOT NULL,
    integrated_lufs REAL, true_peak REAL,
    PRIMARY KEY(source_key,entity_kind,entity_key)
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

DROP TRIGGER IF EXISTS lyrics_cache_insert;
DROP TRIGGER IF EXISTS lyrics_cache_update;
DROP TRIGGER IF EXISTS lyrics_cache_delete;
DROP INDEX IF EXISTS lyrics_cache_changes;
DROP TABLE IF EXISTS lyrics_cache_revision;
CREATE TABLE IF NOT EXISTS artwork_previews(family TEXT PRIMARY KEY,hash TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS local_files (
    local_file_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    root_id TEXT NOT NULL DEFAULT '',
    relative_path TEXT NOT NULL,
    playlist_identity TEXT,
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
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    parent_path TEXT NOT NULL DEFAULT '',
    parsed_stamp TEXT,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'rejected', 'unreadable', 'observed')),
    UNIQUE (source_key, path)
) STRICT;

CREATE INDEX IF NOT EXISTS local_files_identity_idx ON local_files(source_key, device_id, inode);

CREATE INDEX IF NOT EXISTS local_files_native_id_idx ON local_files(source_key, native_id) WHERE native_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS local_files_kind_path_idx ON local_files(source_key, kind, path);
CREATE INDEX IF NOT EXISTS local_files_parent_idx ON local_files(source_key,parent_path,observed_stamp,path);
CREATE INDEX IF NOT EXISTS local_files_root_path_idx ON local_files(source_key,root,relative_path);

CREATE INDEX IF NOT EXISTS local_files_playlist_idx ON local_files(source_key, local_file_key)
    WHERE lower(path) LIKE '%.m3u' OR lower(path) LIKE '%.m3u8' OR lower(path) LIKE '%.pls' OR lower(path) LIKE '%.xspf';

CREATE TABLE IF NOT EXISTS local_file_access_roots (
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    root TEXT NOT NULL,
    root_id TEXT NOT NULL DEFAULT '',
    PRIMARY KEY(source_key,root_id,root)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS local_file_observations (
    local_file_key INTEGER PRIMARY KEY REFERENCES local_files ON DELETE CASCADE,
    path TEXT NOT NULL, root TEXT NOT NULL, relative_path TEXT NOT NULL,
    size_bytes INTEGER, mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER, native_id TEXT,
    revision TEXT, observed_stamp INTEGER NOT NULL DEFAULT 0, parent_path TEXT NOT NULL DEFAULT '',
    parsed_stamp TEXT, accepted_facts TEXT, state TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS local_file_observations_path ON local_file_observations(path,local_file_key);
CREATE INDEX IF NOT EXISTS local_file_observations_identity ON local_file_observations(device_id,inode,local_file_key);
CREATE INDEX IF NOT EXISTS local_file_observations_native ON local_file_observations(native_id,local_file_key) WHERE native_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS local_file_observations_parent ON local_file_observations(root,parent_path,observed_stamp,path);
CREATE VIEW IF NOT EXISTS observed_local_files AS
    SELECT file.local_file_key,file.source_key,access.path,access.root,access.relative_path,file.root_id,
        file.kind,access.size_bytes,access.mtime_ns,access.device_id,access.inode,access.native_id,
        file.picture_index,file.artist_pictures,access.revision,file.parse_version,
        access.observed_stamp,access.parent_path,
        CASE WHEN access.accepted_facts IS file.parsed_stamp THEN access.parsed_stamp END parsed_stamp,
        CASE WHEN access.state IN('accepted','rejected') AND access.accepted_facts IS NOT file.parsed_stamp THEN 'observed' ELSE access.state END state
    FROM local_files file JOIN local_file_observations access USING(local_file_key);
CREATE VIEW IF NOT EXISTS local_track_access AS
    SELECT track.track_key,track.source_key,backing.path source_path,owner.path cue_path,
        CASE WHEN track.cue_path IS NULL THEN backing.local_file_key ELSE owner.local_file_key END local_file_key
    FROM tracks track
    JOIN local_files canonical ON canonical.source_key=track.source_key AND canonical.path=track.source_path
    JOIN local_file_observations backing ON backing.local_file_key=canonical.local_file_key
    LEFT JOIN local_files cue ON cue.source_key=track.source_key AND cue.path=track.cue_path
    LEFT JOIN local_file_observations owner ON owner.local_file_key=cue.local_file_key;

CREATE TABLE IF NOT EXISTS local_file_dependencies (
    local_file_key INTEGER NOT NULL REFERENCES local_files ON DELETE CASCADE,
    dependency_path TEXT NOT NULL CHECK (dependency_path <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    accepted_inputs TEXT,
    PRIMARY KEY (local_file_key, position),
    UNIQUE (local_file_key, dependency_path)
) STRICT;

CREATE INDEX IF NOT EXISTS local_file_dependencies_path_idx ON local_file_dependencies(dependency_path, local_file_key);
CREATE VIEW IF NOT EXISTS observed_file_dependencies AS
    SELECT edge.local_file_key,coalesce(access.path,edge.dependency_path) dependency_path,edge.position,
        CASE WHEN edge.accepted_inputs=json_array(backing.size_bytes,backing.mtime_ns,backing.revision)
            AND access.accepted_facts IS backing.parsed_stamp AND access.parsed_stamp IS NOT NULL
            THEN json_array(json_extract(access.parsed_stamp,'$[0]'),json_extract(access.parsed_stamp,'$[1]'),json_extract(access.parsed_stamp,'$[2]'))
            ELSE edge.accepted_inputs END accepted_inputs
    FROM local_file_dependencies edge JOIN local_files owner USING(local_file_key)
    LEFT JOIN local_files backing ON backing.source_key=owner.source_key AND backing.path=edge.dependency_path
    LEFT JOIN local_file_observations access ON access.local_file_key=backing.local_file_key;

CREATE TABLE IF NOT EXISTS local_access_metadata (
    access_uri TEXT PRIMARY KEY,
    size_bytes INTEGER NOT NULL, mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
    parser_version INTEGER NOT NULL, loudness_analysis_key BLOB
) STRICT;

CREATE TRIGGER IF NOT EXISTS local_play_count_listen_insert AFTER INSERT ON listens BEGIN
             UPDATE tracks SET local_play_count=local_play_count+1 WHERE media_uri=NEW.media_uri;
         END;
         CREATE TRIGGER IF NOT EXISTS local_play_count_listen_delete AFTER DELETE ON listens BEGIN
             UPDATE tracks SET local_play_count=local_play_count-1 WHERE media_uri=OLD.media_uri;
         END;
CREATE VIEW IF NOT EXISTS local_access_files AS
          SELECT locator.local_access_file_key,locator.source_key,locator.media_uri,
                 locator.origin,locator.path,locator.root,locator.relative_path,
                 COALESCE(metadata.size_bytes,0) size_bytes,COALESCE(metadata.mtime_ns,0) mtime_ns,
                 metadata.device_id,metadata.inode,COALESCE(metadata.parser_version,1) parser_version,
                 locator.access_uri,
                 metadata.loudness_analysis_key
          FROM local_locators locator
          LEFT JOIN local_access_metadata metadata USING(access_uri);

CREATE TABLE IF NOT EXISTS native_playlist_pending (
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL, track_key INTEGER NOT NULL REFERENCES tracks,
    position INTEGER NOT NULL,
    PRIMARY KEY(playlist_key,object_id)
) STRICT;
"#;

pub(crate) async fn initialize_durable(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let version = pragma(connection, "user_version").await?;
    if !matches!(version, 0 | 47 | 48 | 49 | 50 | 51 | 53 | 54 | 55 | 56) {
        return Err(crate::LibraryError::Migration(format!(
            "unsupported library schema {version}"
        )));
    }
    let mut transaction = connection.begin().await?;
    if version < 50 {
        sqlx::query("DROP VIEW IF EXISTS local_access_files")
            .execute(&mut *transaction)
            .await?;
    }
    if version < 54 {
        sqlx::query("DROP VIEW IF EXISTS local_track_access")
            .execute(&mut *transaction)
            .await?;
    }
    if version > 0 && version < 51 {
        crate::catalog::index::upgrade(&mut transaction).await?;
    } else if version == 51 {
        crate::catalog::index::unite_totals(&mut transaction).await?;
    }
    if version > 0 {
        for table in ["playlists", "smart_playlists"] {
            let present: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='playlist_folder_key')"
            ).bind(table).fetch_one(&mut *transaction).await?;
            if !present {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "ALTER TABLE {table} ADD COLUMN playlist_folder_key INTEGER REFERENCES playlist_folders(folder_key) ON DELETE SET NULL"
                )))
                .execute(&mut *transaction).await?;
            }
        }
        let previous_folder_layout: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='playlist_folder_members')"
        ).fetch_one(&mut *transaction).await?;
        if previous_folder_layout {
            sqlx::raw_sql("DROP TRIGGER IF EXISTS playlists_folder_cover_update;
                DROP TRIGGER IF EXISTS playlist_folder_members_cover_insert;
                DROP TRIGGER IF EXISTS playlist_folder_members_cover_delete;
                DROP TRIGGER IF EXISTS playlist_folder_members_cover_update;
                DROP TRIGGER IF EXISTS smart_playlist_folder_members_cover_insert;
                DROP TRIGGER IF EXISTS smart_playlist_folder_members_cover_delete;
                DROP TRIGGER IF EXISTS smart_playlist_folder_members_cover_update;
                UPDATE playlists SET playlist_folder_key=(SELECT folder_key FROM playlist_folder_members member WHERE member.playlist_key=playlists.playlist_key);
                UPDATE smart_playlists SET playlist_folder_key=(SELECT folder_key FROM smart_playlist_folder_members member WHERE member.smart_playlist_key=smart_playlists.smart_playlist_key);
                DROP TABLE playlist_folder_members;
                DROP TABLE smart_playlist_folder_members;")
                .execute(&mut *transaction).await?;
        }
    }
    sqlx::raw_sql(STORE_SCHEMA)
        .execute(&mut *transaction)
        .await?;
    for table in ["local_files", "local_file_access_roots"] {
        if !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name='root_id')",
        )
        .bind(table)
        .fetch_one(&mut *transaction)
        .await?
        {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {table} ADD COLUMN root_id TEXT NOT NULL DEFAULT ''"
            )))
            .execute(&mut *transaction)
            .await?;
        }
    }
    if !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('local_files') WHERE name='playlist_identity')",
    )
    .fetch_one(&mut *transaction)
    .await?
    {
        sqlx::query("ALTER TABLE local_files ADD COLUMN playlist_identity TEXT")
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query("CREATE INDEX IF NOT EXISTS local_files_root_identity_idx ON local_files(source_key,root_id,relative_path)")
        .execute(&mut *transaction).await?;
    if !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('user_media_state') WHERE name='extra')",
    )
    .fetch_one(&mut *transaction)
    .await?
    {
        sqlx::query("ALTER TABLE user_media_state ADD COLUMN extra BLOB")
            .execute(&mut *transaction)
            .await?;
    }
    let old_blob: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('connect_media_files') WHERE name='hash')",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if old_blob {
        sqlx::raw_sql("DROP INDEX IF EXISTS connect_media_files_hash_idx; ALTER TABLE connect_media_files DROP COLUMN hash;")
            .execute(&mut *transaction).await?;
    }
    for (column, declaration) in [
        ("source_id", "TEXT"),
        ("owners", "TEXT NOT NULL DEFAULT '[]'"),
        ("storage_root", "TEXT NOT NULL DEFAULT ''"),
        ("relative_path", "TEXT NOT NULL DEFAULT ''"),
        ("completed_size", "INTEGER"),
        ("retired_documents", "TEXT NOT NULL DEFAULT '[]'"),
    ] {
        let present: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('connect_media_files') WHERE name=?1)",
        )
        .bind(column)
        .fetch_one(&mut *transaction)
        .await?;
        if !present {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE connect_media_files ADD COLUMN {column} {declaration}"
            )))
            .execute(&mut *transaction)
            .await?;
        }
    }
    sqlx::query("CREATE INDEX IF NOT EXISTS download_source_idx ON connect_media_files(source_id,media_uri,encoding)")
        .execute(&mut *transaction).await?;
    if version == 47 {
        sqlx::query(
            "INSERT OR IGNORE INTO local_file_access_roots(source_key,root) SELECT DISTINCT source_key,root FROM local_files",
        )
        .execute(&mut *transaction)
        .await?;
    }
    if matches!(version, 47 | 48) {
        // Albums derived from file tags are admitted through their current tracks.
        sqlx::raw_sql("UPDATE albums SET indexed=0 WHERE indexed=1 AND input_metadata IS NULL;
            UPDATE tracks SET indexed=0 WHERE indexed=1 AND source_key IS NOT NULL AND object_id IS NULL;")
            .execute(&mut *transaction)
            .await?;
    }
    if version == 0 {
        crate::smart_playlists::create_default_smart_playlists(&mut transaction).await?;
    }
    if version < 50 {
        crate::migration::current::separate_file_access(&mut transaction).await?;
    }
    crate::migration::current::narrow_access_metadata(&mut transaction).await?;
    sqlx::raw_sql("UPDATE connect_media_files AS download SET source_id=source.object_id FROM tracks track JOIN sources source USING(source_key) WHERE download.source_id IS NULL AND download.media_uri=track.media_uri;
        UPDATE local_locators AS locator SET source_key=track.source_key FROM tracks track WHERE locator.source_key IS NULL AND locator.origin='download' AND locator.media_uri=track.media_uri AND track.source_key IS NOT NULL;")
        .execute(&mut *transaction).await?;
    if version < 51 {
        crate::catalog::index::initialize(&mut transaction).await?;
    }
    transaction.commit().await?;
    Ok(())
}
