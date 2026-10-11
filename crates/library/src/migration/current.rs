//! One-time conversion of the former durable and catalog files.
use super::track_uri;
use crate::{LibraryError, LibraryResult, SourceId, source_entity_uri};
use sqlx::{Connection, Row, SqliteConnection};
use std::path::Path;

pub(super) async fn attach(
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
        copy_table(
            connection,
            schema,
            table,
            table,
            &[
                (
                    "source_key",
                    "(SELECT source_key FROM migration_source_keys WHERE old_key=old.source_key)",
                ),
                ("media_uri", &uri),
                ("local_play_count", "0"),
            ],
        )
        .await?;
        sqlx::query("DELETE FROM migration_uris")
            .execute(&mut *connection)
            .await?;
    }
    for table in [
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
        "home_entries",
        "loudness_measurements",
        "replay_gain_measurements",
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
    if released {
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO lyrics_cache SELECT track.media_uri,lyrics.authority,lyrics.role,lyrics.language,lyrics.script,lyrics.cache_input_digest,lyrics.lyrics,lyrics.updated_at FROM {schema}.lyrics_cache lyrics JOIN tracks track USING(track_key)")))
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
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET source_play_count=activity.play_count,source_skip_count=activity.skip_count,source_last_played=activity.last_played_at FROM {schema}.activity_baseline activity JOIN migration_source_keys mapping ON mapping.old_key=activity.source_key WHERE {table}.source_key=mapping.source_key AND {table}.object_id=activity.track_object_id AND activity.period='lifetime' AND activity.item_kind='{kind}'")))
                    .execute(&mut *connection).await?;
            }
        }
    }
    sqlx::query("UPDATE tracks SET album_display_artist=album.display_artist,musicbrainz_album_id=album.musicbrainz_release_id,musicbrainz_release_group_id=album.musicbrainz_release_group_id FROM albums album WHERE album.album_key=tracks.album_key")
        .execute(&mut *connection).await?;
    sqlx::query("UPDATE tracks SET primary_artist_musicbrainz_id=(SELECT artist.musicbrainz_artist_id FROM track_artists credit JOIN artists artist USING(artist_key) WHERE credit.track_key=tracks.track_key ORDER BY credit.position LIMIT 1)")
        .execute(&mut *connection).await?;
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
    sqlx::query("INSERT INTO tracks(media_uri,indexed,source_favorite,source_rating,first_seen_at,artwork_binding,comment,bpm,date_added) SELECT media_uri,0,COALESCE(json_extract(payload,'$.source_favorite'),0),json_extract(payload,'$.source_rating'),json_extract(payload,'$.first_seen_at'),unhex(json_extract(payload,'$.artwork_binding')),json_extract(payload,'$.comment'),json_extract(payload,'$.bpm'),json_extract(payload,'$.date_added') FROM old_store.connect_collection WHERE true ON CONFLICT(media_uri) DO NOTHING")
        .execute(&mut *connection).await?;
    sqlx::query("UPDATE tracks SET comment=COALESCE(tracks.comment,json_extract(remote.payload,'$.comment')),bpm=COALESCE(tracks.bpm,json_extract(remote.payload,'$.bpm')),date_added=COALESCE(tracks.date_added,json_extract(remote.payload,'$.date_added')),first_seen_at=COALESCE(tracks.first_seen_at,json_extract(remote.payload,'$.first_seen_at')),artwork_binding=COALESCE(tracks.artwork_binding,unhex(json_extract(remote.payload,'$.artwork_binding'))) FROM old_store.connect_collection remote WHERE tracks.media_uri=remote.media_uri")
        .execute(&mut *connection).await?;
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
        sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE tracks SET {field}=(SELECT value.{field} FROM migration_metadata value WHERE value.media_uri=tracks.media_uri AND value.{field} IS NOT NULL ORDER BY value.priority,value.present,value.stamp DESC,value.tie DESC LIMIT 1) WHERE {field} IS NULL AND EXISTS(SELECT 1 FROM migration_metadata value WHERE value.media_uri=tracks.media_uri AND value.{field} IS NOT NULL)")))
            .execute(&mut *connection).await?;
    }
    let mut after = 0_i64;
    loop {
        let rows:Vec<(i64,String,Option<String>,Option<String>,Option<String>,Option<String>,Option<i64>)> = sqlx::query_as("SELECT track_key,media_uri,title,display_artist,display_album,comment,year FROM tracks WHERE track_key>?1 ORDER BY track_key LIMIT 256")
            .bind(after).fetch_all(&mut *connection).await?;
        if rows.is_empty() {
            break;
        }
        for (key, uri, title, artist, album, comment, year) in rows {
            after = key;
            let search = format!(
                "{} {} {} {} {}",
                title.as_deref().unwrap_or(&uri),
                artist.as_deref().unwrap_or_default(),
                album.as_deref().unwrap_or_default(),
                comment.as_deref().unwrap_or_default(),
                year.map(|year| year.to_string()).unwrap_or_default()
            )
            .to_lowercase();
            sqlx::query("UPDATE tracks SET normalized_search=?2,sort_text=COALESCE(sort_text,?3),sort_album=COALESCE(sort_album,?4) WHERE track_key=?1")
                .bind(key).bind(search).bind(title.as_deref().unwrap_or(&uri).to_lowercase()).bind(album.as_deref().unwrap_or_default().to_lowercase()).execute(&mut *connection).await?;
            if let Some((source, kind, object)) = crate::source_entity_parts(&uri)
                && kind == "track"
            {
                let source_key =
                    crate::db::write_source_identity(connection, source.as_str()).await?;
                sqlx::query("UPDATE tracks SET source_key=?2,object_id=?3 WHERE track_key=?1 AND source_key IS NULL")
                    .bind(key).bind(source_key).bind(object).execute(&mut *connection).await?;
            }
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
    sqlx::raw_sql("CREATE TEMP TABLE migration_playlist_keys AS SELECT old.playlist_key old_key,COALESCE(owned.playlist_key,(SELECT COALESCE(max(playlist_key),0) FROM playlists)+row_number() OVER(ORDER BY old.playlist_key)) playlist_key FROM old_catalog.native_playlists old JOIN migration_source_keys source ON source.old_key=old.source_key LEFT JOIN playlists owned ON owned.source_key=source.source_key AND owned.object_id=old.object_id;
            INSERT INTO playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,owner,position,artwork_binding,writable,metadata_writable,provider_revision,valid_until)
            SELECT mapping.playlist_key,source.source_key,old.object_id,old.name,old.normalized_name,old.sort_text,'source',COALESCE(owned.position,(SELECT COALESCE(max(position),-1) FROM playlists)+row_number() OVER(ORDER BY old.playlist_key)),old.artwork_binding,old.writable,old.metadata_writable,old.provider_revision,old.valid_until
            FROM old_catalog.native_playlists old JOIN migration_playlist_keys mapping ON mapping.old_key=old.playlist_key JOIN migration_source_keys source ON source.old_key=old.source_key LEFT JOIN playlists owned ON owned.playlist_key=mapping.playlist_key WHERE true
            ON CONFLICT(playlist_key) DO UPDATE SET owner='source',indexed=1,name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text,artwork_binding=COALESCE(playlists.artwork_binding,excluded.artwork_binding),writable=excluded.writable,metadata_writable=excluded.metadata_writable,provider_revision=excluded.provider_revision,valid_until=excluded.valid_until;")
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
    if !has_table(connection, "old_store", "queue_saved").await? {
        return Ok(());
    }
    let saved = sqlx::query_scalar::<_, String>(
        "SELECT state FROM old_store.queue_saved WHERE singleton=1",
    )
    .fetch_optional(&mut *connection)
    .await?;
    let Some(saved) = saved else {
        return Ok(());
    };
    let mut state =
        crate::queue::decode_saved_queue(connection, serde_json::from_str(&saved)?).await?;
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
    if let Some((current,progress,repeat,shuffled))=sqlx::query_as::<_,(Option<i64>,i64,String,bool)>("SELECT current_index,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(&mut *connection).await? {
        if columns(connection,"old_store","queue_state").await?.iter().any(|column|column=="current_index") {
            state.current_index=current.map(|index|index as usize);
        }
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
    if has_table(&mut transaction, "old_store", "queue_entries").await? {
        sqlx::query("INSERT INTO tracks(media_uri,indexed) SELECT DISTINCT media_uri,0 FROM old_store.queue_entries WHERE true ON CONFLICT(media_uri) DO NOTHING")
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
        "queue_contexts",
        "queue_positions",
        "queue_state",
        "queue_transfer_pages",
    ] {
        copy_table(&mut transaction, "old_store", table, table, &[]).await?;
    }
    copy_table(
        &mut transaction,
        "old_store",
        "queue_entries",
        "queue_entries",
        &[(
            "track_key",
            "(SELECT track_key FROM tracks WHERE media_uri=old.media_uri)",
        )],
    )
    .await?;
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
            crate::tracks::remember_song(&mut transaction, &uri, Default::default()).await?;
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
        sqlx::query("INSERT INTO connect_changes(sequence,kind,object_key,payload) SELECT sequence,kind,object_key,payload FROM old_store.connect_changes WHERE payload IS NULL").execute(&mut *transaction).await?;
    }
    if has_table(&mut transaction, "old_store", "connect_capture").await? {
        sqlx::query("UPDATE connect_capture SET enabled=COALESCE((SELECT enabled FROM old_store.connect_capture WHERE singleton=1),0),applying=0 WHERE singleton=1").execute(&mut *transaction).await?;
    }
    crate::collections::refresh_facts_on(&mut transaction).await?;
    transaction.commit().await?;
    connection.close().await?;
    super::install(&pending, input, destination)?;
    Ok(())
}
