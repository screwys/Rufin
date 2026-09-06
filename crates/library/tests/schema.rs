use std::path::Path;

use library::{Database, ReadCancellation, SourceId};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

const SCHEMA_43: &str = r#"BEGIN IMMEDIATE;
PRAGMA application_id = 1381320270;
PRAGMA user_version = 43;

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
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (source_key, ownership, object_id)
) STRICT;
CREATE INDEX playlists_order_idx ON playlists(source_key, ownership, sort_text, playlist_key);
CREATE INDEX playlists_title_idx ON playlists(source_key, sort_text, playlist_key);
CREATE UNIQUE INDEX playlists_position_idx ON playlists(source_key, position);
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
"#;

async fn connection(path: &Path, create: bool) -> SqliteConnection {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(create),
    )
    .await
    .expect("open schema test connection");
    let catalog = path.with_extension("catalog.sqlite");
    if catalog.exists() {
        super::production_schema::attach_catalog(&mut connection, &catalog)
            .await
            .unwrap();
    }
    connection
}

#[tokio::test]
#[ignore = "requires RUFIN_MIGRATION_STORE and RUFIN_MIGRATION_SOURCES from a real schema-43 installation"]
async fn real_schema_43_snapshot_relocates_and_preserves_owned_state() {
    let original = std::path::PathBuf::from(
        std::env::var_os("RUFIN_MIGRATION_STORE").expect("original Store path"),
    );
    let sources: serde_json::Value = serde_json::from_str(
        &std::env::var("RUFIN_MIGRATION_SOURCES").expect("source identities only"),
    )
    .expect("source identities JSON");
    let configured = sources["configured"]
        .as_array()
        .expect("configured sources")
        .iter()
        .map(|source| SourceId::new(source.as_str().expect("SourceId")))
        .collect::<Vec<_>>();
    let selected = sources["selected"].as_str().map(SourceId::new);
    let directory = tempfile::tempdir().expect("isolated XDG roots");
    let legacy = directory
        .path()
        .join("cache/rufin/store/rufin-store.sqlite");
    let path = directory.path().join("data/rufin/store/rufin-store.sqlite");
    Database::relocate(&original, &legacy)
        .await
        .expect("read-only consistent original snapshot");
    Database::relocate(&legacy, &path)
        .await
        .expect("cache-to-data relocation");
    assert!(
        legacy.exists(),
        "legacy copy survives until the new Store opens"
    );
    let mut before = connection(&legacy, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut before)
            .await
            .unwrap(),
        43
    );
    let stable_queries = [
        (
            "SELECT json_array(source.object_id,activity.period,activity.item_kind,activity.track_object_id,activity.play_count,activity.skip_count,activity.last_played_at) FROM activity_baseline activity JOIN sources source USING(source_key) ORDER BY source.object_id,activity.period,activity.item_kind,activity.track_object_id",
            "SELECT json_array(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) FROM legacy_activity ORDER BY source_id,period,item_kind,track_object_id",
        ),
        (
            "SELECT object_id FROM sources ORDER BY object_id",
            "SELECT object_id FROM main.source_ids ORDER BY object_id",
        ),
        (
            "SELECT json_array(object_id,name) FROM playlists WHERE ownership='user' ORDER BY object_id",
            "SELECT json_array(object_id,name) FROM main.playlists WHERE name IS NOT NULL ORDER BY object_id",
        ),
        (
            "SELECT json_array(entry.object_id,playlist.object_id,entry.position) FROM playlist_entries entry JOIN playlists playlist USING(playlist_key) WHERE playlist.ownership='user' ORDER BY playlist.object_id,entry.position",
            "SELECT json_array(entry.object_id,playlist.object_id,entry.position) FROM main.playlist_entries entry JOIN main.playlists playlist USING(playlist_key) ORDER BY playlist.object_id,entry.position",
        ),
        (
            "SELECT json_array(listen_key,external_id,track_title,artist_name,album_title,started_at,duration_millis,listened_millis,skipped) FROM listens ORDER BY listen_key",
            "SELECT json_array(listen_key,external_id,track_title,artist_name,album_title,started_at,duration_millis,listened_millis,skipped) FROM listens ORDER BY listen_key",
        ),
        (
            "SELECT DISTINCT json_array(object_id,name) FROM smart_playlists WHERE object_id NOT LIKE 'builtin:%' ORDER BY object_id,name",
            "SELECT json_array(object_id,name) FROM smart_playlists WHERE object_id NOT LIKE 'builtin:%' ORDER BY object_id,name",
        ),
        (
            "SELECT json_array(path,origin,root,relative_path) FROM local_access_files ORDER BY path",
            "SELECT json_array(path,origin,root,relative_path) FROM local_locators ORDER BY path",
        ),
        (
            "SELECT json_array(service,account_id,attempts,next_attempt_at,last_error) FROM listen_outbox ORDER BY service,account_id,listen_key",
            "SELECT json_array(service,account_id,attempts,next_attempt_at,last_error) FROM listen_outbox ORDER BY service,account_id,listen_key",
        ),
    ];
    let mut expected_rows = Vec::new();
    for (query, _) in stable_queries {
        expected_rows.push(
            sqlx::query_scalar::<_, String>(query)
                .fetch_all(&mut before)
                .await
                .expect(query),
        );
    }
    let expected_queue = sqlx::query_scalar::<_, String>("SELECT json_array(queue.object_id,queue.position,queue.traversal_position,queue.provenance_kind,queue.provenance_context_id,queue.provenance_source_rank) FROM queue_occurrences queue JOIN sources source USING(source_key) WHERE source.object_id=?1 ORDER BY queue.position")
        .bind(selected.as_ref().map(SourceId::as_str)).fetch_all(&mut before).await.expect("selected Queue");
    let expected_user: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM tracks WHERE user_favorite IS NOT NULL OR user_rating IS NOT NULL)+(SELECT count(*) FROM albums WHERE user_favorite IS NOT NULL OR user_rating IS NOT NULL)+(SELECT count(*) FROM artists WHERE user_favorite IS NOT NULL OR user_rating IS NOT NULL)")
        .fetch_one(&mut before).await.expect("favorite/rating facts");
    before.close().await.unwrap();
    for _ in 0..3 {
        drop(
            Database::open_configured(&path, &configured, selected.as_ref())
                .await
                .expect("migrate and reopen real Store"),
        );
    }
    let mut after = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut after)
            .await
            .unwrap(),
        44
    );
    for ((_, query), expected) in stable_queries.into_iter().zip(expected_rows) {
        assert_eq!(
            sqlx::query_scalar::<_, String>(query)
                .fetch_all(&mut after)
                .await
                .expect(query),
            expected,
            "{query}"
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut after)
            .await
            .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_, String>("SELECT json_array(object_id,position,traversal_position,provenance_kind,provenance_context_id,provenance_source_rank) FROM queue_occurrences ORDER BY position").fetch_all(&mut after).await.unwrap(), expected_queue);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_media_state")
            .fetch_one(&mut after)
            .await
            .unwrap(),
        expected_user
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(&mut after)
            .await
            .unwrap(),
        "ok"
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut after)
            .await
            .unwrap()
            .is_empty()
    );
    tracing::info!(
        "Real Store migration preserved compared semantic families; selected Queue: {}",
        expected_queue.len()
    );
    let options = SqliteConnectOptions::new()
        .filename(&original)
        .read_only(true);
    let mut untouched = SqliteConnection::connect_with(&options)
        .await
        .expect("read-only original check");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut untouched)
            .await
            .unwrap(),
        43
    );
}

#[tokio::test]
async fn schema_43_fixture_migrates_every_core_durable_family_and_selected_queue() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let mut schema_43 = connection(&path, true).await;
    sqlx::raw_sql(SCHEMA_43)
        .execute(&mut schema_43)
        .await
        .expect("create exact schema-43 fixture");
    sqlx::raw_sql(
        r#"
        INSERT INTO sources VALUES
          (1,'source-alpha','Alpha','alpha',X'01',zeroblob(32),zeroblob(32),3),
          (2,'source-beta','Beta','beta',X'02',zeroblob(32),zeroblob(32),4);
        INSERT INTO albums(album_key,source_key,object_id,title,normalized_title,display_artist,sort_text,user_favorite,user_rating)
          VALUES (1,1,'album-a','Album A','album a','Artist A','album a',1,75),
                 (2,2,'album-b','Album B','album b','Artist B','album b',NULL,NULL);
        INSERT INTO artists(artist_key,source_key,object_id,name,normalized_name,sort_text,user_favorite,user_rating)
          VALUES (1,1,'artist-a','Artist A','artist a','artist a',1,80),
                 (2,2,'artist-b','Artist B','artist b','artist b',NULL,NULL);
        INSERT INTO tracks(
          track_key,source_key,object_id,album_key,title,normalized_search,
          display_album,display_artist,sort_text,duration_millis,disc_number,
          track_number,media_uri,user_favorite,user_rating
        ) VALUES
          (1,1,'track-a',1,'Track A','track a','Album A','Artist A','track a',100000,1,1,'subsonic:track-a',1,90),
          (2,2,'track-b',2,'Track B','track b','Album B','Artist B','track b',200000,1,2,'file:///music/b.flac',NULL,NULL);
        INSERT INTO track_artists VALUES (1,1,0),(2,2,0);
        INSERT INTO playlists VALUES
          (1,1,'source','native-a','Native A','native a','native a',NULL,0),
          (2,1,'user','local-a','Local A','local a','local a',NULL,1),
          (3,2,'source','native-b','Native B','native b','native b',NULL,0),
          (4,2,'user','local-b','Local B','local b','local b',NULL,1);
        INSERT INTO playlist_entries VALUES
          (1,1,'native-a:0',1,'track-a',0),
          (2,2,'local-a:0',1,'track-a',0),
          (3,3,'native-b:0',2,'track-b',0),
          (4,4,'local-b:0',2,'track-b',0);
        INSERT INTO smart_playlists VALUES
          (1,1,'builtin:most-played','Most Played','most played','{"match_all":[],"match_any":[],"sort_field":"Title","descending":false}',0),
          (2,2,'builtin:most-played','Most Played','most played','{"match_all":[],"match_any":[],"sort_field":"Title","descending":false}',0),
          (3,1,'custom-a','Custom A','custom a','{"match_all":[],"match_any":[],"sort_field":"Title","descending":false}',1),
          (4,2,'custom-b','Custom B','custom b','{"match_all":[],"match_any":[],"sort_field":"Title","descending":false}',1);
        INSERT INTO listens VALUES
          (1,'listen-a',1,1,'track-a','Track A','Artist A','Album A',100,'1970-01',100000,90000,0),
          (2,'listen-b',2,2,'track-b','Track B','Artist B','Album B',200,'1970-01',200000,10000,1),
          (3,'orphan-play',NULL,NULL,'navidrome:track:gone','Gone A','Lost Artist','Lost Album',300,'1970-01',180000,120000,0),
          (4,NULL,NULL,NULL,'subsonic:track:gone','Gone B','Lost Artist','Lost Album',400,'1970-01',210000,150000,1);
        INSERT INTO queue_state VALUES
          (1,1,NULL,111,'all',0),(2,2,NULL,222,'one',1);
        INSERT INTO queue_occurrences(
          queue_occurrence_key,source_key,object_id,position,traversal_position,
          provenance_kind,track_key,track_object_id,fallback_title,
          fallback_artist,fallback_album,fallback_duration_millis
        ) VALUES
          (1,1,'queue-a',0,0,'manual',1,'track-a','Track A','Artist A','Album A',100000),
          (2,2,'queue-b',0,0,'manual',2,'track-b','Track B','Artist B','Album B',200000);
        INSERT INTO local_access_files(
          local_access_file_key,source_key,track_object_id,origin,path,root,
          relative_path,size_bytes,mtime_ns,parser_version,title,normalized_title,
          album,normalized_album,artist,normalized_artist,disc_number,track_number,
          duration_millis,media_uri,loudness_analysis_key
        ) VALUES
          (1,2,'track-b','mapping','/music/b.flac','/music','b.flac',12,10,1,
           'Track B','track b','Album B','album b','Artist B','artist b',1,2,
           200000,'file:///music/b.flac',zeroblob(32));
        "#,
    )
    .execute(&mut schema_43)
    .await
    .expect("seed schema-43 durable families");
    schema_43.close().await.expect("close schema-43 fixture");

    let configured = [SourceId::new("source-alpha"), SourceId::new("source-beta")];
    let selected = SourceId::new("source-beta");
    let database = Database::open_configured(&path, &configured, Some(&selected))
        .await
        .expect("migrate schema 43 to 44");
    let history = database
        .activity_history(None, "", &ReadCancellation::new())
        .await
        .expect("read migrated orphan Activity");
    assert_eq!(
        history
            .iter()
            .take(2)
            .map(|media| media.title.as_str())
            .collect::<Vec<_>>(),
        ["Gone B", "Gone A"]
    );
    drop(database);
    for _ in 0..2 {
        drop(
            Database::open_configured(&path, &configured, Some(&selected))
                .await
                .expect("reopen migrated Store"),
        );
    }
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read migrated version"),
        44
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT object_id FROM main.source_ids ORDER BY source_key"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read SourceIds"),
        ["source-alpha", "source-beta"]
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT source_key,display_name,catalog_revision FROM sources ORDER BY source_key"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read source observations"),
        Vec::<(i64, String, i64)>::new()
    );
    let expected_user_media = [
        (
            library::source_entity_uri(&configured[0], "album", "album-a"),
            1,
            75,
        ),
        (
            library::source_entity_uri(&configured[0], "artist", "artist-a"),
            1,
            80,
        ),
        (
            library::source_entity_uri(&configured[0], "track", "track-a"),
            1,
            90,
        ),
    ];
    assert_eq!(
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT media_uri,favorite,rating FROM user_media_state ORDER BY media_uri"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read user media state"),
        expected_user_media
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT object_id,position FROM main.playlists ORDER BY position"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read global Playlist rank"),
        [
            ("native-b".to_string(), 0),
            ("local-b".to_string(), 1),
            ("native-a".to_string(), 2),
            ("local-a".to_string(), 3),
        ]
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT object_id,position FROM smart_playlists ORDER BY position"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read global Smart rank"),
        [
            ("builtin:most-played".to_string(), 0),
            ("custom-b".to_string(), 1),
            ("custom-a".to_string(), 3),
        ]
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, Option<String>, String)>(
            "SELECT external_id,source_id,media_uri FROM listens ORDER BY listen_key"
        )
        .fetch_all(&mut reader)
        .await
        .expect("read migrated listens"),
        [
            (
                Some("listen-a".to_string()),
                Some("source-alpha".to_string()),
                library::source_entity_uri(&configured[0], "track", "track-a"),
            ),
            (
                Some("listen-b".to_string()),
                Some("source-beta".to_string()),
                "file:///music/b.flac".to_string(),
            ),
            (
                Some("orphan-play".to_string()),
                None,
                library::source_entity_uri(
                    &SourceId::new("rufin:recovered"),
                    "track",
                    "orphan-play",
                ),
            ),
            (
                None,
                None,
                library::source_entity_uri(&SourceId::new("rufin:recovered"), "track", "4"),
            ),
        ]
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64, String, i64)>(
            "SELECT occurrence.object_id,state.progress_millis,state.repeat_mode,state.shuffled
             FROM queue_occurrences occurrence CROSS JOIN queue_state state"
        )
        .fetch_one(&mut reader)
        .await
        .expect("read selected global Queue"),
        ("queue-b".to_string(), 222, "one".to_string(), 1)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM local_access_files")
            .fetch_one(&mut reader)
            .await
            .expect("read Local observations"),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(&mut reader)
            .await
            .expect("check integrity"),
        "ok"
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_optional(&mut reader)
            .await
            .expect("check foreign keys")
            .is_none()
    );
}

#[tokio::test]
async fn schema_41_and_42_preserve_playlist_order_and_authored_entries() {
    for version in [41, 42] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.sqlite");
        let mut raw = connection(&path, true).await;
        sqlx::raw_sql(SCHEMA_43).execute(&mut raw).await.unwrap();
        sqlx::raw_sql(
            "DROP INDEX playlists_position_idx;
             ALTER TABLE playlists DROP COLUMN position;
             INSERT INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest)
               VALUES(1,'source-a','A','a',zeroblob(32),zeroblob(32)),
                     (2,'source-b','B','b',zeroblob(32),zeroblob(32));
             INSERT INTO playlists VALUES
               (1,1,'user','authored-a','Zulu','zulu','zulu',NULL),
               (2,1,'source','native-a','Alpha','alpha','alpha',NULL),
               (3,2,'user','authored-b','Same','same','same',NULL),
               (4,2,'source','native-b','Same','same','same',NULL);
             INSERT INTO playlist_entries VALUES
               (1,1,'entry-a',NULL,'missing-a',0),
               (2,3,'entry-b',NULL,'missing-b',0);",
        )
        .execute(&mut raw)
        .await
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version={version}"
        )))
        .execute(&mut raw)
        .await
        .unwrap();
        raw.close().await.unwrap();

        let configured = [SourceId::new("source-a"), SourceId::new("source-b")];
        let database = Database::open_configured(&path, &configured, Some(&configured[1]))
            .await
            .unwrap();
        let mut raw = connection(&path, false).await;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT object_id FROM main.playlists ORDER BY position"
            )
            .fetch_all(&mut raw)
            .await
            .unwrap(),
            ["authored-b", "native-b", "native-a", "authored-a"],
            "released schema {version} order"
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String)>(
                "SELECT object_id,media_uri FROM main.playlist_entries ORDER BY object_id"
            )
            .fetch_all(&mut raw)
            .await
            .unwrap(),
            [
                (
                    "entry-a".into(),
                    library::source_entity_uri(&configured[0], "track", "missing-a")
                ),
                (
                    "entry-b".into(),
                    library::source_entity_uri(&configured[1], "track", "missing-b")
                ),
            ]
        );
        raw.close().await.unwrap();
        database.close().await.unwrap();
    }
}

#[tokio::test]
async fn schema_43_malformed_listen_keeps_valid_siblings_and_preserves_input() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let mut schema_43 = connection(&path, true).await;
    sqlx::raw_sql(SCHEMA_43)
        .execute(&mut schema_43)
        .await
        .expect("create exact schema-43 fixture");
    sqlx::query("DROP INDEX queue_occurrences_page_idx")
        .execute(&mut schema_43)
        .await
        .expect("make the known migration fail");
    sqlx::raw_sql("PRAGMA ignore_check_constraints=ON; INSERT INTO listens VALUES(1,'good-before',NULL,NULL,'gone-one','Before','Artist','Album',100,'1970-01',1000,900,0),(2,'bad-duration',NULL,NULL,'gone-two','Invalid','Artist','Album',200,'1970-01',-1,900,0),(3,'good-after',NULL,NULL,'gone-three','After','Artist','Album',300,'1970-01',1000,900,0);").execute(&mut schema_43).await.unwrap();
    schema_43.close().await.expect("close schema-43 fixture");

    let database = Database::open_configured(&path, &[], None).await.unwrap();
    assert!(!database.fresh_start());
    database.close().await.unwrap();
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read rebuilt schema version"),
        44
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT external_id FROM listens ORDER BY listen_key")
            .fetch_all(&mut reader)
            .await
            .unwrap(),
        ["good-before", "good-after"]
    );
    reader.close().await.expect("close rebuilt Store");
    let preserved = std::fs::read_dir(directory.path())
        .expect("read Store directory")
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.contains(".unusable-") && !name.ends_with("-wal") && !name.ends_with("-shm")
        })
        .expect("preserve the failed migration input")
        .path();
    let mut preserved_reader = connection(&preserved, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut preserved_reader)
            .await
            .expect("read preserved schema version"),
        43
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens")
            .fetch_one(&mut preserved_reader)
            .await
            .unwrap(),
        3
    );
}

#[tokio::test]
async fn fresh_schema_has_exact_table_inventory() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    let _database = Database::open(&path).await.expect("open fresh Store");
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read final schema version"),
        44
    );
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema
         WHERE type='table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(&mut reader)
    .await
    .expect("read table inventory");
    assert_eq!(
        tables,
        [
            "favorite_outbox",
            "legacy_activity",
            "listen_outbox",
            "listens",
            "local_locators",
            "playlist_entries",
            "playlists",
            "queue_occurrences",
            "queue_state",
            "smart_playlists",
            "source_ids",
            "user_media_state"
        ]
    );
    let cache=sqlx::query_scalar::<_,String>("SELECT name FROM catalog.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut reader).await.unwrap();
    assert_eq!(
        cache,
        [
            "activity_baseline",
            "album_artists",
            "album_genres",
            "album_release_types",
            "albums",
            "artists",
            "folders",
            "genres",
            "home_entries",
            "local_access_metadata",
            "local_file_dependencies",
            "local_files",
            "loudness_measurements",
            "lyrics_cache",
            "moods",
            "native_playlist_entries",
            "native_playlists",
            "replay_gain_measurements",
            "sources",
            "track_artists",
            "track_folders",
            "track_genres",
            "track_moods",
            "tracks"
        ]
    );
}
#[tokio::test]
async fn older_source_scoped_playlist_ids_remain_distinct_global_playlists() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut old = connection(&path, false).await;
    sqlx::raw_sql("INSERT INTO local_playlists VALUES('second-source','user-list','Other User List');
                   INSERT INTO local_playlist_entries VALUES('second-source','user-list',0,'other-occurrence','other-track')")
        .execute(&mut old).await.unwrap();
    old.close().await.unwrap();
    let database = Database::open(&path).await.unwrap();
    assert!(!database.fresh_start());
    let mut reader = connection(&path, false).await;
    let rows = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT p.name,group_concat(e.object_id,','),p.source_key IS NULL
         FROM main.playlists p JOIN main.playlist_entries e USING(playlist_key)
         GROUP BY p.playlist_key ORDER BY p.position",
    )
    .fetch_all(&mut reader)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            (
                "User List".into(),
                "user-occurrence-one,user-occurrence-two".into(),
                1
            ),
            ("Other User List".into(), "other-occurrence".into(), 1)
        ]
    );
    database.close().await.unwrap();
}

#[tokio::test]
async fn schema_40_store_migrates_into_current_schema() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let database = Database::open_configured(
        &path,
        &[SourceId::new("legacy-source")],
        Some(&SourceId::new("legacy-source")),
    )
    .await
    .expect("migrate and open schema-40 Store");
    assert!(!database.fresh_start());
    let mut reader = connection(&path, false).await;
    let uri = library::source_entity_uri(&SourceId::new("legacy-source"), "track", "legacy-track");
    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT favorite,rating FROM user_media_state WHERE media_uri=?1"
        )
        .bind(&uri)
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        (1, 80)
    );
    assert_eq!(sqlx::query_scalar::<_,String>("SELECT entry.object_id FROM main.playlist_entries entry JOIN main.playlists playlist USING(playlist_key) WHERE playlist.object_id='user-list' ORDER BY entry.position").fetch_all(&mut reader).await.unwrap(),["user-occurrence-one","user-occurrence-two"]);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT object_id FROM smart_playlists")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        "smart-list"
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT favorite,attempts,next_attempt_at FROM favorite_outbox"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        (1, 2, 200)
    );
    assert_eq!(sqlx::query_as::<_,(String,i64,i64,Option<i64>)>("SELECT period,play_count,skip_count,last_played_at FROM legacy_activity ORDER BY period").fetch_all(&mut reader).await.unwrap(),[("2025-06".into(),3,0,None),("lifetime".into(),4,2,Some(90))]);
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT track_title,started_at FROM listens WHERE external_id='legacy-play'"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        ("Legacy Track".into(), 95)
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT service,attempts,next_attempt_at FROM listen_outbox"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        ("lastfm".into(), 0, 200)
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT media_uri,path,access_uri FROM local_locators WHERE origin='mapping'"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        (
            uri,
            "/music/track.flac".into(),
            "file:///music/track.flac".into()
        )
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT object_id FROM queue_occurrences ORDER BY traversal_position"
        )
        .fetch_all(&mut reader)
        .await
        .unwrap(),
        ["occ-auto", "occ-context"]
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, i64)>(
            "SELECT current_occurrence_id,progress_millis FROM queue_state"
        )
        .fetch_one(&mut reader)
        .await
        .unwrap(),
        (Some("occ-context".into()), 1200)
    );
    for table in [
        "tracks",
        "albums",
        "artists",
        "genres",
        "home_entries",
        "lyrics_cache",
        "loudness_measurements",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {table}"
            )))
            .fetch_one(&mut reader)
            .await
            .unwrap(),
            0,
            "catalog {table} is disposable"
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        "ok"
    );
}

#[tokio::test]
async fn schema_39_store_runs_the_committed_chain_into_current_schema() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut schema_39 = connection(&path, false).await;
    sqlx::raw_sql(
        "DROP TABLE user_ratings;
         UPDATE smart_playlists SET definition_json=
           '{\"match_all\":[{\"field\":\"Rating\",\"operator\":\"Above\",\"value\":{\"Number\":4}}],\"match_any\":[],\"sort_field\":\"Title\",\"descending\":false}';
         PRAGMA user_version=39;",
    )
    .execute(&mut schema_39)
    .await
    .expect("restore schema-39 facts");
    schema_39.close().await.expect("close schema-39 fixture");

    let _database = Database::open(&path)
        .await
        .expect("run the committed migration chain before opening the current Store");
    assert!(!_database.fresh_start());
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut reader)
            .await
            .expect("read current schema version"),
        44
    );
    let definition = sqlx::query_scalar::<_, String>(
        "SELECT definition_json FROM smart_playlists WHERE object_id='smart-list'",
    )
    .fetch_one(&mut reader)
    .await
    .expect("read migrated Smart Playlist");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&definition)
            .expect("parse migrated Smart Playlist")["match_all"][0]["value"]["Number"],
        8
    );
}

#[tokio::test]
async fn released_migration_ignores_unused_catalog_damage() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut legacy = connection(&path, false).await;
    sqlx::raw_sql(
        "DROP TABLE user_ratings;
         UPDATE tracks SET duration_seconds=-1 WHERE track_id='legacy-track';
         PRAGMA user_version=39;",
    )
    .execute(&mut legacy)
    .await
    .expect("make one known-schema value invalid for the current schema");
    legacy.close().await.expect("close known-schema fixture");

    let _database = Database::open(&path)
        .await
        .expect("migrate user facts without rebuilding obsolete catalog rows");

    let mut restored = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&mut restored)
            .await
            .expect("read repaired schema version"),
        44
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
            .fetch_one(&mut restored)
            .await
            .expect("read fresh catalog"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut restored)
            .await
            .expect("read skipped invalid Tracks family"),
        0
    );
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.unusable-")),
        "migration preserves its released input"
    );
}

#[tokio::test]
async fn released_migration_keeps_playlists_without_old_genre_catalog() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    create_legacy_store(&path).await;
    let mut legacy = connection(&path, false).await;
    sqlx::query("DROP TABLE genres")
        .execute(&mut legacy)
        .await
        .expect("remove one legacy family");
    legacy.close().await.expect("close partial Store");

    let database = Database::open(&path)
        .await
        .expect("migrate known user facts without unused old catalog tables");
    let mut reader = connection(&path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
            .fetch_one(&mut reader)
            .await
            .expect("read fresh catalog"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM main.playlist_entries")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut reader)
            .await
            .unwrap(),
        0
    );
    drop(database);
}

#[tokio::test]
async fn not_a_database_is_preserved_and_rebuilt_automatically() {
    let directory = tempfile::tempdir().expect("temporary Store directory");
    let path = directory.path().join("library.sqlite3");
    std::fs::write(&path, b"not a sqlite database").expect("write invalid Store");

    let _database = Database::open(&path)
        .await
        .expect("automatically preserve and rebuild invalid Store content");
    assert!(
        std::fs::read_dir(directory.path())
            .expect("read Store directory")
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.unusable-"))
    );
}

async fn create_legacy_store(path: &Path) {
    let mut connection = connection(path, true).await;
    sqlx::raw_sql(
        "PRAGMA application_id=1381320270;
         PRAGMA user_version=40;
         CREATE TABLE source_libraries(
             library_id INTEGER PRIMARY KEY, source_id TEXT NOT NULL,
             input_digest BLOB NOT NULL, content_digest BLOB,
             freshness_marker BLOB, home_digest BLOB, home_json TEXT, accepted_at INTEGER
         ) STRICT;
         CREATE TABLE albums(
             library_id INTEGER NOT NULL, album_id TEXT NOT NULL,
             title TEXT NOT NULL, display_artist TEXT NOT NULL,
             year INTEGER, release_date TEXT, date_added TEXT,
             musicbrainz_release_id TEXT, musicbrainz_release_group_id TEXT,
             image_item_id TEXT, image_tag TEXT,
             local_artwork_kind TEXT, local_artwork_path TEXT,
             local_artwork_picture_index INTEGER, local_artwork_revision TEXT,
             favorite INTEGER NOT NULL, user_rating INTEGER,
             release_types_json TEXT NOT NULL, relations_json TEXT NOT NULL,
             is_compilation INTEGER
         ) STRICT;
         CREATE TABLE tracks(
             library_id INTEGER NOT NULL, track_id TEXT NOT NULL,
             album_id TEXT, title TEXT NOT NULL, display_album TEXT NOT NULL,
             display_artist TEXT NOT NULL, duration_seconds INTEGER NOT NULL,
             disc_number INTEGER NOT NULL, track_number INTEGER NOT NULL,
             year INTEGER, release_date TEXT, date_added TEXT, source_path TEXT,
             source_format TEXT, comment TEXT, bpm INTEGER,
             musicbrainz_recording_id TEXT, musicbrainz_release_track_id TEXT,
             cue_path TEXT, cue_start_millis INTEGER, cue_end_millis INTEGER,
             image_item_id TEXT, image_tag TEXT,
             local_artwork_kind TEXT, local_artwork_path TEXT,
             local_artwork_picture_index INTEGER, local_artwork_revision TEXT,
             favorite INTEGER NOT NULL,
             user_rating INTEGER, relations_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE artists(
             library_id INTEGER NOT NULL, artist_id TEXT NOT NULL, name TEXT NOT NULL,
             musicbrainz_artist_id TEXT,
             image_item_id TEXT, image_tag TEXT,
             local_artwork_kind TEXT, local_artwork_path TEXT,
             local_artwork_picture_index INTEGER, local_artwork_revision TEXT,
             favorite INTEGER NOT NULL,
             user_rating INTEGER
         ) STRICT;
         CREATE TABLE genres(
             library_id INTEGER NOT NULL, genre_id TEXT NOT NULL, name TEXT NOT NULL,
             image_item_id TEXT, image_tag TEXT
         ) STRICT;
         CREATE TABLE music_folders(
             library_id INTEGER NOT NULL, folder_id TEXT NOT NULL, name TEXT NOT NULL,
             image_item_id TEXT, image_tag TEXT
         ) STRICT;
         CREATE TABLE source_playlists(
             library_id INTEGER NOT NULL, playlist_id TEXT NOT NULL,
             name TEXT NOT NULL, image_item_id TEXT, image_tag TEXT
         ) STRICT;
         CREATE TABLE source_playlist_entries(
             library_id INTEGER NOT NULL, playlist_id TEXT NOT NULL,
             position INTEGER NOT NULL, occurrence_id TEXT NOT NULL,
             track_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE local_favorites(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL, item_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE user_ratings(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL,
             item_id TEXT NOT NULL, rating INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE pending_favorites(
             source_id TEXT NOT NULL, item_kind TEXT NOT NULL,
             item_id TEXT NOT NULL, favorite INTEGER NOT NULL,
             previous_favorite INTEGER NOT NULL, attempts INTEGER NOT NULL,
             next_attempt_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE local_playlists(
             source_id TEXT NOT NULL, playlist_id TEXT NOT NULL, name TEXT NOT NULL
         ) STRICT;
         CREATE TABLE local_playlist_entries(
             source_id TEXT NOT NULL, playlist_id TEXT NOT NULL,
             position INTEGER NOT NULL, occurrence_id TEXT NOT NULL,
             track_id TEXT NOT NULL
         ) STRICT;
         CREATE TABLE smart_playlists(
             source_id TEXT NOT NULL, smart_playlist_id TEXT NOT NULL,
             name TEXT NOT NULL, builtin_key TEXT, definition_json TEXT NOT NULL,
             position INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE local_imports(
             source_id TEXT NOT NULL, track_id TEXT NOT NULL,
             first_seen_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE playback_queues(
             source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
             rows_json TEXT NOT NULL, traversal_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE playback_state(
             source_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
             selected_occurrence_id TEXT, progress_millis INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE local_files(
             library_id INTEGER NOT NULL, path TEXT NOT NULL, root TEXT NOT NULL,
             relative_path TEXT NOT NULL, kind TEXT NOT NULL, size_bytes INTEGER,
             mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
             parse_version INTEGER, state TEXT NOT NULL, dependencies_json TEXT NOT NULL
         ) STRICT;
         CREATE TABLE local_access_files(
             source_id TEXT NOT NULL, path TEXT NOT NULL, root TEXT NOT NULL,
             relative_path TEXT NOT NULL, size_bytes INTEGER NOT NULL,
             mtime_ns INTEGER NOT NULL, device_id INTEGER, inode INTEGER,
             parser_version INTEGER NOT NULL, title TEXT NOT NULL,
             album TEXT NOT NULL, artist TEXT NOT NULL, disc_number INTEGER NOT NULL,
             track_number INTEGER NOT NULL, duration_seconds INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE pending_scrobbles(
             service TEXT NOT NULL, account_id TEXT NOT NULL, play_id TEXT NOT NULL,
             track_title TEXT NOT NULL, artist_name TEXT NOT NULL, album_title TEXT,
             duration_millis INTEGER NOT NULL, started_at INTEGER NOT NULL,
             attempts INTEGER NOT NULL, next_attempt_at INTEGER, last_error TEXT
         ) STRICT;
         CREATE TABLE lyrics_cache(
             source_id TEXT NOT NULL, track_id TEXT NOT NULL, role TEXT NOT NULL,
             language TEXT NOT NULL, script TEXT NOT NULL, origin TEXT NOT NULL,
             input_version INTEGER NOT NULL, input_digest BLOB NOT NULL,
             payload TEXT NOT NULL, cached_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE loudness_measurements(
             source_id TEXT NOT NULL, scope TEXT NOT NULL, item_id TEXT NOT NULL,
             analysis_key BLOB NOT NULL, integrated_lufs REAL, true_peak REAL
         ) STRICT;
         CREATE TABLE listening_aggregates(
             source_id TEXT NOT NULL, item_id TEXT NOT NULL, period TEXT NOT NULL,
             item_kind TEXT NOT NULL, play_count INTEGER NOT NULL,
             skip_count INTEGER NOT NULL, last_played_at INTEGER
         ) STRICT;
         CREATE TABLE recent_plays(
             play_id TEXT NOT NULL, source_id TEXT NOT NULL, track_id TEXT NOT NULL,
             track_title TEXT NOT NULL, artist_name TEXT NOT NULL,
             album_title TEXT, played_at INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE album_release_info(
             source_id TEXT NOT NULL,album_id TEXT NOT NULL,
             exact_identity_key TEXT NOT NULL,lookup_state TEXT NOT NULL,
             release_types_json TEXT,is_compilation INTEGER
         ) STRICT;
         INSERT INTO source_libraries VALUES(
             1, 'legacy-source', zeroblob(32), zeroblob(32), X'01', zeroblob(32),
             '{\"kind\":\"Source\",\"sections\":[{\"kind\":\"MostPlayed\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]},{\"kind\":\"NewlyAdded\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]},{\"kind\":\"RecentlyPlayed\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]},{\"kind\":\"RecentlyReleased\",\"items\":[{\"kind\":\"track\",\"id\":\"legacy-track\"}]}]}', 1
         );
         INSERT INTO albums VALUES(
             1, 'legacy-album', 'Legacy Album', 'Legacy Artist',
             2024, '2024-01-01', '2024-01-02', 'release-id', 'release-group-id',
             NULL, NULL, 'file', '/music/cover.jpg', NULL, 'album-rev',
             0, NULL, '[\"Album\"]',
             '{\"album_artists\":[{\"id\":\"legacy-artist\",\"name\":\"Legacy Artist\"},{\"id\":\"album-credit-only\",\"name\":\"Album Credit\"}],\"genres\":[{\"id\":\"legacy-genre\",\"name\":\"Legacy Genre\"},{\"id\":\"album-genre-only\",\"name\":\"Album Genre\"}]}', 1
         );
         INSERT INTO tracks VALUES(
             1, 'legacy-track', 'legacy-album', 'Legacy Track',
             'Legacy Album', 'Legacy Artist', 180, 1, 1, 2024,
             '2024-01-01', '2024-01-02', '/music/track.flac', 'FLAC',
             'Legacy comment', 120, 'recording-id', 'release-track-id',
             '/music/album.cue', 1000, 181000, NULL, NULL,
             'embedded', '/music/track.flac', 2, 'track-rev', 0, NULL,
             '{\"artists\":[{\"id\":\"legacy-artist\",\"name\":\"Legacy Artist\"},{\"id\":\"track-credit-only\",\"name\":\"Track Credit\"}],\"genres\":[{\"id\":\"legacy-genre\",\"name\":\"Legacy Genre\"},{\"id\":\"track-genre-only\",\"name\":\"Track Genre\"}],\"moods\":[{\"id\":\"legacy-mood\",\"name\":\"Legacy Mood\"}],\"music_folders\":[\"legacy-folder\"]}'
         );
         INSERT INTO artists VALUES(
             1, 'legacy-artist', 'Legacy Artist', 'artist-id', NULL, NULL,
             'file', '/music/artist.jpg', NULL, 'artist-rev', 0, NULL
         );
         INSERT INTO genres VALUES(
             1, 'legacy-genre', 'Legacy Genre', 'genre-image', 'genre-tag'
         );
         INSERT INTO music_folders VALUES(
             1, 'legacy-folder', 'Legacy Folder', NULL, NULL
         );
         INSERT INTO source_playlists VALUES(
             1, 'source-list', 'Source List', NULL, NULL
         );
         INSERT INTO source_playlist_entries VALUES(
             1, 'source-list', 0, 'source-occurrence', 'legacy-track'
         );
         INSERT INTO local_favorites VALUES(
             'legacy-source', 'track', 'legacy-track'
         );
         INSERT INTO user_ratings VALUES(
             'legacy-source', 'track', 'legacy-track', 8
         );
         INSERT INTO pending_favorites VALUES(
             'legacy-source', 'track', 'legacy-track', 1, 0, 2, 200
         );
         INSERT INTO local_playlists VALUES(
             'legacy-source', 'user-list', 'User List'
         );
         INSERT INTO local_playlist_entries VALUES(
             'legacy-source', 'user-list', 0, 'user-occurrence-one', 'legacy-track'
         );
         INSERT INTO local_playlist_entries VALUES(
             'legacy-source', 'user-list', 1, 'user-occurrence-two', 'legacy-track'
         );
         INSERT INTO smart_playlists VALUES(
             'legacy-source', 'smart-list', 'Smart List', NULL,
             '{\"match_all\":[],\"match_any\":[],\"sort_field\":\"Title\",\"descending\":false}', 0
         );
         INSERT INTO local_imports VALUES(
             'legacy-source', 'legacy-track', 50
         );
         INSERT INTO playback_queues VALUES(
             'legacy-source', 1,
             '{\"occurrences\":[{\"id\":\"occ-context\",\"track_id\":\"legacy-track\",\"provenance\":{\"Context\":{\"context_id\":\"route-album\",\"source_rank\":7}}},{\"id\":\"occ-auto\",\"track_id\":\"legacy-track\",\"provenance\":\"AutoDj\"}],\"fallback_tracks\":[{\"id\":\"legacy-track\",\"title\":\"Legacy Track\",\"artist\":\"Legacy Artist\",\"album\":\"Legacy Album\",\"album_id\":\"legacy-album\",\"primary_artist_id\":\"legacy-artist\",\"local_artwork\":\"/music/local-cover.jpg\",\"year\":2024,\"duration_seconds\":180,\"favorite\":true,\"track_number\":1,\"disc_number\":1,\"source_format\":\"FLAC\",\"source_path\":\"/music/track.flac\",\"musicbrainz_recording_id\":\"recording-id\",\"cue\":{\"cue_path\":\"/music/album.cue\",\"start_millis\":1000,\"end_millis\":181000}}]}',
             '[\"occ-auto\",\"occ-context\"]'
         );
         INSERT INTO playback_state VALUES('legacy-source', 1, 'occ-context', 1200);
         INSERT INTO local_files VALUES(
             1, '/music/track.flac', '/music', 'track.flac', 'media', 100,
             10, 1, 2, 1, 'accepted', '[\"/music/image.jpg\"]'
         );
         INSERT INTO local_access_files VALUES(
             'legacy-source', '/music/track.flac', '/music', 'track.flac',
             100, 10, 1, 2, 1, 'Legacy Track', 'Legacy Album',
             'Legacy Artist', 1, 1, 180
         );
         INSERT INTO pending_scrobbles VALUES(
             'lastfm', 'account', 'pending-play', 'Pending Track',
             'Pending Artist', NULL, 180000, 100, 0, 200, NULL
         );
         INSERT INTO lyrics_cache VALUES(
             'legacy-source', 'legacy-track', 'Lyrics', 'en', 'Latn',
             'source', 1, zeroblob(32), 'Legacy lyrics', 100
         );
         INSERT INTO loudness_measurements VALUES(
             'legacy-source','track','legacy-track',zeroblob(32),-14.0,NULL
         );
         INSERT INTO listening_aggregates VALUES(
             'legacy-source','legacy-track','lifetime','track',4,2,90
         );
         INSERT INTO listening_aggregates VALUES(
             'legacy-source','legacy-track','2025-06','track',3,0,NULL
         );
         INSERT INTO recent_plays VALUES(
             'legacy-play','legacy-source','legacy-track','Legacy Track',
             'Legacy Artist','Legacy Album',95
         );
         INSERT INTO album_release_info VALUES(
             'legacy-source','legacy-album','release-group:release-group-id',
             'found','[\"Remix\"]',NULL
         );",
    )
    .execute(&mut connection)
    .await
    .expect("create legacy Store fixture");
    connection.close().await.expect("close legacy fixture");
}

#[tokio::test]
async fn deleting_or_corrupting_catalog_preserves_exact_durable_records_and_locators() {
    let fixture = super::support::fixture().await;
    let path = fixture.path.clone();
    let catalog = path.with_extension("catalog.sqlite");
    let uri = &fixture.track_uris[0];
    fixture
        .database
        .create_playlist(None, "Durable duplicate list", &[uri.clone(), uri.clone()])
        .await
        .unwrap();
    fixture
        .database
        .import_user_media_state_jsonl(std::io::Cursor::new(format!(
            "{{\"version\":1}}\n{{\"media_uri\":{},\"favorite\":true,\"rating\":87}}\n",
            serde_json::to_string(uri).unwrap()
        )))
        .await
        .unwrap();
    fixture.database.import_local_locators_jsonl(std::io::Cursor::new(format!("{{\"version\":1}}\n{{\"source_id\":\"source\",\"media_uri\":{},\"origin\":\"mapping\",\"path\":\"/music/original.flac\",\"root\":\"/music\",\"relative_path\":\"original.flac\",\"access_uri\":\"file:///music/original.flac\"}}\n",serde_json::to_string(uri).unwrap()))).await.unwrap();
    let mut raw = connection(&path, false).await;
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(NULL,'source',?1,'Snapshot','Artist','Album',100,'1970-01',100000,90000,0)").bind(uri).execute(&mut raw).await.unwrap();
    sqlx::raw_sql("INSERT INTO listen_outbox(listen_key,service,account_id,attempts,next_attempt_at,last_error) SELECT listen_key,'lastfm','account',3,200,'retry' FROM listens; INSERT INTO legacy_activity VALUES('source','lifetime','track','track-a',4,2,90);").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    fixture.database.close().await.unwrap();
    let before = durable_rows(&path).await;
    for damage in 0..3 {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = catalog.as_os_str().to_os_string();
            file.push(suffix);
            match std::fs::remove_file(file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }
        }
        match damage {
            1 => std::fs::write(&catalog, b"damaged catalog").unwrap(),
            2 => {
                let mut old = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&catalog)
                        .create_if_missing(true),
                )
                .await
                .unwrap();
                sqlx::raw_sql("PRAGMA user_version=1; CREATE TABLE obsolete(value TEXT)")
                    .execute(&mut old)
                    .await
                    .unwrap();
                old.close().await.unwrap();
            }
            _ => {}
        }
        let database = Database::open(&path).await.unwrap();
        assert!(
            !database.fresh_start(),
            "cache damage never recovers/replaces durable Store"
        );
        assert_eq!(durable_rows(&path).await, before);
        let mut raw = connection(&path, false).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT access_uri FROM local_access_files")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "file:///music/original.flac"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "ok"
        );
        raw.close().await.unwrap();
        // Ordinary rescan rejoins effective favorites/ratings by the unchanged media URI.
        let (_, _, object) = library::source_entity_parts(uri).unwrap();
        let mut scan = library::Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        scan.write_track(
            &object,
            None,
            "Changed tags",
            "changed tags",
            "",
            "Artist",
            "changed tags",
            100000,
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(999),
            None,
            None,
            None,
            None,
            None,
            [0; 32],
        )
        .await
        .unwrap();
        scan.finish().await.unwrap();
        let mut raw = connection(&path, false).await;
        assert_eq!(sqlx::query_as::<_,(bool,i64)>("SELECT state.favorite,state.rating FROM tracks track JOIN user_media_state state USING(media_uri) WHERE track.media_uri=?1").bind(uri).fetch_one(&mut raw).await.unwrap(),(true,87));
        raw.close().await.unwrap();
        assert_eq!(durable_rows(&path).await, before);
        database.close().await.unwrap();
    }
}

async fn durable_rows(path: &Path) -> Vec<(String, Vec<String>)> {
    let mut raw = connection(path, false).await;
    let tables=sqlx::query_scalar::<_,String>("SELECT name FROM main.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut raw).await.unwrap();
    let mut result = Vec::new();
    for table in tables {
        let columns = sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?1)")
            .bind(&table)
            .fetch_all(&mut raw)
            .await
            .unwrap();
        let values = columns
            .iter()
            .map(|column| format!("\"{}\"", column.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("SELECT json_array({values}) FROM main.\"{table}\" ORDER BY {values}");
        let rows = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query))
            .fetch_all(&mut raw)
            .await
            .unwrap();
        result.push((table, rows));
    }
    raw.close().await.unwrap();
    result
}

#[tokio::test]
async fn forgetting_source_uses_durable_identity_with_no_cached_source() {
    let fixture = super::support::fixture().await;
    let database = &fixture.database;
    database
        .create_playlist(None, "Snapshot", &[fixture.track_uris[0].clone()])
        .await
        .unwrap();
    let mut raw = connection(&fixture.path, false).await;
    for kind in ["track", "album", "artist"] {
        let uri = library::source_entity_uri(&SourceId::new("source"), kind, "owned");
        sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating) VALUES(?1,1,87)")
            .bind(uri)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM catalog.sources")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    assert_eq!(
        database
            .source_identity_key(&SourceId::new("source"))
            .await
            .unwrap(),
        None
    );
    assert!(
        database
            .remove_source(&library::SourceId::new("source"))
            .await
            .unwrap()
    );
    let mut raw = connection(&fixture.path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_media_state")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM main.playlist_entries")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cache_and_durable_keys_are_independent_and_a_fresh_store_keeps_catalog() {
    let fixture = super::support::fixture().await;
    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql("PRAGMA foreign_keys=OFF;
      UPDATE main.source_ids SET source_key=88 WHERE object_id='source';
      INSERT INTO main.playlists(playlist_key,source_key,object_id,position) VALUES(9,88,'native',0);
      INSERT INTO catalog.native_playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text) VALUES(61,1,'native','Native','native','native');
      INSERT INTO catalog.native_playlist_entries(playlist_key,object_id,media_uri,title,position) VALUES(61,'occurrence','https://example.test/song','Song',0);
      INSERT INTO main.local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(88,'https://example.test/song','mapping','/music/song.flac','/music','song.flac','file:///music/song.flac');
      INSERT INTO catalog.local_access_metadata VALUES('file:///music/song.flac',9,1,NULL,NULL,1,'Song','song','Album','album','Artist','artist',1,1,1000,NULL);")
      .execute(&mut raw).await.unwrap();
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT playlist_key,source_key,name FROM playlists WHERE object_id='native'"
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        (-61, 1, "Native".into())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playlist_entries WHERE playlist_key=-61"
        )
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, String)>("SELECT source_key,title FROM local_access_files")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        (1, "Song".into())
    );
    raw.close().await.unwrap();
    let catalog = fixture.path.with_extension("catalog.sqlite");
    fixture.database.close().await.unwrap();
    std::fs::write(&fixture.path, b"unusable user store").unwrap();
    let database = Database::open(&fixture.path).await.unwrap();
    assert!(database.fresh_start());
    let mut raw = connection(&fixture.path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM playlists WHERE object_id='native'")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        "Native"
    );
    assert!(catalog.exists());
    raw.close().await.unwrap();
    database.close().await.unwrap();
}

#[tokio::test]
async fn unused_fields_and_version_stamp_do_not_replace_usable_state() {
    let fixture = super::support::fixture().await;
    fixture
        .database
        .create_playlist(None, "Keep me", &[])
        .await
        .unwrap();
    fixture.database.close().await.unwrap();
    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql(
        "ALTER TABLE main.playlists ADD COLUMN irrelevant TEXT;
      CREATE TABLE extra(value TEXT); PRAGMA user_version=999;",
    )
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();
    let database = Database::open(&fixture.path).await.unwrap();
    assert!(!database.fresh_start());
    let mut raw = connection(&fixture.path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM main.playlists WHERE name IS NOT NULL")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        "Keep me"
    );
    raw.close().await.unwrap();
    database.close().await.unwrap();
}

#[tokio::test]
async fn missing_required_index_column_opens_fresh_without_catalog_rescan() {
    let fixture = super::support::fixture().await;
    fixture.database.close().await.unwrap();
    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql("DROP VIEW temp.playlist_entries; DROP VIEW temp.playlists; DROP VIEW temp.local_access_files; DROP VIEW temp.activity_baseline; DROP INDEX listens_media_idx; ALTER TABLE listens DROP COLUMN media_uri;").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let database = Database::open(&fixture.path).await.unwrap();
    assert!(database.fresh_start());
    let mut raw = connection(&fixture.path, false).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        4
    );
    raw.close().await.unwrap();
    database.close().await.unwrap();
}

#[tokio::test]
async fn legacy_local_cue_queue_uses_the_same_identity_as_favorites() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    create_legacy_store(&path).await;
    let mut raw = connection(&path, false).await;
    sqlx::raw_sql(
        "UPDATE source_libraries SET source_id='local:server:library';
         UPDATE local_favorites SET source_id='local:server:library';
         UPDATE playback_queues SET source_id='local:server:library';
         UPDATE playback_state SET source_id='local:server:library';",
    )
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();
    let source = SourceId::new("local:server:library");
    let database = Database::open_configured(&path, std::slice::from_ref(&source), Some(&source))
        .await
        .unwrap();
    let mut raw = connection(&path, false).await;
    let uri = library::cue_media_uri("legacy-track", "file:///music/track.flac", 1000, 181000);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT media_uri FROM queue_occurrences ORDER BY position"
        )
        .fetch_all(&mut raw)
        .await
        .unwrap(),
        [uri.clone(), uri.clone()]
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT favorite FROM user_media_state WHERE media_uri=?1")
            .bind(&uri)
            .fetch_one(&mut raw)
            .await
            .unwrap()
    );
    raw.close().await.unwrap();
    database.close().await.unwrap();
}

#[tokio::test]
async fn released_cue_segments_keep_distinct_user_facts_and_backing_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    let mut raw = connection(&path, true).await;
    sqlx::raw_sql(SCHEMA_43).execute(&mut raw).await.unwrap();
    sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,catalog_digest,artwork_digest) VALUES(1,'local:server:library','Local','local',zeroblob(32),zeroblob(32));
      INSERT INTO tracks(source_key,object_id,title,normalized_search,display_album,display_artist,sort_text,duration_millis,media_uri,source_path,cue_path,cue_start_millis,cue_end_millis,user_favorite)
      VALUES(1,'file:///music/disc.cue#1','First','first','Album','Artist','first',1000,'file:///music/disc.flac','/music/disc.flac','/music/disc.cue',0,1000,1),
            (1,'file:///music/disc.cue#2','Second','second','Album','Artist','second',1000,'file:///music/disc.flac','/music/disc.flac','/music/disc.cue',1000,2000,1);")
      .execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let database = Database::open(&path).await.unwrap();
    let mut raw = connection(&path, false).await;
    let uris = sqlx::query_scalar::<_, String>(
        "SELECT media_uri FROM user_media_state ORDER BY media_uri",
    )
    .fetch_all(&mut raw)
    .await
    .unwrap();
    assert_eq!(uris.len(), 2);
    let parts = uris
        .iter()
        .map(|uri| library::cue_media_parts(uri).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(parts[0].1, "file:///music/disc.flac");
    assert_eq!(parts[0].0, "file:///music/disc.cue#1");
    assert_eq!(parts[1].0, "file:///music/disc.cue#2");
    assert_eq!((parts[0].2, parts[0].3), (0, 1000));
    assert_eq!((parts[1].2, parts[1].3), (1000, 2000));
    raw.close().await.unwrap();
    database.close().await.unwrap();
}
