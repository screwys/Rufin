//! One-time conversion of the former durable and catalog files.
use super::track_uri;
use crate::{LibraryError, LibraryResult, SourceId, source_entity_uri};
use sqlx::{Connection, Row, SqliteConnection};
use std::path::Path;

pub(crate) async fn attach(
    connection: &mut SqliteConnection,
    path: &Path,
    alias: &str,
) -> LibraryResult<()> {
    let mut uri = url::Url::from_file_path(path.canonicalize()?)
        .map_err(|_| LibraryError::Migration("database path is not absolute".into()))?;
    uri.set_query(Some("mode=ro"));
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ATTACH DATABASE ?1 AS {alias}"
    )))
    .bind(uri.as_str())
    .execute(connection)
    .await?;
    Ok(())
}

async fn columns(
    connection: &mut SqliteConnection,
    schema: &str,
    table: &str,
) -> LibraryResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT name FROM pragma_table_info(?1,?2)")
            .bind(table)
            .bind(schema)
            .fetch_all(connection)
            .await?,
    )
}

pub(super) async fn has_table(
    connection: &mut SqliteConnection,
    schema: &str,
    table: &str,
) -> LibraryResult<bool> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_schema WHERE type='table' AND name=?1)"
    )))
    .bind(table)
    .fetch_one(connection)
    .await?)
}

async fn copy_table(
    connection: &mut SqliteConnection,
    schema: &str,
    from: &str,
    to: &str,
    overrides: &[(&str, &str)],
) -> LibraryResult<()> {
    let old = columns(connection, schema, from).await?;
    if old.is_empty() {
        return Ok(());
    }
    let target = columns(connection, "main", to).await?;
    let selected: Vec<_> = target
        .iter()
        .filter_map(|name| {
            overrides
                .iter()
                .find(|(field, _)| *field == name)
                .map(|(_, expression)| (name.clone(), expression.to_string()))
                .or_else(|| {
                    old.contains(name)
                        .then(|| (name.clone(), format!("old.\"{name}\"")))
                })
        })
        .collect();
    let names = selected
        .iter()
        .map(|(name, _)| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    let expressions = selected
        .iter()
        .map(|(_, expression)| expression.as_str())
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO main.{to}({names}) SELECT {expressions} FROM {schema}.{from} old"
    )))
    .execute(connection)
    .await?;
    Ok(())
}

pub(super) async fn copy_catalog(
    connection: &mut SqliteConnection,
    schema: &str,
    released: bool,
) -> LibraryResult<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO sources(object_id) SELECT object_id FROM {schema}.sources WHERE true ON CONFLICT(object_id) DO NOTHING")))
        .execute(&mut *connection).await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE TEMP TABLE migration_source_keys AS SELECT old.source_key old_key,current.source_key FROM {schema}.sources old JOIN main.sources current USING(object_id)")))
        .execute(&mut *connection).await?;
    let source_columns = columns(connection, schema, "sources").await?;
    let target_columns = columns(connection, "main", "sources").await?;
    let updates = target_columns
        .iter()
        .filter(|name| {
            !matches!(name.as_str(), "source_key" | "object_id" | "track_count")
                && source_columns.contains(name)
        })
        .map(|name| format!("\"{name}\"=old.\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE main.sources SET {updates} FROM {schema}.sources old WHERE sources.object_id=old.object_id")))
        .execute(&mut *connection).await?;
    if !source_columns
        .iter()
        .any(|column| column == "catalog_complete")
    {
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE sources SET catalog_complete=(old.freshness IS NOT NULL) FROM {schema}.sources old WHERE sources.object_id=old.object_id")))
            .execute(&mut *connection).await?;
    }
    sqlx::raw_sql(
        "CREATE TEMP TABLE migration_uris(entity_key INTEGER PRIMARY KEY,uri TEXT NOT NULL)",
    )
    .execute(&mut *connection)
    .await?;
    for (table, key, kind) in [
        ("albums", "album_key", "album"),
        ("artists", "artist_key", "artist"),
        ("tracks", "track_key", "track"),
        ("genres", "genre_key", "genre"),
        ("moods", "mood_key", "mood"),
        ("folders", "folder_key", "folder"),
    ] {
        let mut after = 0_i64;
        loop {
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT old.*,source.object_id migration_source FROM {schema}.{table} old JOIN {schema}.sources source USING(source_key) WHERE old.{key}>?1 ORDER BY old.{key} LIMIT 256")))
                .bind(after).fetch_all(&mut *connection).await?;
            if rows.is_empty() {
                break;
            }
            let mut values = Vec::with_capacity(rows.len());
            for row in rows {
                after = row.try_get(key)?;
                let source: String = row.try_get("migration_source")?;
                let object: String = row.try_get("object_id")?;
                let uri = if kind == "track" && released {
                    track_uri(&row, &source, &object)
                } else {
                    super::string(&row, "media_uri")
                }
                .unwrap_or_else(|| source_entity_uri(&SourceId::new(source), kind, &object));
                values.push((after, uri));
            }
            let mut insert = sqlx::QueryBuilder::new("INSERT INTO migration_uris(entity_key,uri) ");
            insert.push_values(&values, |mut row, (key, uri)| {
                row.push_bind(key).push_bind(uri);
            });
            insert.build().execute(&mut *connection).await?;
        }
        let uri = format!("(SELECT uri FROM migration_uris WHERE entity_key=old.{key})");
        let mut overrides = vec![
            (
                "source_key",
                "(SELECT source_key FROM migration_source_keys WHERE old_key=old.source_key)",
            ),
            ("media_uri", uri.as_str()),
            ("local_play_count", "0"),
        ];
        if !columns(connection, schema, table)
            .await?
            .iter()
            .any(|column| column == "indexed")
        {
            overrides.push(("indexed", "1"));
        }
        copy_table(connection, schema, table, table, &overrides).await?;
        sqlx::query("DELETE FROM migration_uris")
            .execute(&mut *connection)
            .await?;
    }
    for table in [
        "album_artists",
        "track_artists",
        "track_album_artists",
        "album_genres",
        "track_genres",
        "track_moods",
        "track_folders",
        "album_release_types",
        "home_entries",
        "local_files",
        "local_file_dependencies",
    ] {
        copy_table(
            connection,
            schema,
            table,
            table,
            &[(
                "source_key",
                "(SELECT source_key FROM migration_source_keys WHERE old_key=old.source_key)",
            )],
        )
        .await?;
    }
    let mut after = 0_i64;
    loop {
        let files: Vec<(i64, String)> = sqlx::query_as("SELECT local_file_key,path FROM local_files WHERE local_file_key>?1 ORDER BY local_file_key LIMIT 128")
            .bind(after).fetch_all(&mut *connection).await?;
        if files.is_empty() {
            break;
        }
        for (key, path) in files {
            after = key;
            sqlx::query("UPDATE local_files SET parent_path=?2,parsed_stamp=CASE WHEN state IN('accepted','rejected') THEN CASE WHEN kind='directory' THEN json_array(size_bytes,mtime_ns,revision,parse_version,observed_stamp) ELSE json_array(size_bytes,mtime_ns,revision,parse_version) END END WHERE local_file_key=?1")
                .bind(key).bind(crate::catalog::files::parent_locator(&path)).execute(&mut *connection).await?;
        }
    }
    if released {
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO lyrics_cache(media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at) SELECT track.media_uri,lyrics.authority,lyrics.role,lyrics.language,lyrics.script,lyrics.cache_input_digest,lyrics.lyrics,lyrics.updated_at FROM {schema}.lyrics_cache lyrics JOIN tracks track USING(track_key)")))
            .execute(&mut *connection).await?;
        copy_table(
            connection,
            schema,
            "local_access_files",
            "local_access_metadata",
            &[("access_uri", "old.media_uri")],
        )
        .await?;
    } else {
        copy_table(connection, schema, "lyrics_cache", "lyrics_cache", &[]).await?;
        copy_table(
            connection,
            schema,
            "local_access_metadata",
            "local_access_metadata",
            &[],
        )
        .await?;
        if has_table(connection, schema, "activity_baseline").await? {
            for (table, kind) in [
                ("tracks", "track"),
                ("artists", "artist"),
                ("genres", "genre"),
            ] {
                sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO native_media_state(media_uri,play_count,skip_count,last_played) SELECT item.media_uri,activity.play_count,activity.skip_count,activity.last_played_at FROM {schema}.activity_baseline activity JOIN migration_source_keys mapping ON mapping.old_key=activity.source_key JOIN {table} item ON item.source_key=mapping.source_key AND item.object_id=activity.track_object_id WHERE activity.period='lifetime' AND activity.item_kind='{kind}' ON CONFLICT(media_uri) DO UPDATE SET play_count=excluded.play_count,skip_count=excluded.skip_count,last_played=excluded.last_played")))
                    .execute(&mut *connection).await?;
            }
        }
    }
    crate::migration::snapshots::populate(connection, &format!("{schema}.")).await?;
    if has_table(connection, schema, "loudness_measurements").await? {
        let cols = columns(connection, schema, "loudness_measurements").await?;
        if cols.iter().any(|col| col == "origin") {
            sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR REPLACE INTO loudness_measurements(source_key,entity_kind,entity_key,analysis_key,integrated_lufs,true_peak) SELECT mapping.source_key,old.entity_kind,old.entity_key,old.analysis_key,old.integrated_lufs,old.true_peak FROM {schema}.loudness_measurements old JOIN migration_source_keys mapping ON mapping.old_key=old.source_key WHERE old.origin='analysis'"))).execute(&mut *connection).await?;
        }
    }
    Ok(())
}

const FIELDS: &[(&str, &str)] = &[
    ("title", "TEXT"),
    ("display_artist", "TEXT"),
    ("display_album", "TEXT"),
    ("album_display_artist", "TEXT"),
    ("duration_millis", "INTEGER"),
    ("disc_number", "INTEGER"),
    ("track_number", "INTEGER"),
    ("year", "INTEGER"),
    ("release_date", "TEXT"),
    ("source_format", "TEXT"),
    ("musicbrainz_recording_id", "TEXT"),
    ("musicbrainz_release_track_id", "TEXT"),
    ("musicbrainz_album_id", "TEXT"),
    ("musicbrainz_release_group_id", "TEXT"),
    ("primary_artist_musicbrainz_id", "TEXT"),
];

pub(super) async fn create_metadata_candidates(
    connection: &mut SqliteConnection,
) -> LibraryResult<()> {
    let fields = FIELDS
        .iter()
        .map(|(name, kind)| format!("{name} {kind}"))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TEMP TABLE migration_metadata(media_uri TEXT NOT NULL,priority INTEGER NOT NULL,present INTEGER NOT NULL,stamp INTEGER NOT NULL,tie INTEGER NOT NULL,{fields}); CREATE INDEX migration_metadata_uri_idx ON migration_metadata(media_uri,priority,present,stamp DESC,tie DESC);")))
        .execute(connection).await?;
    Ok(())
}

async fn snapshot_candidates(
    connection: &mut SqliteConnection,
    schema: &str,
    table: &str,
    priority: i64,
    order: &str,
    renames: &[(&str, &str)],
) -> LibraryResult<()> {
    let old = columns(connection, schema, table).await?;
    if old.is_empty() {
        return Ok(());
    }
    let fields = FIELDS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",");
    let values = FIELDS
        .iter()
        .map(|(name, _)| {
            let column = renames
                .iter()
                .find(|(field, _)| field == name)
                .map_or(*name, |(_, column)| *column);
            if old.iter().any(|name| name == column) {
                format!("old.{column}")
            } else {
                "NULL".into()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,{fields}) SELECT old.media_uri,{priority},{order},{values} FROM {schema}.{table} old")))
        .execute(connection).await?;
    Ok(())
}

async fn connect_candidates(connection: &mut SqliteConnection) -> LibraryResult<()> {
    if !has_table(connection, "old_store", "connect_collection").await? {
        return Ok(());
    }
    sqlx::query("INSERT INTO tracks(media_uri,indexed,metadata,first_seen_at,native_artwork) SELECT media_uri,0,jsonb(json_object('comment',json_extract(payload,'$.comment'),'bpm',json_extract(payload,'$.bpm'),'date_added',json_extract(payload,'$.date_added'))),json_extract(payload,'$.first_seen_at'),unhex(json_extract(payload,'$.artwork_binding')) FROM old_store.connect_collection WHERE true ON CONFLICT(media_uri) DO NOTHING")
        .execute(&mut *connection).await?;
    sqlx::query("INSERT INTO native_media_state(media_uri,favorite,rating) SELECT media_uri,json_extract(payload,'$.source_favorite'),json_extract(payload,'$.source_rating') FROM old_store.connect_collection WHERE true ON CONFLICT(media_uri) DO NOTHING").execute(&mut *connection).await?;
    let fields = FIELDS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",");
    let values = FIELDS
        .iter()
        .map(|(name, _)| format!("json_extract(payload,'$.{name}')"))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,{fields}) SELECT media_uri,5,0,0,0,{values} FROM old_store.connect_collection"))).execute(connection).await?;
    Ok(())
}

pub(super) async fn saved_candidates(
    connection: &mut SqliteConnection,
    schema: &str,
) -> LibraryResult<()> {
    if !has_table(connection, schema, "queue_saved").await? {
        return Ok(());
    }
    let fields = FIELDS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",");
    let values = FIELDS
        .iter()
        .map(|(name, _)| {
            let field = match *name {
                "display_artist" => "artist",
                "display_album" => "album",
                field => field,
            };
            format!("json_extract(item.value,'$.{field}')")
        })
        .collect::<Vec<_>>()
        .join(",");
    sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,{fields}) SELECT json_extract(item.value,'$.media_uri'),2,0,COALESCE(json_extract(item.value,'$.snapshot_at'),0),item.id,{values} FROM {schema}.queue_saved saved,json_tree(saved.state) item WHERE item.type='object' AND json_type(item.value,'$.media_uri')='text'")))
        .execute(connection).await?;
    Ok(())
}

pub(super) async fn row_candidate(
    connection: &mut SqliteConnection,
    row: &sqlx::sqlite::SqliteRow,
    uri: &str,
    priority: i64,
    stamp: i64,
    tie: i64,
    listen: bool,
) -> LibraryResult<()> {
    for field in ["duration_millis", "disc_number", "track_number"] {
        if super::integer(row, field)
            .or_else(|| super::integer(row, &format!("fallback_{field}")))
            .is_some_and(|value| value < 0)
        {
            return Err(LibraryError::InvalidStore(format!("invalid saved {field}")));
        }
    }
    let mut query = sqlx::QueryBuilder::new(
        "INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,",
    );
    query.push(
        FIELDS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(","),
    );
    query.push(") VALUES (");
    query
        .push_bind(uri)
        .push(",")
        .push_bind(priority)
        .push(",")
        .push_bind(i64::from(
            priority == 1 && super::string(row, "title").is_none(),
        ))
        .push(",")
        .push_bind(stamp)
        .push(",")
        .push_bind(tie);
    for (name, kind) in FIELDS {
        let column = match (*name, listen) {
            ("title", true) => "track_title",
            ("display_artist", true) => "artist_name",
            ("display_album", true) => "album_title",
            ("display_artist", false) => "artist",
            ("display_album", false) => "album",
            (name, _) => name,
        };
        query.push(",");
        if *kind == "TEXT" {
            query.push_bind(
                super::string(row, column)
                    .or_else(|| super::string(row, &format!("fallback_{column}"))),
            );
        } else {
            query.push_bind(
                super::integer(row, column)
                    .or_else(|| super::integer(row, &format!("fallback_{column}"))),
            );
        }
    }
    query.push(")").build().execute(connection).await?;
    Ok(())
}

pub(super) async fn reduce_metadata(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::query("INSERT INTO tracks(media_uri,indexed) SELECT DISTINCT media_uri,0 FROM migration_metadata WHERE true ON CONFLICT(media_uri) DO NOTHING")
        .execute(&mut *connection).await?;
    for (field, _) in FIELDS {
        let canonical = match *field {
            "display_artist" => "artist",
            "display_album" => "album",
            field => field,
        };
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE tracks SET metadata=jsonb_set(metadata,'$.{canonical}',(SELECT value.{field} FROM migration_metadata value WHERE value.media_uri=tracks.media_uri AND value.{field} IS NOT NULL ORDER BY value.priority,value.present,value.stamp DESC,value.tie DESC LIMIT 1)) WHERE(json_extract(metadata,'$.{canonical}') IS NULL OR json_extract(metadata,'$.{canonical}')='' OR json_extract(metadata,'$.{canonical}')=0) AND EXISTS(SELECT 1 FROM migration_metadata value WHERE value.media_uri=tracks.media_uri AND value.{field} IS NOT NULL)"))).execute(&mut *connection).await?;
    }
    sqlx::query("UPDATE tracks SET metadata=jsonb_set(metadata,'$.sort_text',lower(coalesce(title,media_uri))) WHERE json_extract(metadata,'$.sort_text') IS NULL").execute(&mut *connection).await?;
    let mut after = 0_i64;
    loop {
        let rows:Vec<(i64,String)>=sqlx::query_as("SELECT track_key,media_uri FROM tracks WHERE track_key>?1 AND source_key IS NULL ORDER BY track_key LIMIT 256").bind(after).fetch_all(&mut *connection).await?;
        if rows.is_empty() {
            break;
        }
        for (key, uri) in rows {
            after = key;
            if let Some((source, kind, object)) = crate::source_entity_parts(&uri)
                && kind == "track"
            {
                let source_key =
                    crate::db::write_source_identity(connection, source.as_str()).await?;
                sqlx::query("UPDATE tracks SET source_key=?2,object_id=?3 WHERE track_key=?1")
                    .bind(key)
                    .bind(source_key)
                    .bind(object)
                    .execute(&mut *connection)
                    .await?;
            }
        }
    }
    let mut after = 0_i64;
    loop {
        let rows:Vec<(i64,String)>=sqlx::query_as("SELECT track_key,json(metadata) FROM tracks WHERE track_key>?1 ORDER BY track_key LIMIT 128").bind(after).fetch_all(&mut *connection).await?;
        if rows.is_empty() {
            break;
        }
        for (key, payload) in rows {
            after = key;
            let metadata: crate::TrackMetadata = serde_json::from_str(&payload)?;
            let normalized = crate::catalog::cache::track_search(&metadata);
            sqlx::query("UPDATE tracks SET normalized_search=?2 WHERE track_key=?1 AND normalized_search<>?2").bind(key).bind(normalized).execute(&mut *connection).await?;
        }
    }
    sqlx::query("DROP TABLE migration_metadata")
        .execute(&mut *connection)
        .await?;
    Ok(())
}

pub(super) async fn copy_native_playlists(connection: &mut SqliteConnection) -> LibraryResult<()> {
    if !has_table(connection, "old_catalog", "native_playlists").await? {
        return Ok(());
    }
    let columns = columns(connection, "old_catalog", "native_playlists").await?;
    let column = |name: &str, absent: &str| {
        if columns.iter().any(|column| column == name) {
            format!("old.{name}")
        } else {
            absent.to_owned()
        }
    };
    let writable = column("writable", "1");
    let metadata_writable = column("metadata_writable", &writable);
    let provider_revision = column("provider_revision", "NULL");
    let valid_until = column("valid_until", "NULL");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE TEMP TABLE migration_playlist_keys AS SELECT old.playlist_key old_key,COALESCE(owned.playlist_key,(SELECT COALESCE(max(playlist_key),0) FROM playlists)+row_number() OVER(ORDER BY old.playlist_key)) playlist_key FROM old_catalog.native_playlists old JOIN migration_source_keys source ON source.old_key=old.source_key LEFT JOIN playlists owned ON owned.source_key=source.source_key AND owned.object_id=old.object_id;
            INSERT INTO playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,owner,position,artwork_binding,writable,metadata_writable,provider_revision,valid_until)
            SELECT mapping.playlist_key,source.source_key,old.object_id,old.name,old.normalized_name,old.sort_text,'source',COALESCE(owned.position,(SELECT COALESCE(max(position),-1) FROM playlists)+row_number() OVER(ORDER BY old.playlist_key)),old.artwork_binding,{writable},{metadata_writable},{provider_revision},{valid_until}
            FROM old_catalog.native_playlists old JOIN migration_playlist_keys mapping ON mapping.old_key=old.playlist_key JOIN migration_source_keys source ON source.old_key=old.source_key LEFT JOIN playlists owned ON owned.playlist_key=mapping.playlist_key WHERE true
            ON CONFLICT(playlist_key) DO UPDATE SET owner='source',indexed=1,name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text,artwork_binding=COALESCE(playlists.artwork_binding,excluded.artwork_binding),writable=excluded.writable,metadata_writable=excluded.metadata_writable,provider_revision=excluded.provider_revision,valid_until=excluded.valid_until;")))
            .execute(&mut *connection).await?;
    sqlx::raw_sql("DELETE FROM playlist_entries WHERE playlist_key IN (SELECT playlist_key FROM migration_playlist_keys);
            INSERT INTO playlist_entries(playlist_entry_key,playlist_key,object_id,track_key,position)
            SELECT (SELECT COALESCE(max(playlist_entry_key),0) FROM playlist_entries)+row_number() OVER(ORDER BY old.playlist_entry_key),mapping.playlist_key,old.object_id,track.track_key,old.position
            FROM old_catalog.native_playlist_entries old JOIN migration_playlist_keys mapping ON mapping.old_key=old.playlist_key JOIN tracks track ON track.media_uri=old.media_uri;
            UPDATE home_entries SET entity_key=(SELECT playlist_key FROM migration_playlist_keys WHERE old_key=-home_entries.entity_key) WHERE entity_kind='playlist' AND entity_key<0;")
            .execute(&mut *connection).await?;
    Ok(())
}

pub(super) async fn native_candidates(connection: &mut SqliteConnection) -> LibraryResult<()> {
    snapshot_candidates(
        connection,
        "old_catalog",
        "native_playlist_entries",
        1,
        "(old.title IS NULL),old.snapshot_at,-old.playlist_entry_key",
        &[("display_artist", "artist"), ("display_album", "album")],
    )
    .await
}

async fn playlists(connection: &mut SqliteConnection, catalog: bool) -> LibraryResult<()> {
    copy_table(
        connection,
        "old_store",
        "playlists",
        "playlists",
        &[
            (
                "owner",
                "CASE WHEN old.name IS NULL THEN 'source' ELSE 'rufin' END",
            ),
            ("indexed", "CASE WHEN old.name IS NULL THEN 0 ELSE 1 END"),
        ],
    )
    .await?;
    sqlx::query("UPDATE playlists SET artwork_binding=CAST(json_object('kind','playlist','source_id',(SELECT object_id FROM sources WHERE source_key=playlists.source_key),'object_id',object_id,'revision',artwork_revision) AS BLOB) WHERE artwork_revision IS NOT NULL")
        .execute(&mut *connection).await?;
    copy_table(
        connection,
        "old_store",
        "playlist_entries",
        "playlist_entries",
        &[(
            "track_key",
            "(SELECT track_key FROM tracks WHERE media_uri=old.media_uri)",
        )],
    )
    .await?;
    if catalog {
        copy_native_playlists(connection).await?;
    }
    for table in [
        "playlist_file_links",
        "playlist_file_ignored",
        "playlist_file_settings",
    ] {
        copy_table(connection, "old_store", table, table, &[]).await?;
    }
    Ok(())
}

async fn restore_saved_queue(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let saved = if has_table(connection, "old_store", "queue_saved").await? {
        sqlx::query_scalar::<_, String>("SELECT state FROM old_store.queue_saved WHERE singleton=1")
            .fetch_optional(&mut *connection)
            .await?
            .map(|json| serde_json::from_str::<serde_json::Value>(&json))
            .transpose()?
    } else {
        None
    };
    let (mut state, occurrences) = match saved {
        Some(saved) if saved.get("entries").is_some() => {
            crate::queue::legacy::decode_saved(connection, saved).await?
        }
        Some(saved) => {
            let (rows, _) = crate::queue::legacy::stored_rows(connection).await?;
            crate::queue::legacy::migrate_saved(connection, saved, &rows).await?
        }
        None => {
            let (rows, keys) = crate::queue::legacy::stored_rows(connection).await?;
            let mut ordinary = (0..rows.len()).collect::<Vec<_>>();
            ordinary.sort_unstable_by_key(|index| rows[*index].canonical_position);
            let mut order = vec![0; rows.len()];
            let mut entries = Vec::with_capacity(rows.len());
            let mut occurrences = Vec::with_capacity(rows.len());
            for (position, index) in ordinary.into_iter().enumerate() {
                order[index] = position as u32;
                entries.push(rows[index].entry(keys[index]));
                occurrences.push(Some(rows[index].occurrence.clone()));
            }
            (
                crate::QueueRestore {
                    ids: (1..=entries.len() as u64).collect(),
                    entries: entries.into(),
                    order: order.into(),
                    ..Default::default()
                },
                occurrences,
            )
        }
    };
    if has_table(connection, "old_store", "queue_order").await? {
        if let Some(order) = sqlx::query_scalar::<_, String>(
            "SELECT state FROM old_store.queue_order WHERE singleton=1",
        )
        .fetch_optional(&mut *connection)
        .await?
        {
            state.order = serde_json::from_str(&order)?;
        }
    }
    if columns(connection, "old_store", "queue_occurrences")
        .await?
        .iter()
        .any(|column| column == "received_from_connect")
    {
        let received = sqlx::query_scalar::<_, String>(
            "SELECT object_id FROM old_store.queue_occurrences WHERE received_from_connect=1",
        )
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
        for (entry, occurrence) in std::sync::Arc::make_mut(&mut state.entries)
            .iter_mut()
            .zip(&occurrences)
        {
            entry.received_from_connect = occurrence
                .as_ref()
                .is_some_and(|occurrence| received.contains(occurrence));
        }
    }
    if let Some((current,progress,repeat,shuffled))=sqlx::query_as::<_,(Option<String>,i64,String,bool)>("SELECT current_occurrence_id,progress_millis,repeat_mode,shuffled FROM old_store.queue_state WHERE singleton=1").fetch_optional(&mut *connection).await? {
        state.current_index=current.and_then(|current|state.order.iter().position(|index|occurrences.get(*index as usize).and_then(Option::as_deref)==Some(current.as_str())));
        state.progress_millis=progress;
        state.repeat_mode=match repeat.as_str() {"none"=>crate::QueueRepeatMode::Off,"one"=>crate::QueueRepeatMode::One,"all"=>crate::QueueRepeatMode::All,_=>return Err(LibraryError::InvalidStore("invalid saved repeat mode".into()))};
        state.shuffled=shuffled;
    }
    crate::queue::save_queue_on(connection, &state).await
}

pub(crate) async fn import(input: &Path, destination: &Path, catalog: &Path) -> LibraryResult<()> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix("rufin-migration-")
        .tempdir_in(parent)?;
    let pending = stage.path().join("library.sqlite");
    let mut connection = crate::db::open_writer(&pending).await?;
    crate::schema::initialize_durable(&mut connection).await?;
    crate::connect::initialize(&mut connection).await?;
    attach(&mut connection, input, "old_store").await?;
    let has_catalog = catalog.exists();
    if has_catalog {
        attach(&mut connection, catalog, "old_catalog").await?;
    }
    let mut transaction = connection.begin().await?;
    sqlx::query("DELETE FROM smart_playlists")
        .execute(&mut *transaction)
        .await?;
    sqlx::query("INSERT INTO sources(source_key,object_id) SELECT source_key,object_id FROM old_store.source_ids")
        .execute(&mut *transaction).await?;
    if has_catalog {
        copy_catalog(&mut transaction, "old_catalog", false).await?;
    }
    create_metadata_candidates(&mut transaction).await?;
    connect_candidates(&mut transaction).await?;
    snapshot_candidates(
        &mut transaction,
        "old_store",
        "playlist_entries",
        1,
        "(old.title IS NULL),old.snapshot_at,old.playlist_entry_key",
        &[("display_artist", "artist"), ("display_album", "album")],
    )
    .await?;
    if has_catalog {
        snapshot_candidates(
            &mut transaction,
            "old_catalog",
            "native_playlist_entries",
            1,
            "(old.title IS NULL),old.snapshot_at,-old.playlist_entry_key",
            &[("display_artist", "artist"), ("display_album", "album")],
        )
        .await?;
    }
    snapshot_candidates(
        &mut transaction,
        "old_store",
        "queue_occurrences",
        2,
        "0,old.snapshot_at,old.queue_occurrence_key",
        &[("display_artist", "artist"), ("display_album", "album")],
    )
    .await?;
    snapshot_candidates(
        &mut transaction,
        "old_store",
        "listens",
        3,
        "0,old.started_at,old.listen_key",
        &[
            ("title", "track_title"),
            ("display_artist", "artist_name"),
            ("display_album", "album_title"),
        ],
    )
    .await?;
    if has_catalog {
        sqlx::query("INSERT INTO migration_metadata(media_uri,priority,present,stamp,tie,title,display_artist,display_album,duration_millis,disc_number,track_number) SELECT locator.media_uri,4,0,0,locator.local_access_file_key,metadata.title,metadata.artist,metadata.album,metadata.duration_millis,metadata.disc_number,metadata.track_number FROM old_store.local_locators locator JOIN old_catalog.local_access_metadata metadata USING(access_uri) WHERE locator.origin='download'")
            .execute(&mut *transaction).await?;
    }
    saved_candidates(&mut transaction, "old_store").await?;
    reduce_metadata(&mut transaction).await?;
    playlists(&mut transaction, has_catalog).await?;
    for table in [
        "smart_playlists",
        "favorite_outbox",
        "user_media_state",
        "listens",
        "listen_outbox",
        "legacy_activity",
        "local_locators",
        "connect_media_files",
    ] {
        copy_table(&mut transaction, "old_store", table, table, &[]).await?;
    }
    sqlx::query("UPDATE smart_playlists SET artwork_binding=CAST(json_object('kind','playlist','smart',json('true'),'source_id',NULL,'object_id',object_id,'revision',artwork_revision) AS BLOB) WHERE artwork_revision IS NOT NULL").execute(&mut *transaction).await?;
    let has_received_media = has_table(&mut transaction, "old_store", "connect_collection").await?;
    loop {
        let files:Vec<(String,String)>=sqlx::query_as("SELECT media_uri,encoding FROM connect_media_files WHERE backing_uri IS NULL LIMIT 128").fetch_all(&mut *transaction).await?;
        if files.is_empty() {
            break;
        }
        for (uri, encoding) in files {
            let received_backing = if has_received_media {
                sqlx::query_scalar::<_,Option<String>>("SELECT json_extract(payload,'$.backing_id') FROM old_store.connect_collection WHERE media_uri=?1")
                    .bind(&uri).fetch_optional(&mut *transaction).await?.flatten()
            } else {
                None
            };
            let backing = received_backing
                .or_else(|| crate::cue_media_parts(&uri).map(|(_, backing, _, _)| backing))
                .unwrap_or_else(|| uri.clone());
            sqlx::query(
                "UPDATE connect_media_files SET backing_uri=?1 WHERE media_uri=?2 AND encoding=?3",
            )
            .bind(backing)
            .bind(uri)
            .bind(encoding)
            .execute(&mut *transaction)
            .await?;
        }
    }
    let mut history_after = (String::new(), String::new());
    loop {
        let missing:Vec<(String,String)>=sqlx::query_as("SELECT DISTINCT history.source_id,history.track_object_id FROM legacy_activity history WHERE item_kind='track' AND (history.source_id,history.track_object_id)>(?1,?2) AND NOT EXISTS(SELECT 1 FROM tracks JOIN sources USING(source_key) WHERE sources.object_id=history.source_id AND tracks.object_id=history.track_object_id) ORDER BY history.source_id,history.track_object_id LIMIT 256")
            .bind(&history_after.0).bind(&history_after.1).fetch_all(&mut *transaction).await?;
        if missing.is_empty() {
            break;
        }
        for (source, object) in missing {
            let uri = crate::normalize_direct_media_uri(&object)
                .unwrap_or_else(|| source_entity_uri(&SourceId::new(&source), "track", &object));
            crate::catalog::remember_track(
                &mut transaction,
                &crate::TrackReference {
                    media_uri: uri,
                    ..Default::default()
                },
            )
            .await?;
            history_after = (source, object);
        }
    }
    restore_saved_queue(&mut transaction).await?;
    sqlx::query("UPDATE tracks SET local_play_count=(SELECT count(*) FROM listens WHERE listens.media_uri=tracks.media_uri)").execute(&mut *transaction).await?;
    copy_table(
        &mut transaction,
        "old_store",
        "connect_roots",
        "connect_roots",
        &[],
    )
    .await?;
    if has_table(&mut transaction, "old_store", "connect_collection").await? {
        let fields = [
            "source_id",
            "album_id",
            "root_id",
            "root_label",
            "relative_path",
            "revision",
            "local_media",
            "cue_start_millis",
            "cue_end_millis",
            "backing_id",
        ];
        let values = fields
            .iter()
            .map(|field| format!("'{field}',json_extract(payload,'$.{field}')"))
            .collect::<Vec<_>>()
            .join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO connect_collection(media_uri,payload) SELECT media_uri,json_object({values}) FROM old_store.connect_collection"))).execute(&mut *transaction).await?;
    }
    if has_table(&mut transaction, "old_store", "connect_changes").await? {
        sqlx::query("INSERT INTO connect_changes(sequence,kind,object_key,payload) SELECT sequence,kind,object_key,payload FROM old_store.connect_changes WHERE payload IS NULL AND kind<>'cache_lyrics'").execute(&mut *transaction).await?;
    }
    if has_table(&mut transaction, "old_store", "connect_capture").await? {
        sqlx::query("UPDATE connect_capture SET enabled=COALESCE((SELECT enabled FROM old_store.connect_capture WHERE singleton=1),0),applying=0 WHERE singleton=1").execute(&mut *transaction).await?;
    }
    super::normalize_stored_artwork(&mut transaction).await?;
    transaction.commit().await?;
    connection.close().await?;
    super::install(&pending, input, destination)?;
    Ok(())
}
