-- The catalog file as Rufin 0.16.6 created it, with a small library.
PRAGMA application_id = 1381320270;

PRAGMA user_version = 51;

CREATE TABLE IF NOT EXISTS sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    display_name TEXT NOT NULL, normalized_name TEXT NOT NULL, freshness BLOB,
    artwork_digest BLOB NOT NULL CHECK (length(artwork_digest) = 32),
    distinct_track_covers INTEGER NOT NULL DEFAULT 0 CHECK (distinct_track_covers IN (0,1)),
    catalog_revision INTEGER NOT NULL DEFAULT 0 CHECK (catalog_revision >= 0),
    catalog_complete INTEGER NOT NULL DEFAULT 0 CHECK (catalog_complete IN (0,1))
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
CREATE INDEX IF NOT EXISTS albums_source_favorites_idx ON albums(source_key, object_id) WHERE source_favorite=1;

CREATE INDEX IF NOT EXISTS albums_key_idx ON albums(source_key, album_key);
CREATE INDEX IF NOT EXISTS albums_shuffle_idx ON albums(source_key,((album_key*1103515245)%2147483647),album_key);

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
CREATE INDEX IF NOT EXISTS tracks_source_favorites_idx ON tracks(source_key, object_id) WHERE source_favorite=1;

CREATE INDEX IF NOT EXISTS tracks_key_idx ON tracks(source_key, track_key);
CREATE INDEX IF NOT EXISTS tracks_source_path_idx ON tracks(source_key, source_path, album_key);

CREATE INDEX IF NOT EXISTS tracks_source_cue_idx ON tracks(source_key, cue_path, object_id) WHERE cue_path IS NOT NULL;

CREATE INDEX IF NOT EXISTS tracks_artwork_idx ON tracks(source_key, artwork_binding)
    WHERE artwork_binding IS NOT NULL;

CREATE INDEX IF NOT EXISTS tracks_album_idx ON tracks(source_key, album_key, disc_number, track_number, track_key);
CREATE INDEX IF NOT EXISTS tracks_album_source_order_idx ON tracks(source_key,album_key,coalesce(disc_number,-1),coalesce(track_number,-1),sort_text,track_key);

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
CREATE INDEX IF NOT EXISTS artists_source_favorites_idx ON artists(source_key, object_id) WHERE source_favorite=1;

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
    writable INTEGER NOT NULL DEFAULT 1,
    metadata_writable INTEGER NOT NULL DEFAULT 1,
    provider_revision TEXT,
    valid_until INTEGER,
    UNIQUE (source_key, object_id)
) STRICT;

CREATE INDEX IF NOT EXISTS native_playlists_order_idx ON native_playlists(source_key, sort_text, playlist_key);

CREATE INDEX IF NOT EXISTS native_playlists_title_idx ON native_playlists(source_key, sort_text, playlist_key);

CREATE INDEX IF NOT EXISTS native_playlists_public_key_idx ON native_playlists(-playlist_key);

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
CREATE INDEX IF NOT EXISTS native_playlist_entries_public_order_idx ON native_playlist_entries(-playlist_key,position,-playlist_entry_key);
CREATE INDEX IF NOT EXISTS native_playlist_entries_public_key_idx ON native_playlist_entries(-playlist_entry_key);
CREATE INDEX IF NOT EXISTS native_playlist_entries_shuffle_idx ON native_playlist_entries(playlist_key,((playlist_entry_key*1103515245)%2147483647),playlist_entry_key);

CREATE INDEX IF NOT EXISTS native_playlist_entries_media_idx ON native_playlist_entries(media_uri,(title IS NULL),snapshot_at DESC,playlist_entry_key ASC);

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

INSERT INTO sources(source_key,object_id,display_name,normalized_name,freshness,artwork_digest,catalog_revision,catalog_complete) VALUES(10,'local-source','Music','music',NULL,X'0000000000000000000000000000000000000000000000000000000000000000',3,1);
INSERT INTO sources(source_key,object_id,display_name,normalized_name,freshness,artwork_digest,catalog_revision,catalog_complete) VALUES(11,'server','Server','server',X'6672657368',X'0000000000000000000000000000000000000000000000000000000000000000',1,1);
INSERT INTO artists(artist_key,source_key,object_id,media_uri,name,normalized_name,sort_text,musicbrainz_artist_id,artwork_binding,source_favorite,source_rating) VALUES(1,10,'artist-a','rufin:source/artist/local-source/artist-a','Artist A','artist a','artist a','mb-artist-a',NULL,0,NULL);
INSERT INTO artists(artist_key,source_key,object_id,media_uri,name,normalized_name,sort_text,musicbrainz_artist_id,artwork_binding,source_favorite,source_rating) VALUES(2,11,'ar2','rufin:source/artist/server/ar2','Server Artist','server artist','server artist',NULL,NULL,1,NULL);
INSERT INTO genres(genre_key,source_key,object_id,name,normalized_name,sort_text,artwork_binding) VALUES(1,10,'rock','Rock','rock','rock',NULL);
INSERT INTO genres(genre_key,source_key,object_id,name,normalized_name,sort_text,artwork_binding) VALUES(2,11,'jazz','Jazz','jazz','jazz',NULL);
INSERT INTO moods(mood_key,source_key,object_id,name,normalized_name,sort_text) VALUES(1,10,'calm','Calm','calm','calm');
INSERT INTO folders(folder_key,source_key,object_id,name,normalized_name,sort_text,artwork_binding) VALUES(1,10,'/music/Album','Album','album','album',NULL);
INSERT INTO albums(album_key,source_key,object_id,media_uri,title,normalized_title,display_artist,sort_text,year,release_date,date_added,musicbrainz_release_id,musicbrainz_release_group_id,is_compilation,release_lookup_identity,artwork_binding,source_favorite,source_rating,first_seen_at) VALUES(1,10,'album-1','rufin:source/album/local-source/album-1','Album','album','Artist A','album',2001,'2001-05-01','2020-01-02T03:04:05Z','mb-release','mb-group',0,'release-group:mb-group',NULL,0,NULL,1577934245);
INSERT INTO albums(album_key,source_key,object_id,media_uri,title,normalized_title,display_artist,sort_text,year,release_date,date_added,musicbrainz_release_id,musicbrainz_release_group_id,is_compilation,release_lookup_identity,artwork_binding,source_favorite,source_rating,first_seen_at) VALUES(2,11,'a2','rufin:source/album/server/a2','Server Album','server album','Server Artist','server album',2010,NULL,'2021-06-07T00:00:00Z',NULL,NULL,NULL,NULL,X'7B22736F757263655F6964223A22736572766572222C22696D616765223A7B226974656D5F6964223A22636F7665722D6132222C22746167223A2274227D7D',1,NULL,1623024000);
INSERT INTO tracks(normalized_search,comment,bpm,musicbrainz_recording_id,musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis,artwork_binding,source_favorite,source_rating,local_play_count,source_format,year,release_date,date_added,source_path,first_seen_at,audio_properties,track_key,source_key,object_id,album_key,title,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri,source_loudness_analysis_key,loudness_analysis_key) VALUES('first','liner',120,'mb-recording-1',NULL,NULL,NULL,NULL,X'7B22456D626564646564223A7B22736F757263655F6964223A226C6F63616C2D736F75726365222C2270617468223A222F6D757369632F416C62756D2F30312E666C6163222C227265766973696F6E223A227231222C22706963747572655F696E646578223A307D7D',0,NULL,0,'flac',2001,'2001-05-01','2020-01-02T03:04:05Z','/music/Album/01.flac',1577934245,'{"sample_rate_hz":44100,"bit_depth":16,"channels":2}',1,10,'/music/Album/01.flac',1,'First','Album','Artist A','first',200000,1,1,'file:///music/Album/01.flac',X'0000000000000000000000000000000000000000000000000000000000000000',X'0000000000000000000000000000000000000000000000000000000000000000');
INSERT INTO tracks(normalized_search,comment,bpm,musicbrainz_recording_id,musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis,artwork_binding,source_favorite,source_rating,local_play_count,source_format,year,release_date,date_added,source_path,first_seen_at,audio_properties,track_key,source_key,object_id,album_key,title,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri,source_loudness_analysis_key,loudness_analysis_key) VALUES('cue one',NULL,NULL,NULL,NULL,'/music/Album/album.cue',0,100000,NULL,0,NULL,0,'flac',NULL,NULL,NULL,'/music/Album/album.flac',1577934245,'{}',2,10,'album-cue:1',1,'Cue One','Album','Artist A','cue one',100000,1,2,'rufin:cue/album-cue%3A1/file%3A%2F%2F%2Fmusic%2FAlbum%2Falbum.flac/0/100000',X'0000000000000000000000000000000000000000000000000000000000000000',X'0000000000000000000000000000000000000000000000000000000000000000');
INSERT INTO tracks(normalized_search,comment,bpm,musicbrainz_recording_id,musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis,artwork_binding,source_favorite,source_rating,local_play_count,source_format,year,release_date,date_added,source_path,first_seen_at,audio_properties,track_key,source_key,object_id,album_key,title,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri,source_loudness_analysis_key,loudness_analysis_key) VALUES('server one',NULL,NULL,NULL,NULL,NULL,NULL,NULL,X'7B22736F757263655F6964223A22736572766572222C22696D616765223A7B226974656D5F6964223A22636F7665722D7331222C22746167223A2274227D7D',1,80,0,'mp3',NULL,NULL,'2021-06-07T00:00:00Z',NULL,NULL,'{}',3,11,'s1',2,'Server One','Server Album','Server Artist','server one',180000,1,1,'rufin:source/track/server/s1',X'0000000000000000000000000000000000000000000000000000000000000000',X'0000000000000000000000000000000000000000000000000000000000000000');
INSERT INTO tracks(normalized_search,comment,bpm,musicbrainz_recording_id,musicbrainz_release_track_id,cue_path,cue_start_millis,cue_end_millis,artwork_binding,source_favorite,source_rating,local_play_count,source_format,year,release_date,date_added,source_path,first_seen_at,audio_properties,track_key,source_key,object_id,album_key,title,display_album,display_artist,sort_text,duration_millis,disc_number,track_number,media_uri,source_loudness_analysis_key,loudness_analysis_key) VALUES('server two',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,'{}',4,11,'s2',2,'Server Two','Server Album','Server Artist','server two',150000,1,2,'rufin:source/track/server/s2',X'0000000000000000000000000000000000000000000000000000000000000000',X'0000000000000000000000000000000000000000000000000000000000000000');
INSERT INTO track_artists(track_key,artist_key,position) VALUES(1,1,0);
INSERT INTO track_artists(track_key,artist_key,position) VALUES(2,1,0);
INSERT INTO track_artists(track_key,artist_key,position) VALUES(3,2,0);
INSERT INTO track_artists(track_key,artist_key,position) VALUES(4,2,0);
INSERT INTO track_genres(track_key,genre_key,position) VALUES(1,1,0);
INSERT INTO track_genres(track_key,genre_key,position) VALUES(3,2,0);
INSERT INTO track_moods(track_key,mood_key,position) VALUES(1,1,0);
INSERT INTO track_folders(track_key,folder_key,position) VALUES(1,1,0);
INSERT INTO track_folders(track_key,folder_key,position) VALUES(2,1,0);
INSERT INTO album_artists(album_key,artist_key,position) VALUES(1,1,0);
INSERT INTO album_artists(album_key,artist_key,position) VALUES(2,2,0);
INSERT INTO album_genres(album_key,genre_key,position) VALUES(1,1,0);
INSERT INTO album_release_types(album_key,release_type,position) VALUES(1,'album',0);
INSERT INTO local_files(local_file_key,source_key,path,root,relative_path,kind,size_bytes,mtime_ns,state,revision,parse_version) VALUES(1,10,'/music/Album','/music','Album','directory',NULL,1,'accepted',NULL,1);
INSERT INTO local_files(local_file_key,source_key,path,root,relative_path,kind,size_bytes,mtime_ns,state,revision,parse_version) VALUES(2,10,'/music/Album/01.flac','/music','Album/01.flac','media',1000,2,'accepted','r1',1);
INSERT INTO local_files(local_file_key,source_key,path,root,relative_path,kind,size_bytes,mtime_ns,state,revision,parse_version) VALUES(3,10,'/music/Album/album.cue','/music','Album/album.cue','cue',100,3,'accepted','r2',1);
INSERT INTO local_files(local_file_key,source_key,path,root,relative_path,kind,size_bytes,mtime_ns,state,revision,parse_version) VALUES(4,10,'/music/Album/album.flac','/music','Album/album.flac','media',5000,4,'accepted','r3',1);
INSERT INTO local_file_dependencies(local_file_key,dependency_path,position) VALUES(3,'/music/Album/album.flac',0);
INSERT INTO lyrics_cache(media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at) VALUES('file:///music/Album/01.flac','source','original','','',X'0000000000000000000000000000000000000000000000000000000000000000','{"version":4}',100);
INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) VALUES(10,'track',1,X'0000000000000000000000000000000000000000000000000000000000000000',-10.5,0.9,'source');
INSERT INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak,origin) VALUES(11,'track',3,X'0000000000000000000000000000000000000000000000000000000000000000',-12.0,0.8,'analysis');
INSERT INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,section_title) VALUES(11,'recent',0,'album',2,'Server Album','Server Artist','Recently added');
INSERT INTO native_playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,artwork_binding,writable,metadata_writable,provider_revision,valid_until) VALUES(1,11,'p100','Server Mix','server mix','server mix',NULL,1,1,'rev-7',NULL);
INSERT INTO native_playlist_entries(playlist_entry_key,playlist_key,object_id,media_uri,title,snapshot_at,position) VALUES(1,1,'pe1','rufin:source/track/server/s1','Server One',1,0);
INSERT INTO native_playlist_entries(playlist_entry_key,playlist_key,object_id,media_uri,title,snapshot_at,position) VALUES(2,1,'pe2','rufin:source/track/server/s2','Server Two',1,1);
INSERT INTO local_access_metadata(access_uri,size_bytes,mtime_ns,device_id,inode,parser_version,title,normalized_title,album,normalized_album,artist,normalized_artist,disc_number,track_number,duration_millis,loudness_analysis_key) VALUES('file:///downloads/s1.flac',900,5,NULL,NULL,1,'Server One','server one','Server Album','server album','Server Artist','server artist',1,1,180000,NULL);
