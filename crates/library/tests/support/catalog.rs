use std::path::Path;

use sqlx::SqliteConnection;

pub async fn copy_before_index(connection: &mut SqliteConnection, source: &Path) {
    sqlx::raw_sql(r#"-- Only the catalog tables changed by the index migration.
-- Other tables and sample rows come from the existing test database.
PRAGMA foreign_keys = OFF;
PRAGMA application_id = 1381320270;
PRAGMA user_version = 50;

CREATE TABLE IF NOT EXISTS sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    display_name TEXT NOT NULL DEFAULT '', normalized_name TEXT NOT NULL DEFAULT '', freshness BLOB,
    kind TEXT,
    artwork_digest BLOB NOT NULL DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000' CHECK (length(artwork_digest) = 32),
    distinct_track_covers INTEGER NOT NULL DEFAULT 0 CHECK (distinct_track_covers IN (0,1)),
    catalog_revision INTEGER NOT NULL DEFAULT 0 CHECK (catalog_revision >= 0),
    catalog_complete INTEGER NOT NULL DEFAULT 0 CHECK (catalog_complete IN (0,1)),
    observation_stamp INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE IF NOT EXISTS albums (
    album_key INTEGER PRIMARY KEY,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    object_id TEXT NOT NULL, media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    cached_local INTEGER NOT NULL DEFAULT 0,
    normalized_title TEXT,
    native_artwork BLOB,
    artwork_binding BLOB, artwork_track_key INTEGER, artwork_artist_key INTEGER,
    first_seen_at INTEGER,
    release_lookup_identity TEXT,
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
    local_play_count INTEGER NOT NULL DEFAULT 0,
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

CREATE TABLE IF NOT EXISTS album_release_types (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    release_type TEXT NOT NULL CHECK (release_type <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, release_type)
) STRICT;

CREATE TABLE IF NOT EXISTS album_order (
    album_key INTEGER PRIMARY KEY REFERENCES albums ON DELETE CASCADE,
    source_key INTEGER NOT NULL REFERENCES sources ON DELETE CASCADE,
    title_sort TEXT NOT NULL, title_search TEXT NOT NULL DEFAULT '', artist_search TEXT NOT NULL DEFAULT '', album_artist_sort TEXT,
    year_sort INTEGER, release_sort TEXT, added_sort INTEGER
) STRICT;

-- These views are retired without reading their contents.
CREATE VIEW effective_album_artists AS SELECT * FROM album_artists;
CREATE VIEW effective_album_genres AS SELECT * FROM album_genres;
CREATE VIEW effective_album_release_types AS SELECT * FROM album_release_types;
"#)
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("ATTACH DATABASE ?1 AS accepted")
        .bind(source.to_str().unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    let tables: Vec<(String, String)> = sqlx::query_as(
        "SELECT name,sql FROM accepted.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN('folder_children','collection_album_index','collection_totals','playlist_genres')",
    )
    .fetch_all(&mut *connection)
    .await
    .unwrap();
    for (table, create) in tables {
        if !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE type='table' AND name=?1)",
        )
        .bind(&table)
        .fetch_one(&mut *connection)
        .await
        .unwrap()
        {
            sqlx::raw_sql(sqlx::AssertSqlSafe(create))
                .execute(&mut *connection)
                .await
                .unwrap();
        }
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT old.name FROM pragma_table_xinfo(?1,'main') old JOIN pragma_table_xinfo(?1,'accepted') current USING(name) WHERE old.hidden=0 ORDER BY old.cid",
        )
        .bind(&table)
        .fetch_all(&mut *connection)
        .await
        .unwrap();
        let values = columns
            .iter()
            .map(|column| {
                if table == "albums" && column == "metadata" {
                    "input_metadata"
                } else {
                    column.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO main.{table}({}) SELECT {values} FROM accepted.{table}",
            columns.join(",")
        )))
        .execute(&mut *connection)
        .await
        .unwrap();
    }
    sqlx::query("DETACH DATABASE accepted")
        .execute(connection)
        .await
        .unwrap();
}
