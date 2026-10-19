//! Shared file facts and accepted parser output at the profile boundary.
use super::*;
use crate::{CatalogFile, catalog::*};
use serde_json::json;

#[derive(Serialize, Deserialize)]
struct FileOutput {
    #[serde(flatten)]
    index: CatalogFile,
    tracks: Vec<TrackObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    playlist_identity: Option<String>,
}

pub(crate) async fn mark_files_on(
    connection: &mut SqliteConnection,
    source: i64,
    paths: &[&str],
    deleted: bool,
) -> LibraryResult<()> {
    for page in paths.chunks(CONNECT_PAGE_SIZE) {
        let keys: Vec<String> = sqlx::query_scalar("SELECT json_array(source.object_id,file.path) FROM json_each(?2) input JOIN local_files file ON file.source_key=?1 AND file.path=input.value JOIN sources source USING(source_key)")
            .bind(source).bind(serde_json::to_string(page)?).fetch_all(&mut *connection).await?;
        mark_current_many_on(
            connection,
            "catalog:file",
            &keys.iter().map(String::as_str).collect::<Vec<_>>(),
            deleted,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn mark_artwork_on(
    connection: &mut SqliteConnection,
    source: i64,
    kind: &str,
    ids: &[&str],
) -> LibraryResult<()> {
    for page in ids.chunks(CONNECT_PAGE_SIZE) {
        let keys: Vec<String> = sqlx::query_scalar("SELECT json_array(source.object_id,?2,input.value) FROM sources source CROSS JOIN json_each(?3) input WHERE source.source_key=?1 AND source.kind IN('local','webdav','smb')")
            .bind(source).bind(kind).bind(serde_json::to_string(page)?).fetch_all(&mut *connection).await?;
        mark_current_many_on(
            connection,
            "catalog:artwork",
            &keys.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        )
        .await?;
    }
    Ok(())
}

fn file_object(alias: &str) -> String {
    format!(
        "json_object('path',{alias}.path,'root',{alias}.root,'relative_path',replace({alias}.relative_path,char(92),'/'),'kind',{alias}.kind,'size_bytes',{alias}.size_bytes,'mtime_ns',{alias}.mtime_ns,'device_id',NULL,'inode',NULL,'native_id',NULL,'revision',{alias}.revision,'picture_index',{alias}.picture_index,'artist_pictures',{alias}.artist_pictures,'parse_version',{alias}.parse_version,'state',{alias}.state)"
    )
}

pub(super) async fn current_on(
    connection: &mut SqliteConnection,
    kind: &str,
    key: &str,
) -> LibraryResult<Option<Value>> {
    if kind == "catalog:source" {
        let value: Option<String> = sqlx::query_scalar("SELECT json_object('kind',kind,'name',display_name,'complete',json(CASE catalog_complete WHEN 1 THEN 'true' ELSE 'false' END),'freshness',CASE WHEN freshness IS NULL THEN NULL ELSE hex(freshness) END) FROM sources WHERE object_id=json_extract(?1,'$[0]') AND kind IS NOT NULL")
            .bind(key).fetch_optional(&mut *connection).await?;
        let Some(value) = value else { return Ok(None) };
        let mut value: Value = serde_json::from_str(&value)?;
        let (source, _): (String, String) = serde_json::from_str(key)?;
        value["roots"] = serde_json::to_value(read_roots(connection, &source).await?)?;
        return Ok(Some(value));
    }
    if kind == "catalog:artwork" {
        let (source, kind, id): (String, String, String) = serde_json::from_str(key)?;
        let (table, _) = crate::catalog::cache::table(&kind);
        let binding: Option<Option<Vec<u8>>> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT native_artwork FROM {table} WHERE source_key=(SELECT source_key FROM sources WHERE object_id=?1) AND object_id=?2")))
            .bind(source).bind(id).fetch_optional(connection).await?;
        return Ok(binding.map(|binding| json!({"artwork_binding":binding})));
    }
    let value:Option<(i64,i64,String,Option<String>)>=sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT file.source_key,file.local_file_key,json_object('file',json({}),'root_id',file.root_id,'parsed',CASE WHEN file.parsed_stamp IS NULL THEN NULL ELSE json_array(json_object('size_bytes',json_extract(file.parsed_stamp,'$[0]'),'mtime_ns',json_extract(file.parsed_stamp,'$[1]'),'revision',json_extract(file.parsed_stamp,'$[2]')),json_extract(file.parsed_stamp,'$[3]')) END,'directory_complete',json(CASE WHEN json_extract(file.parsed_stamp,'$[4]')=file.observed_stamp THEN 'true' ELSE 'false' END),'dependencies',json((SELECT json_group_array(json_object('path',edge.dependency_path,'accepted',CASE WHEN edge.accepted_inputs IS NULL THEN NULL ELSE json_object('size_bytes',json_extract(edge.accepted_inputs,'$[0]'),'mtime_ns',json_extract(edge.accepted_inputs,'$[1]'),'revision',json_extract(edge.accepted_inputs,'$[2]')) END,'root_id',dependency.root_id,'file',CASE WHEN dependency.path IS NULL THEN NULL ELSE json({}) END) ORDER BY edge.position) FROM local_file_dependencies edge LEFT JOIN local_files dependency ON dependency.source_key=file.source_key AND dependency.path=edge.dependency_path WHERE edge.local_file_key=file.local_file_key))),file.playlist_identity FROM local_files file JOIN sources source USING(source_key) WHERE source.object_id=json_extract(?1,'$[0]') AND file.path=json_extract(?1,'$[1]')",
        file_object("file"),file_object("dependency")
    ))).bind(key).fetch_optional(&mut *connection).await?;
    let Some((source, file, value, playlist_identity)) = value else {
        return Ok(None);
    };
    let index = serde_json::from_str(&value)?;
    let mut tracks = Vec::new();
    let mut after = 0;
    loop {
        let page =
            crate::catalog::file_reads::accepted_file_output_on(connection, source, file, after)
                .await?;
        let Some(last) = page.last() else { break };
        after = last.0;
        tracks.extend(page.into_iter().map(|(_, track)| track));
    }
    Ok(Some(serde_json::to_value(FileOutput {
        index,
        tracks,
        playlist_identity,
    })?))
}

impl Database {
    pub(crate) async fn apply_current_catalog_on(
        connection: &mut SqliteConnection,
        records: &[ConnectRecord],
    ) -> LibraryResult<bool> {
        let mut update = CatalogUpdate::default();
        for record in records {
            let key: Vec<String> = serde_json::from_str(&record.key)?;
            let source_id = &key[0];
            let source = crate::db::write_source_identity(connection, source_id).await?;
            let observed: i64 =
                sqlx::query_scalar("SELECT observation_stamp FROM sources WHERE source_key=?1")
                    .bind(source)
                    .fetch_one(&mut *connection)
                    .await?;
            match (record.kind.as_str(), &record.value) {
                ("catalog:source", Some(value)) => {
                    update.metadata |= sqlx::query("UPDATE sources SET kind=?2,display_name=?3,normalized_name=lower(?3),catalog_complete=?4,freshness=?5 WHERE source_key=?1 AND(kind,display_name,catalog_complete,freshness) IS NOT(?2,?3,?4,?5)")
                        .bind(source).bind(value["kind"].as_str()).bind(value["name"].as_str().unwrap_or_default()).bind(value["complete"].as_bool().unwrap_or_default()).bind(hex_bytes(&value["freshness"])).execute(&mut *connection).await?.rows_affected()>0;
                    let roots: Vec<ConnectRoot> = serde_json::from_value(
                        value.get("roots").cloned().unwrap_or_else(|| json!([])),
                    )?;
                    let roots=roots.into_iter().map(|root|json!({"source_id":source_id,"root_id":root.id,"root_label":root.label})).collect::<Vec<_>>();
                    remember_roots(connection, &roots.iter().collect::<Vec<_>>()).await?;
                }
                ("catalog:file", Some(value)) => {
                    let output: FileOutput = serde_json::from_value(value.clone())?;
                    let index = output.index;
                    remember_roots(connection,&[&json!({"source_id":source_id,"root_id":index.root_id,"_local_root":index.file.root})]).await?;
                    update.metadata |= crate::catalog::files::import_index_on(
                        connection,
                        source,
                        Some(observed),
                        std::slice::from_ref(&index),
                        false,
                    )
                    .await?;
                    if let Some(identity) = output.playlist_identity {
                        update.metadata |= sqlx::query("UPDATE local_files SET playlist_identity=?3 WHERE source_key=?1 AND path=?2 AND playlist_identity IS NOT ?3")
                            .bind(source).bind(&index.file.path).bind(identity).execute(&mut *connection).await?.rows_affected()>0;
                    }
                    let ids = output
                        .tracks
                        .iter()
                        .map(|track| &track.object_id)
                        .collect::<Vec<_>>();
                    loop {
                        let withdrawn:Vec<String>=sqlx::query_scalar("SELECT object_id FROM tracks track WHERE local_file_key=(SELECT local_file_key FROM local_files WHERE source_key=?1 AND path=?2) AND indexed=1 AND object_id NOT IN(SELECT value FROM json_each(?3)) AND NOT EXISTS(SELECT 1 FROM local_file_observations access WHERE access.local_file_key=track.local_file_key) LIMIT 128")
                            .bind(source).bind(&index.file.path).bind(serde_json::to_string(&ids)?).fetch_all(&mut *connection).await?;
                        if withdrawn.is_empty() {
                            break;
                        }
                        update.merge(
                            crate::catalog::collections::remove_items_on(
                                connection,
                                source,
                                "track",
                                &withdrawn,
                                Some(observed),
                                true,
                            )
                            .await?,
                        );
                    }
                    let files = vec![(
                        index.file,
                        index
                            .dependencies
                            .into_iter()
                            .map(|dependency| dependency.path)
                            .collect(),
                    )];
                    update.merge(
                        crate::catalog::files::write_file_observations_on(
                            connection,
                            (source, source_id),
                            Some(observed),
                            &files,
                            output.tracks,
                            true,
                            false,
                        )
                        .await?,
                    );
                }
                ("catalog:file", None) => {
                    let path = &key[1];
                    loop {
                        let ids:Vec<String>=sqlx::query_scalar("SELECT object_id FROM tracks track WHERE local_file_key=(SELECT local_file_key FROM local_files WHERE source_key=?1 AND path=?2) AND indexed=1 AND NOT EXISTS(SELECT 1 FROM local_file_observations access WHERE access.local_file_key=track.local_file_key) LIMIT 128")
                            .bind(source).bind(path).fetch_all(&mut *connection).await?;
                        if ids.is_empty() {
                            break;
                        }
                        update.merge(
                            crate::catalog::collections::remove_items_on(
                                connection,
                                source,
                                "track",
                                &ids,
                                Some(observed),
                                true,
                            )
                            .await?,
                        );
                    }
                    let keys:Vec<i64>=sqlx::query_scalar("SELECT local_file_key FROM local_files file WHERE source_key=?1 AND path=?2 AND NOT EXISTS(SELECT 1 FROM local_file_observations access WHERE access.local_file_key=file.local_file_key)")
                        .bind(source).bind(path).fetch_all(&mut *connection).await?;
                    if !keys.is_empty() {
                        crate::catalog::files::delete_physical_files(
                            connection, source, &keys, observed,
                        )
                        .await?;
                        update.metadata = true;
                    }
                }
                ("catalog:artwork", Some(value)) => {
                    let binding: Option<Vec<u8>> =
                        serde_json::from_value(value["artwork_binding"].clone())?;
                    update.artwork |= crate::catalog::artwork::write_images(
                        connection,
                        source,
                        &key[1],
                        &[(&key[2], binding.as_deref())],
                        Some(observed),
                        true,
                    )
                    .await?;
                    if update.artwork {
                        let (table, column) = crate::catalog::cache::table(&key[1]);
                        let keys: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                            "SELECT {column} FROM {table} WHERE source_key=?1 AND object_id=?2"
                        )))
                        .bind(source)
                        .bind(&key[2])
                        .fetch_all(&mut *connection)
                        .await?;
                        crate::catalog::artwork::refresh_choices(
                            connection,
                            if key[1] == "track" { &keys } else { &[] },
                            if key[1] == "album" { &keys } else { &[] },
                            if key[1] == "artist" { &keys } else { &[] },
                            false,
                        )
                        .await?;
                    }
                }
                _ => {}
            }
        }
        Ok(update.changed())
    }
}
