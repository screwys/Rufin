//! The Store holds base facts and user data. Aggregates, sort copies and artwork choices are
//! not stored.
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
pub(crate) const STORE_VERSION: i64 = 60;

/// Bits of the `flags` column of tracks, albums, artists, tags and folders, copied from the
/// artwork binding and the details row so the Catalog load reads neither.
pub(crate) const HAS_PICTURE: i64 = 1;
/// Tracks only.
pub(crate) const TRACK_CUE: i64 = 2;
/// Albums only: the release class code, from its compilation flag and release types, in
/// three bits from here.
pub(crate) const ALBUM_RELEASE_SHIFT: i64 = 2;

/// Roles of `track_links` and `album_links`. Albums use the artist and genre roles.
pub(crate) const LINK_ARTIST: i64 = 0;
pub(crate) const LINK_ALBUM_ARTIST: i64 = 1;
pub(crate) const LINK_GENRE: i64 = 2;
pub(crate) const LINK_MOOD: i64 = 3;
pub(crate) const LINK_FOLDER: i64 = 4;

pub(crate) const STORE_SCHEMA: &str = r#"PRAGMA user_version = 60;

CREATE TABLE sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    kind TEXT,
    display_name TEXT NOT NULL DEFAULT '',
    freshness BLOB,
    catalog_complete INTEGER NOT NULL DEFAULT 0 CHECK (catalog_complete IN (0,1)),
    -- Server time (unix seconds) at the start of the last completed listing or catch-up.
    synced_at INTEGER
) STRICT;

CREATE TABLE albums (
    album_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    cached_local INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL DEFAULT '',
    display_artist TEXT NOT NULL DEFAULT '',
    sort_text TEXT NOT NULL DEFAULT '',
    year INTEGER,
    release_date TEXT,
    date_added TEXT,
    -- release_date as yyyymmdd, missing parts zero; date_added, else first_seen_at, in unix
    -- seconds. The Catalog load reads these and parses no text.
    released INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER,
    is_compilation INTEGER,
    first_seen_at INTEGER,
    release_identity TEXT,
    release_lookup_identity TEXT,
    native_favorite INTEGER,
    native_rating INTEGER,
    native_play_count INTEGER,
    native_last_played INTEGER,
    -- The rest of the resolved AlbumMetadata: MusicBrainz ids, credits, genres, release types.
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    -- The header a source observed, when it observed one.
    input_metadata BLOB,
    native_artwork BLOB,
    flags INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_key,object_id)
) STRICT;

CREATE TABLE tracks (
    track_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES sources ON DELETE SET NULL,
    object_id TEXT,
    media_uri TEXT NOT NULL UNIQUE,
    album_key INTEGER REFERENCES albums ON DELETE SET NULL,
    indexed INTEGER NOT NULL DEFAULT 0,
    hidden_since INTEGER,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL DEFAULT '',
    display_artist TEXT NOT NULL DEFAULT '',
    display_album TEXT NOT NULL DEFAULT '',
    sort_text TEXT NOT NULL DEFAULT '',
    duration_millis INTEGER NOT NULL DEFAULT 0,
    disc_number INTEGER NOT NULL DEFAULT 0,
    track_number INTEGER NOT NULL DEFAULT 0,
    year INTEGER,
    release_date TEXT,
    date_added TEXT,
    released INTEGER NOT NULL DEFAULT 0,
    added_at INTEGER,
    first_seen_at INTEGER,
    bpm INTEGER,
    comment TEXT,
    source_format TEXT,
    flags INTEGER NOT NULL DEFAULT 0,
    native_favorite INTEGER,
    native_rating INTEGER,
    native_play_count INTEGER,
    native_last_played INTEGER,
    UNIQUE(source_key,object_id)
) STRICT;
CREATE INDEX tracks_album_idx ON tracks(album_key) WHERE album_key IS NOT NULL;

-- Read by key only: the rest of TrackMetadata, the picture binding, paths, the CUE window and
-- the audio keys. A track without a row has none of them.
CREATE TABLE track_details (
    track_key INTEGER PRIMARY KEY REFERENCES tracks ON DELETE CASCADE,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_artwork BLOB,
    source_path TEXT,
    cue_path TEXT, cue_start_millis INTEGER, cue_end_millis INTEGER,
    -- When the audio may have changed: the file or server revision plus the CUE window.
    audio_revision BLOB,
    -- A hash of the audio stream; NULL until known. Analysis is keyed by it.
    audio_hash BLOB
) STRICT;
CREATE INDEX track_details_path_idx ON track_details(source_path) WHERE source_path IS NOT NULL;
CREATE INDEX track_details_cue_idx ON track_details(cue_path) WHERE cue_path IS NOT NULL;

CREATE TABLE artists (
    artist_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    name TEXT NOT NULL DEFAULT '',
    sort_text TEXT,
    musicbrainz_artist_id TEXT,
    native_favorite INTEGER,
    native_rating INTEGER,
    native_play_count INTEGER,
    native_last_played INTEGER,
    native_artwork BLOB,
    flags INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_key,object_id)
) STRICT;

-- Genres (kind 0) and moods (kind 1).
CREATE TABLE tags (
    tag_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    kind INTEGER NOT NULL CHECK (kind IN (0,1)),
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    name TEXT NOT NULL DEFAULT '',
    sort_text TEXT,
    native_artwork BLOB,
    flags INTEGER NOT NULL DEFAULT 0,
    -- The album URIs of the set-once mosaic a genre or mood without a picture shows.
    mosaic TEXT,
    UNIQUE(source_key,kind,object_id)
) STRICT;

CREATE TABLE folders (
    folder_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    name TEXT,
    sort_text TEXT,
    native_artwork BLOB,
    flags INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_key,object_id)
) STRICT;

-- Ordered credits, tags and folders of a track; the role names the target's table.
CREATE TABLE track_links (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    role INTEGER NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    target INTEGER NOT NULL,
    PRIMARY KEY(track_key,role,position)
) STRICT, WITHOUT ROWID;
CREATE INDEX track_links_target_idx ON track_links(role,target,track_key);

CREATE TABLE album_links (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    role INTEGER NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    target INTEGER NOT NULL,
    PRIMARY KEY(album_key,role,position)
) STRICT, WITHOUT ROWID;
CREATE INDEX album_links_target_idx ON album_links(role,target,album_key);

CREATE TABLE album_release_observations (
    album_key INTEGER PRIMARY KEY REFERENCES albums ON DELETE CASCADE,
    lookup_identity TEXT NOT NULL,
    release_types BLOB NOT NULL
) STRICT;

CREATE TABLE home_entries (
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

CREATE TABLE artwork_previews(family TEXT PRIMARY KEY,hash TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE files (
    file_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    path TEXT NOT NULL CHECK (path <> ''),
    root TEXT NOT NULL CHECK (root <> ''),
    -- '/'-separated on every platform; the walk order key.
    relative_path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('media', 'cue', 'image', 'directory')),
    size_bytes INTEGER, mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
    native_id TEXT, revision TEXT, parse_version INTEGER,
    picture_index INTEGER, artist_pictures TEXT, playlist_identity TEXT,
    state TEXT NOT NULL CHECK (state IN ('accepted', 'rejected', 'unreadable')),
    UNIQUE (source_key, path)
) STRICT;
-- Component order: 'a' < 'a/b' < 'a.flac', the order a depth-first walk of name-sorted
-- listings produces.
CREATE INDEX files_walk_idx ON files(source_key, root, replace(relative_path, '/', char(1)));
CREATE INDEX files_identity_idx ON files(source_key, device_id, inode) WHERE inode IS NOT NULL;
CREATE INDEX files_native_id_idx ON files(source_key, native_id) WHERE native_id IS NOT NULL;
CREATE INDEX files_playlist_idx ON files(source_key, file_key)
    WHERE lower(path) LIKE '%.m3u' OR lower(path) LIKE '%.m3u8' OR lower(path) LIKE '%.pls' OR lower(path) LIKE '%.xspf';

-- The input files of an accepted CUE sheet, in sheet order.
CREATE TABLE file_inputs (
    file_key INTEGER NOT NULL REFERENCES files ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    path TEXT NOT NULL CHECK (path <> ''),
    PRIMARY KEY (file_key, position)
) STRICT, WITHOUT ROWID;
CREATE INDEX file_inputs_path_idx ON file_inputs(path);

-- The user's own favorite and rating. They override the source's values until a delivery the
-- source accepted drops them.
CREATE TABLE user_state (
    media_uri TEXT PRIMARY KEY CHECK (media_uri <> ''),
    favorite INTEGER CHECK (favorite IS NULL OR favorite IN (0,1)),
    rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 100),
    changed_at INTEGER NOT NULL
) STRICT;

-- Favorites and ratings waiting for their server. A cleared rating has no value.
CREATE TABLE outbox (
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    kind TEXT NOT NULL CHECK (kind IN ('favorite','rating')),
    value INTEGER,
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0),
    PRIMARY KEY (media_uri, kind)
) STRICT;
CREATE INDEX outbox_due_idx ON outbox(next_attempt_at);

CREATE TABLE listens (
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
CREATE INDEX listens_history_idx ON listens(source_id, started_at DESC, listen_key DESC);
CREATE INDEX listens_recent_idx ON listens(started_at DESC, listen_key DESC);
CREATE INDEX listens_media_activity_idx ON listens(media_uri, started_at DESC,listen_key DESC,skipped);
CREATE INDEX listens_period_idx ON listens(local_period) WHERE skipped=0;

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

-- Counts carried over from 0.15 and 0.16, with their own display copy.
CREATE TABLE legacy_activity (
    source_id TEXT NOT NULL, period TEXT NOT NULL, item_kind TEXT NOT NULL,
    track_object_id TEXT NOT NULL, play_count INTEGER NOT NULL, skip_count INTEGER NOT NULL,
    last_played_at INTEGER,
    title TEXT NOT NULL DEFAULT '', artist TEXT NOT NULL DEFAULT '', album TEXT NOT NULL DEFAULT '',
    PRIMARY KEY(source_id,period,item_kind,track_object_id)
) STRICT;

CREATE TABLE playlist_folders (
    folder_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    smart INTEGER NOT NULL CHECK(smart IN(0,1))
) STRICT;

CREATE TABLE playlists (
    playlist_key INTEGER PRIMARY KEY,
    owner TEXT NOT NULL DEFAULT 'rufin' CHECK(owner IN ('rufin','source')),
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
    source_key INTEGER REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    artwork_bytes BLOB,
    artwork_mime TEXT,
    artwork_revision TEXT,
    artwork_binding BLOB,
    writable INTEGER NOT NULL DEFAULT 1,
    metadata_writable INTEGER NOT NULL DEFAULT 1,
    playlist_folder_key INTEGER REFERENCES playlist_folders(folder_key) ON DELETE SET NULL,
    provider_revision TEXT,
    valid_until INTEGER,
    -- The track URIs of the set-once mosaic.
    mosaic TEXT,
    UNIQUE (source_key, object_id)
) STRICT;
CREATE UNIQUE INDEX playlists_global_object_idx ON playlists(object_id) WHERE source_key IS NULL;
CREATE INDEX playlists_folder_idx ON playlists(playlist_folder_key) WHERE playlist_folder_key IS NOT NULL;

CREATE TABLE playlist_entries (
    playlist_entry_key INTEGER PRIMARY KEY,
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    track_key INTEGER NOT NULL REFERENCES tracks,
    -- A fractional rank key; entries sort by rank, then by key.
    rank BLOB NOT NULL,
    UNIQUE (playlist_key, object_id)
) STRICT;
CREATE INDEX playlist_entries_rank_idx ON playlist_entries(playlist_key, rank);
CREATE INDEX playlist_entries_track_idx ON playlist_entries(track_key);

CREATE TABLE playlist_file_links (
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

CREATE TABLE playlist_file_ignored (
    source_id TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY(source_id,path)
) STRICT;

CREATE TABLE playlist_file_settings (
    source_id TEXT PRIMARY KEY,
    auto_save INTEGER NOT NULL
) STRICT;

CREATE TABLE smart_playlists (
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
    -- The track URIs of the set-once mosaic.
    mosaic TEXT,
    UNIQUE (object_id),
    UNIQUE (position)
) STRICT;
CREATE INDEX smart_playlists_folder_idx ON smart_playlists(playlist_folder_key) WHERE playlist_folder_key IS NOT NULL;

CREATE TABLE queue_contexts (
    context_key INTEGER PRIMARY KEY,
    context TEXT NOT NULL UNIQUE
) STRICT;

-- The queue in its order by rank, and in its play order by shuffle_rank while shuffle is on.
CREATE TABLE queue_entries (
    entry_id INTEGER PRIMARY KEY,
    track_key INTEGER NOT NULL REFERENCES tracks,
    rank BLOB NOT NULL,
    shuffle_rank BLOB,
    provenance TEXT,
    context_key INTEGER REFERENCES queue_contexts,
    source_rank INTEGER
) STRICT;
CREATE INDEX queue_entries_track_idx ON queue_entries(track_key);
CREATE INDEX queue_entries_context_idx ON queue_entries(context_key) WHERE context_key IS NOT NULL;

CREATE TABLE queue_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    current_index INTEGER,
    progress_millis INTEGER NOT NULL DEFAULT 0 CHECK (progress_millis >= 0),
    repeat_mode TEXT NOT NULL DEFAULT 'none' CHECK (repeat_mode IN ('none', 'one', 'all')),
    shuffled INTEGER NOT NULL DEFAULT 0 CHECK (shuffled IN (0, 1))
) STRICT;

-- A track's downloaded copy, written after its file lands, at the quality it was downloaded
-- with; a later quality setting applies only to new downloads. The reason names the requests and rules that
-- keep it; root and relative_path place a copy in its storage; a copy that is not managed is
-- the track's own local file, which removal leaves alone; retired lists replaced documents
-- that still wait for deletion.
CREATE TABLE downloads (
    media_uri TEXT NOT NULL,
    quality TEXT NOT NULL,
    size INTEGER,
    path TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '[]',
    source_id TEXT,
    root TEXT NOT NULL DEFAULT '',
    relative_path TEXT NOT NULL DEFAULT '',
    managed INTEGER NOT NULL,
    retired TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (media_uri)
) STRICT;
CREATE INDEX downloads_path_idx ON downloads(path);
CREATE INDEX downloads_source_idx ON downloads(source_id, media_uri);

CREATE TABLE local_locators (
    local_access_file_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES sources ON DELETE CASCADE,
    media_uri TEXT NOT NULL CHECK(media_uri<>''),
    origin TEXT NOT NULL CHECK(origin IN ('local','import')),
    path TEXT NOT NULL CHECK(path<>''), root TEXT NOT NULL, relative_path TEXT NOT NULL,
    access_uri TEXT NOT NULL CHECK(access_uri<>''),
    UNIQUE(media_uri,origin)
) STRICT;
CREATE INDEX local_locators_source_idx ON local_locators(source_key,origin);
CREATE INDEX local_locators_path_idx ON local_locators(source_key,path);

CREATE TABLE lyrics_cache (
    lyrics_key INTEGER PRIMARY KEY,
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    authority TEXT NOT NULL CHECK (authority <> ''),
    role TEXT NOT NULL CHECK (role <> ''),
    language TEXT NOT NULL DEFAULT '',
    script TEXT NOT NULL DEFAULT '',
    lyrics TEXT NOT NULL,
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    -- A person chose these lyrics for a track whose source cannot store them. Connect shares
    -- them, and clearing the lyrics cache keeps them.
    picked INTEGER NOT NULL DEFAULT 0 CHECK (picked IN (0, 1)),
    UNIQUE (media_uri, authority, role, language, script)
) STRICT;
-- What changed lately, for a paired device that follows this device's source lyrics.
CREATE INDEX lyrics_cache_updated_idx ON lyrics_cache(authority, updated_at);

-- Analysis results by audio hash. Expiry and Forget leave them.
CREATE TABLE loudness (audio_hash BLOB PRIMARY KEY, integrated_lufs REAL, true_peak REAL) STRICT, WITHOUT ROWID;
-- members: the 16-hex-character prefixes of the album's indexed members' audio hashes, in hash order.
CREATE TABLE album_loudness (members TEXT PRIMARY KEY, integrated_lufs REAL, true_peak REAL) STRICT, WITHOUT ROWID;
CREATE TABLE waveform (audio_hash BLOB PRIMARY KEY, peaks TEXT NOT NULL) STRICT, WITHOUT ROWID;
CREATE TABLE fingerprint (
    audio_hash BLOB PRIMARY KEY, version TEXT NOT NULL, fingerprint TEXT NOT NULL,
    duration_seconds INTEGER NOT NULL, lookup_key TEXT NOT NULL DEFAULT ''
) STRICT, WITHOUT ROWID;

CREATE TABLE metadata_lookup_cache (
    key TEXT PRIMARY KEY, payload BLOB NOT NULL,
    written_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
) STRICT;

-- The Store's identity and a counter the first Write after a Catalog image raises, so an image
-- of an older state is not read.
CREATE TABLE store_commits (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    store_id BLOB NOT NULL,
    commits INTEGER NOT NULL
) STRICT;
INSERT INTO store_commits VALUES (1, randomblob(16), 0);
"#;

/// Creates a new Store, and refuses any other version than the current one. Released Stores
/// were converted by the importer before this runs; unreleased formats are not migrated.
pub(crate) async fn initialize_durable(connection: &mut SqliteConnection) -> LibraryResult<()> {
    crate::lyrics_tokenizer::register(connection).await?;
    match pragma(connection, "user_version").await? {
        STORE_VERSION => Ok(()),
        0 => {
            let mut transaction = connection.begin().await?;
            sqlx::raw_sql(STORE_SCHEMA)
                .execute(&mut *transaction)
                .await?;
            crate::lyrics_search::initialize(&mut transaction).await?;
            crate::smart_playlists::create_default_smart_playlists(&mut transaction).await?;
            transaction.commit().await?;
            Ok(())
        }
        version => Err(crate::LibraryError::Migration(format!(
            "unsupported library schema {version}"
        ))),
    }
}
