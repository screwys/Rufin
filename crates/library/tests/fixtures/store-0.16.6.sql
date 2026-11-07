-- The store file as Rufin 0.16.6 created it, with a small library.
PRAGMA application_id = 1381320270;

PRAGMA user_version = 45;

CREATE TABLE IF NOT EXISTS source_ids (source_key INTEGER PRIMARY KEY, object_id TEXT NOT NULL UNIQUE CHECK(object_id<>'')) STRICT;

CREATE TABLE IF NOT EXISTS queue_saved (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    state TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS playlists (
    playlist_key INTEGER PRIMARY KEY,
    source_key INTEGER REFERENCES source_ids ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT,
    normalized_name TEXT,
    sort_text TEXT,
    position INTEGER NOT NULL CHECK (position >= 0),
    artwork_bytes BLOB,
    artwork_mime TEXT,
    artwork_revision TEXT,
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
CREATE INDEX IF NOT EXISTS playlist_entries_shuffle_idx ON playlist_entries(playlist_key,((playlist_entry_key*1103515245)%2147483647),playlist_entry_key);

CREATE INDEX IF NOT EXISTS playlist_entries_media_idx ON playlist_entries(media_uri,(title IS NULL),snapshot_at DESC,playlist_entry_key DESC);

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
    current_occurrence_id TEXT,
    progress_millis INTEGER NOT NULL DEFAULT 0 CHECK (progress_millis >= 0),
    repeat_mode TEXT NOT NULL DEFAULT 'none' CHECK (repeat_mode IN ('none', 'one', 'all')),
    shuffled INTEGER NOT NULL DEFAULT 0 CHECK (shuffled IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS queue_order (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    state TEXT NOT NULL
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

CREATE TABLE IF NOT EXISTS queue_occurrences (
    queue_occurrence_key INTEGER PRIMARY KEY,
    received_from_connect INTEGER NOT NULL DEFAULT 0,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    media_uri TEXT NOT NULL CHECK (media_uri <> ''),
    snapshot_at INTEGER NOT NULL DEFAULT (unixepoch()),
    position INTEGER NOT NULL CHECK (position >= 0),
    traversal_position INTEGER NOT NULL CHECK (traversal_position >= 0),
    origin_source INTEGER,
    origin_position INTEGER,
    playlist_entry_id TEXT,
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

DROP INDEX IF EXISTS queue_occurrences_page_idx;
DROP INDEX IF EXISTS queue_occurrences_traversal_idx;

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

INSERT INTO source_ids(source_key,object_id) VALUES(1,'local-source');
INSERT INTO source_ids(source_key,object_id) VALUES(2,'server');
INSERT INTO playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,position) VALUES(1,1,'rufin:playlist:local','Local Mix','local mix','local mix',1);
INSERT INTO playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,position,artwork_bytes,artwork_mime,artwork_revision) VALUES(2,NULL,'rufin:playlist:global','Global Mix','global mix','global mix',0,X'89504E47','image/png','art-1');
INSERT INTO playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,position) VALUES(3,2,'p100',NULL,NULL,NULL,2);
INSERT INTO playlist_entries(album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,playlist_entry_key,playlist_key,object_id,media_uri,title,artist,album,position) VALUES(NULL,10,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,1,1,'e1','file:///music/Album/01.flac','First','Artist A','Album',0);
INSERT INTO playlist_entries(album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,playlist_entry_key,playlist_key,object_id,media_uri,title,artist,album,position) VALUES(NULL,10,123000,NULL,NULL,NULL,NULL,NULL,'mb-gone',NULL,2,1,'e2','file:///music/Gone/old.flac','Gone Song','Old Artist','Old Album',1);
INSERT INTO playlist_entries(album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,playlist_entry_key,playlist_key,object_id,media_uri,title,artist,album,position) VALUES(NULL,10,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,3,2,'e3','rufin:source/track/server/s1','Server One','Server Artist','Server Album',0);
INSERT INTO playlist_entries(album_display_artist,snapshot_at,duration_millis,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,playlist_entry_key,playlist_key,object_id,media_uri,title,artist,album,position) VALUES(NULL,10,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,4,2,'e4','file:///music/Album/01.flac','First','Artist A','Album',1);
INSERT INTO smart_playlists(smart_playlist_key,object_id,name,normalized_name,definition_json,position) VALUES(1,'rufin:smart:recent','Recently Played','recently played','{"current":true,"match_all":[{"field":"PlayCount","operator":"Above","value":{"Number":0}}],"sort_field":"PlayCount","descending":true,"activity_period":"Weekly","limit":100}',0);
INSERT INTO favorite_outbox(outbox_key,media_uri,favorite,previous_favorite,attempts,next_attempt_at) VALUES(1,'rufin:source/track/server/s1',0,1,1,500);
INSERT INTO user_media_state(media_uri,favorite,rating) VALUES('file:///music/Album/01.flac',1,60);
INSERT INTO user_media_state(media_uri,favorite,rating) VALUES('file:///music/Gone/old.flac',1,NULL);
INSERT INTO user_media_state(media_uri,favorite,rating) VALUES('rufin:source/album/local-source/album-1',1,NULL);
INSERT INTO listens(external_id,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,skipped,listen_key,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES('listen-1',NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,1,'local-source','file:///music/Album/01.flac','First','Artist A','Album',1600000000,'2020-09',200000,200000);
INSERT INTO listens(external_id,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,skipped,listen_key,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES('listen-2',NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,2,'local-source','file:///music/Gone/old.flac','Gone Song','Old Artist','Old Album',1600000500,'2020-09',123000,100000);
INSERT INTO listens(external_id,disc_number,track_number,year,release_date,source_format,musicbrainz_recording_id,musicbrainz_release_track_id,skipped,listen_key,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis) VALUES(NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,0,3,'server','rufin:source/track/server/vanished','Vanished','Lost Artist','Lost Album',1500000000,'2017-07',100000,90000);
INSERT INTO listen_outbox(outbox_key,listen_key,service,account_id,attempts,next_attempt_at,last_error) VALUES(1,1,'lastfm','me',2,700,'offline');
INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES('server','lifetime','track','s2',5,1,1400000000);
INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES('server','2017-07','track','vanished',2,0,1500000000);
INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at) VALUES('server','lifetime','artist','ar2',9,0,NULL);
INSERT INTO local_locators(local_access_file_key,source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(1,2,'rufin:source/track/server/s1','download','/downloads/s1.flac','/downloads','s1.flac','file:///downloads/s1.flac');
INSERT INTO connect_media_files(media_uri,encoding,revision,path,managed,hash,backing_uri) VALUES('rufin:source/track/server/s1','original','r1','file:///downloads/s1.flac',1,NULL,NULL);
INSERT INTO playlist_file_links(playlist_key,source_id,path,auto_refresh,auto_save,path_mode,revision,dirty,error) VALUES(1,'local-source','/music/Local Mix.m3u8',1,1,'relative','m1',0,NULL);
INSERT INTO queue_saved(singleton,state) VALUES(1,'{"entries":[["occ-a","file:///music/Album/01.flac","Manual",null],["occ-b","file:///music/Gone/old.flac",[0,1],null],["occ-c","rufin:source/track/server/s1","Manual",null]],"contexts":["album:local"],"next_id":4}');
INSERT INTO queue_order(singleton,state) VALUES(1,'[2,0,1]');
INSERT INTO queue_state(singleton,current_occurrence_id,progress_millis,repeat_mode,shuffled) VALUES(1,'occ-b',5000,'all',0);
