use super::*;
use crate::{LocalFileState, LocalFileWrite, LocalLocatorWrite, TrackKey, TrackObservation};
use sqlx::{QueryBuilder, Sqlite};

pub(crate) fn directory_locator(path: &str) -> String {
    path.trim_end_matches(['/', std::path::MAIN_SEPARATOR])
        .to_owned()
}

pub(crate) fn parent_locator(path: &str) -> String {
    if let Ok(mut uri) = url::Url::parse(path)
        && uri.path_segments().is_some()
    {
        uri.set_fragment(None);
        if let Ok(mut segments) = uri.path_segments_mut() {
            segments.pop_if_empty().pop();
        }
        return directory_locator(uri.as_str());
    }
    std::path::Path::new(path)
        .parent()
        .map(|parent| directory_locator(&parent.to_string_lossy()))
        .unwrap_or_default()
}

async fn observe_files(
    connection: &mut SqliteConnection,
    source: i64,
    files: &[(LocalFileWrite, Vec<String>)],
    observed: Option<i64>,
) -> LibraryResult<Vec<(LocalFileWrite, Vec<String>)>> {
    let mut roots = std::collections::BTreeMap::<String, Vec<(String, String)>>::new();
    for root in files
        .iter()
        .map(|(file, _)| file.root.as_str())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let mut shared:Vec<(String,String)>=sqlx::query_as("SELECT access.root_id,coalesce((SELECT root FROM local_files file WHERE file.source_key=access.source_key AND file.root_id=access.root_id LIMIT 1),(SELECT shared.root FROM connect_roots shared JOIN sources source ON source.object_id=shared.source_id WHERE source.source_key=access.source_key AND shared.id=access.root_id),access.root) FROM local_file_access_roots access WHERE source_key=?1 AND access.root=?2")
            .bind(source).bind(root).fetch_all(&mut *connection).await?;
        if shared.is_empty() {
            let id = blake3::hash(root.as_bytes()).to_hex().to_string();
            sqlx::query("INSERT OR IGNORE INTO local_file_access_roots(source_key,root,root_id) VALUES(?1,?2,?3)")
                .bind(source).bind(root).bind(&id).execute(&mut *connection).await?;
            sqlx::query("INSERT OR IGNORE INTO connect_roots(source_id,id,label,root) SELECT object_id,?3,'',?2 FROM sources WHERE source_key=?1")
                .bind(source).bind(root).bind(&id).execute(&mut *connection).await?;
            shared.push((id, root.to_owned()));
        }
        roots.insert(root.to_owned(), shared);
    }
    let input = serde_json::to_string(
        &files
            .iter()
            .map(|(file, deps)| (file, deps, parent_locator(&file.path)))
            .collect::<Vec<_>>(),
    )?;
    let rows:Vec<(String,String,String,Option<String>,bool)>=sqlx::query_as("WITH requested AS MATERIALIZED(SELECT json_extract(value,'$[0]') file FROM json_each(?2)),candidates(physical,local_file_key) AS(SELECT json_extract(input.file,'$.path'),access.local_file_key FROM requested input CROSS JOIN local_file_observations access ON access.path=json_extract(input.file,'$.path') JOIN local_files canonical ON canonical.local_file_key=access.local_file_key AND canonical.source_key=?1 UNION SELECT json_extract(input.file,'$.path'),canonical.local_file_key FROM requested input CROSS JOIN local_files canonical ON canonical.source_key=?1 AND canonical.path=json_extract(input.file,'$.path') UNION SELECT json_extract(input.file,'$.path'),canonical.local_file_key FROM requested input CROSS JOIN local_file_access_roots root ON root.source_key=?1 AND root.root=json_extract(input.file,'$.root') CROSS JOIN local_files canonical ON canonical.source_key=?1 AND canonical.root_id=root.root_id AND canonical.relative_path=json_extract(input.file,'$.relative_path')) SELECT candidate.physical,json_object('path',canonical.path,'root',canonical.root,'relative_path',canonical.relative_path,'kind',canonical.kind,'size_bytes',canonical.size_bytes,'mtime_ns',canonical.mtime_ns,'device_id',NULL,'inode',NULL,'native_id',NULL,'revision',canonical.revision,'picture_index',canonical.picture_index,'artist_pictures',canonical.artist_pictures,'parse_version',canonical.parse_version,'state',canonical.state),canonical.root_id,CASE WHEN access.local_file_key IS NOT NULL THEN json_array(access.size_bytes,access.mtime_ns,access.revision) END,access.local_file_key IS NOT NULL AND access.accepted_facts IS canonical.parsed_stamp FROM candidates candidate JOIN local_files canonical USING(local_file_key) LEFT JOIN local_file_observations access USING(local_file_key)")
        .bind(source).bind(&input).fetch_all(&mut *connection).await?;
    let mut identities = std::collections::BTreeMap::<
        String,
        Vec<(LocalFileWrite, String, Option<String>, bool)>,
    >::new();
    for (physical, file, id, stamp, accepted) in rows {
        identities.entry(physical).or_default().push((
            serde_json::from_str(&file)?,
            id,
            stamp,
            accepted,
        ));
    }
    let mut canonical = Vec::new();
    let mut bindings = Vec::new();
    for (actual, dependencies) in files {
        let entries = identities.get(&actual.path).cloned().unwrap_or_else(|| {
            roots[&actual.root]
                .iter()
                .map(|(id, root)| {
                    let mut file = actual.clone();
                    file.root = root.clone();
                    if root != &actual.root {
                        let separator = if root.contains('\\') { '\\' } else { '/' };
                        file.path = format!(
                            "{}{separator}{}",
                            root.trim_end_matches(['/', '\\']),
                            actual.relative_path.replace('/', &separator.to_string())
                        );
                    }
                    (file, id.clone(), None, false)
                })
                .collect()
        });
        for (previous, id, stamp, accepted) in entries {
            let mut file = actual.clone();
            file.path = previous.path;
            file.root = previous.root;
            file.relative_path = previous.relative_path;
            file.device_id = None;
            file.inode = None;
            file.native_id = None;
            if accepted
                && stamp.as_deref()
                    == Some(
                        serde_json::json!([actual.size_bytes, actual.mtime_ns, actual.revision])
                            .to_string()
                            .as_str(),
                    )
            {
                file.size_bytes = previous.size_bytes;
                file.mtime_ns = previous.mtime_ns;
                file.revision = previous.revision;
            }
            let deps = dependencies
                .iter()
                .map(|path| {
                    identities
                        .get(path)
                        .and_then(|entries| {
                            entries
                                .iter()
                                .find(|entry| entry.1 == id)
                                .or_else(|| entries.first())
                        })
                        .map_or_else(|| path.clone(), |entry| entry.0.path.clone())
                })
                .collect();
            bindings.push((
                &actual.path,
                file.path.clone(),
                file.root.clone(),
                file.relative_path.clone(),
                id,
            ));
            canonical.push((file, deps));
        }
    }
    let mapping = serde_json::to_string(&bindings)?;
    sqlx::query("INSERT INTO local_files(source_key,path,root,root_id,relative_path,kind,size_bytes,mtime_ns,state) SELECT ?1,json_extract(value,'$[1]'),json_extract(value,'$[2]'),json_extract(value,'$[4]'),json_extract(value,'$[3]'),'media',NULL,0,'observed' FROM json_each(?2) WHERE true ON CONFLICT(source_key,path) DO NOTHING")
        .bind(source).bind(&mapping).execute(&mut *connection).await?;
    sqlx::query("WITH input AS MATERIALIZED(SELECT json_extract(value,'$[0]') file,json_extract(value,'$[2]') parent FROM json_each(?2)),binding AS MATERIALIZED(SELECT json_extract(value,'$[0]') physical,json_extract(value,'$[1]') canonical FROM json_each(?3)),observations AS(
        SELECT input.*,binding.physical,canonical.local_file_key,canonical.parsed_stamp facts,canonical.state accepted_state,
            json_extract(input.file,'$.state')='observed' AND canonical.kind<>'directory' AND canonical.state IN('accepted','rejected') AND
            json_array(json_extract(input.file,'$.size_bytes'),json_extract(input.file,'$.mtime_ns'),json_extract(input.file,'$.revision'))=json_array(json_extract(canonical.parsed_stamp,'$[0]'),json_extract(canonical.parsed_stamp,'$[1]'),json_extract(canonical.parsed_stamp,'$[2]')) reused
        FROM input JOIN binding ON binding.physical=json_extract(input.file,'$.path') CROSS JOIN local_files canonical ON canonical.source_key=?1 AND canonical.path=binding.canonical)
        INSERT INTO local_file_observations(local_file_key,path,root,relative_path,size_bytes,mtime_ns,device_id,inode,native_id,revision,observed_stamp,parent_path,parsed_stamp,accepted_facts,state)
        SELECT local_file_key,physical,json_extract(file,'$.root'),json_extract(file,'$.relative_path'),json_extract(file,'$.size_bytes'),json_extract(file,'$.mtime_ns'),json_extract(file,'$.device_id'),json_extract(file,'$.inode'),json_extract(file,'$.native_id'),json_extract(file,'$.revision'),coalesce(?4,0),parent,
            CASE WHEN reused THEN facts WHEN json_extract(file,'$.state') IN('accepted','rejected') THEN CASE WHEN json_extract(file,'$.kind')='directory' THEN json_array(json_extract(file,'$.size_bytes'),json_extract(file,'$.mtime_ns'),json_extract(file,'$.revision'),json_extract(file,'$.parse_version'),coalesce(?4,0)) ELSE json_array(json_extract(file,'$.size_bytes'),json_extract(file,'$.mtime_ns'),json_extract(file,'$.revision'),json_extract(file,'$.parse_version')) END END,
            CASE WHEN reused THEN facts END,CASE WHEN reused THEN accepted_state ELSE json_extract(file,'$.state') END FROM observations WHERE true
        ON CONFLICT(local_file_key) DO UPDATE SET path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,observed_stamp=CASE WHEN excluded.observed_stamp=0 THEN local_file_observations.observed_stamp ELSE excluded.observed_stamp END,parent_path=excluded.parent_path,parsed_stamp=coalesce(excluded.parsed_stamp,local_file_observations.parsed_stamp),accepted_facts=coalesce(excluded.accepted_facts,local_file_observations.accepted_facts),state=CASE WHEN excluded.state='observed' AND(local_file_observations.size_bytes,local_file_observations.mtime_ns,local_file_observations.revision) IS(excluded.size_bytes,excluded.mtime_ns,excluded.revision) THEN local_file_observations.state ELSE excluded.state END WHERE excluded.observed_stamp>=local_file_observations.observed_stamp")
        .bind(source).bind(input).bind(mapping).bind(observed).execute(&mut *connection).await?;
    Ok(canonical)
}

pub(crate) async fn physical(
    connection: &mut SqliteConnection,
    source: i64,
    files: &[(LocalFileWrite, Vec<String>)],
    observed: Option<i64>,
    accessible: bool,
) -> LibraryResult<bool> {
    let mut changed = false;
    for page in files.chunks(128) {
        if page.is_empty() {
            continue;
        }
        let canonical;
        let page = if accessible {
            canonical = observe_files(connection, source, page, observed).await?;
            canonical.as_slice()
        } else {
            page
        };
        let input = serde_json::to_string(page)?;
        let same = "(old.size_bytes,old.mtime_ns,old.revision) IS(json_extract(input.file,'$.size_bytes'),json_extract(input.file,'$.mtime_ns'),json_extract(input.file,'$.revision'))";
        let mut changed_paths:std::collections::BTreeSet<String>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("WITH input AS(SELECT json_extract(value,'$[0]') file FROM json_each(?2)) SELECT json_extract(input.file,'$.path') FROM input LEFT JOIN local_files old ON old.source_key=?1 AND old.path=json_extract(input.file,'$.path') WHERE coalesce(old.observed_stamp,0)<=coalesce(?3,0) AND(old.path IS NULL OR(old.root,old.relative_path,old.kind,old.size_bytes,old.mtime_ns,old.device_id,old.inode,old.native_id,old.revision,old.picture_index,old.artist_pictures,old.parse_version,old.state) IS NOT(json_extract(input.file,'$.root'),json_extract(input.file,'$.relative_path'),json_extract(input.file,'$.kind'),json_extract(input.file,'$.size_bytes'),json_extract(input.file,'$.mtime_ns'),json_extract(input.file,'$.device_id'),json_extract(input.file,'$.inode'),json_extract(input.file,'$.native_id'),json_extract(input.file,'$.revision'),CASE WHEN json_extract(input.file,'$.artist_pictures') IS NULL AND {same} THEN old.picture_index ELSE json_extract(input.file,'$.picture_index') END,CASE WHEN {same} THEN coalesce(json_extract(input.file,'$.artist_pictures'),old.artist_pictures) ELSE json_extract(input.file,'$.artist_pictures') END,CASE WHEN json_extract(input.file,'$.state')='observed' AND {same} THEN old.parse_version ELSE json_extract(input.file,'$.parse_version') END,CASE WHEN json_extract(input.file,'$.state')='observed' AND {same} THEN old.state ELSE json_extract(input.file,'$.state') END) OR(json_extract(input.file,'$.state') IN('accepted','rejected') AND(json_remove(old.parsed_stamp,'$[4]') IS NOT json_array(json_extract(input.file,'$.size_bytes'),json_extract(input.file,'$.mtime_ns'),json_extract(input.file,'$.revision'),json_extract(input.file,'$.parse_version')) OR(old.kind='directory' AND json_extract(old.parsed_stamp,'$[4]') IS NOT old.observed_stamp))))")))
            .bind(source).bind(input).bind(observed).fetch_all(&mut *connection).await?.into_iter().collect();
        changed |= !changed_paths.is_empty()
            && page
                .iter()
                .any(|(file, _)| file.kind == crate::LocalFileKind::Directory);
        let mut query = QueryBuilder::<Sqlite>::new(
            "INSERT INTO local_files(source_key,path,root,relative_path,parent_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state,observed_stamp,parsed_stamp) ",
        );
        query.push_values(page, |mut row, (file, _)| {
            let parent = parent_locator(&file.path);
            let parsed = matches!(
                file.state,
                LocalFileState::Accepted | LocalFileState::Rejected
            )
            .then(|| {
                let mut parsed = serde_json::json!([
                    file.size_bytes,
                    file.mtime_ns,
                    file.revision,
                    file.parse_version
                ]);
                if file.kind == crate::LocalFileKind::Directory {
                    parsed
                        .as_array_mut()
                        .unwrap()
                        .push(observed.unwrap_or(0).into());
                }
                serde_json::to_string(&parsed).unwrap()
            });
            row.push_bind(source)
                .push_bind(&file.path)
                .push_bind(&file.root)
                .push_bind(&file.relative_path)
                .push_bind(parent)
                .push_bind(file.kind)
                .push_bind(file.size_bytes)
                .push_bind(file.mtime_ns)
                .push_bind(file.device_id)
                .push_bind(file.inode)
                .push_bind(&file.native_id)
                .push_bind(&file.revision)
                .push_bind(file.picture_index)
                .push_bind(&file.artist_pictures)
                .push_bind(file.parse_version)
                .push_bind(file.state)
                .push_bind(observed.unwrap_or(0))
                .push_bind(parsed);
        });
        query.push(" ON CONFLICT(source_key,path) DO UPDATE SET root=excluded.root,relative_path=excluded.relative_path,parent_path=excluded.parent_path,kind=excluded.kind,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,observed_stamp=CASE WHEN excluded.observed_stamp=0 OR(local_files.kind='directory' AND excluded.kind='directory' AND excluded.state='observed' AND(local_files.size_bytes,local_files.mtime_ns,local_files.revision) IS(excluded.size_bytes,excluded.mtime_ns,excluded.revision)) THEN local_files.observed_stamp ELSE excluded.observed_stamp END,picture_index=CASE WHEN excluded.artist_pictures IS NULL AND(local_files.size_bytes,local_files.mtime_ns,local_files.revision) IS(excluded.size_bytes,excluded.mtime_ns,excluded.revision) THEN local_files.picture_index ELSE excluded.picture_index END,artist_pictures=CASE WHEN(local_files.size_bytes,local_files.mtime_ns,local_files.revision) IS(excluded.size_bytes,excluded.mtime_ns,excluded.revision) THEN coalesce(excluded.artist_pictures,local_files.artist_pictures) ELSE excluded.artist_pictures END,parse_version=CASE WHEN excluded.state='observed' AND(local_files.size_bytes,local_files.mtime_ns,local_files.revision) IS(excluded.size_bytes,excluded.mtime_ns,excluded.revision) THEN local_files.parse_version ELSE excluded.parse_version END,state=CASE WHEN excluded.state='observed' AND(local_files.size_bytes,local_files.mtime_ns,local_files.revision) IS(excluded.size_bytes,excluded.mtime_ns,excluded.revision) THEN local_files.state ELSE excluded.state END,parsed_stamp=coalesce(excluded.parsed_stamp,local_files.parsed_stamp) WHERE excluded.observed_stamp>=local_files.observed_stamp");
        query.build().execute(&mut *connection).await?;
        let parsed = page
            .iter()
            .filter(|(file, dependencies)| {
                !dependencies.is_empty()
                    || matches!(
                        file.state,
                        LocalFileState::Accepted | LocalFileState::Rejected
                    )
            })
            .map(|(file, dependencies)| {
                (
                    &file.path,
                    dependencies,
                    file.state == LocalFileState::Accepted,
                )
            })
            .collect::<Vec<_>>();
        if accessible {
            sqlx::query("UPDATE local_file_observations SET accepted_facts=file.parsed_stamp FROM local_files file WHERE file.local_file_key=local_file_observations.local_file_key AND file.source_key=?1 AND file.path IN(SELECT json_extract(value,'$[0].path') FROM json_each(?2) WHERE json_extract(value,'$[0].state') IN('accepted','rejected'))")
                .bind(source).bind(serde_json::to_string(page)?).execute(&mut *connection).await?;
        }
        if parsed.is_empty() {
            crate::connect::catalog::mark_files_on(
                connection,
                source,
                &changed_paths.iter().map(String::as_str).collect::<Vec<_>>(),
                false,
            )
            .await?;
            continue;
        }
        let input = serde_json::to_string(&parsed)?;
        let expected = "WITH owners AS(SELECT json_extract(value,'$[0]') path,json_extract(value,'$[1]') dependencies,json_extract(value,'$[2]') accepted FROM json_each(?2)),expected AS(SELECT file.local_file_key,item.value dependency_path,CAST(item.key AS INTEGER) position FROM owners CROSS JOIN local_files file ON file.source_key=?1 AND file.path=owners.path AND file.observed_stamp<=?3 CROSS JOIN json_each(owners.dependencies) item) ";
        let mut dependency_keys:Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("{expected} DELETE FROM local_file_dependencies AS current WHERE local_file_key IN(SELECT file.local_file_key FROM owners CROSS JOIN local_files file ON file.source_key=?1 AND file.path=owners.path WHERE file.observed_stamp<=?3) AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.local_file_key=current.local_file_key AND expected.dependency_path=current.dependency_path AND expected.position=current.position) RETURNING local_file_key"))).bind(source).bind(&input).bind(observed).fetch_all(&mut *connection).await?;
        dependency_keys.extend(sqlx::query_scalar::<_,i64>(sqlx::AssertSqlSafe(format!("{expected} INSERT OR IGNORE INTO local_file_dependencies(local_file_key,dependency_path,position) SELECT local_file_key,dependency_path,position FROM expected RETURNING local_file_key"))).bind(source).bind(&input).bind(observed).fetch_all(&mut *connection).await?);
        dependency_keys.extend(sqlx::query_scalar::<_,i64>("WITH accepted AS MATERIALIZED(SELECT edge.local_file_key,edge.dependency_path,json_array(dependency.size_bytes,dependency.mtime_ns,dependency.revision) inputs FROM json_each(?2) input CROSS JOIN local_files owner ON owner.source_key=?1 AND owner.path=json_extract(input.value,'$[0]') CROSS JOIN local_file_dependencies edge ON edge.local_file_key=owner.local_file_key CROSS JOIN local_files dependency ON dependency.source_key=?1 AND dependency.path=edge.dependency_path WHERE json_extract(input.value,'$[2]') AND owner.kind='cue' AND owner.observed_stamp<=?3 AND dependency.observed_stamp<=?3) UPDATE local_file_dependencies AS edge SET accepted_inputs=accepted.inputs FROM accepted WHERE edge.local_file_key=accepted.local_file_key AND edge.dependency_path=accepted.dependency_path AND edge.accepted_inputs IS NOT accepted.inputs RETURNING local_file_key")
            .bind(source).bind(&input).bind(observed).fetch_all(&mut *connection).await?);
        if !dependency_keys.is_empty() {
            changed_paths.extend(sqlx::query_scalar::<_,String>("SELECT path FROM local_files WHERE local_file_key IN(SELECT value FROM json_each(?1))").bind(serde_json::to_string(&dependency_keys)?).fetch_all(&mut *connection).await?);
        }
        crate::connect::catalog::mark_files_on(
            connection,
            source,
            &changed_paths.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        )
        .await?;
    }
    Ok(changed)
}

pub(crate) async fn import_index_on(
    connection: &mut SqliteConnection,
    source: i64,
    observed: Option<i64>,
    files: &[crate::CatalogFile],
    accessible: bool,
) -> LibraryResult<bool> {
    if files.is_empty() {
        return Ok(false);
    }
    let observations = files
        .iter()
        .map(|row| {
            (
                {
                    let mut file = row.file.clone();
                    file.device_id = None;
                    file.inode = None;
                    file.native_id = None;
                    file
                },
                row.dependencies
                    .iter()
                    .map(|dependency| dependency.path.clone())
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let changed = physical(connection, source, &observations, observed, accessible).await?;
    let roots = files
        .iter()
        .map(|row| {
            (
                &row.file.path,
                row.root_id
                    .clone()
                    .unwrap_or_else(|| blake3::hash(row.file.root.as_bytes()).to_hex().to_string()),
            )
        })
        .collect::<Vec<_>>();
    sqlx::query("UPDATE local_files SET root_id=json_extract(input.value,'$[1]') FROM json_each(?2) input WHERE local_files.source_key=?1 AND local_files.path=json_extract(input.value,'$[0]') AND local_files.root_id IS NOT json_extract(input.value,'$[1]')")
        .bind(source).bind(serde_json::to_string(&roots)?).execute(&mut *connection).await?;

    let parsed = files
        .iter()
        .map(|row| -> LibraryResult<_> {
            let stamp = row
                .parsed
                .as_ref()
                .map(|(stamp, version)| {
                    let mut values = vec![
                        serde_json::json!(stamp.size_bytes),
                        serde_json::json!(stamp.mtime_ns),
                        serde_json::json!(stamp.revision),
                        serde_json::json!(version),
                    ];
                    if row.file.kind == crate::LocalFileKind::Directory {
                        values.push(serde_json::json!(if row.directory_complete {
                            observed
                        } else {
                            None
                        }));
                    }
                    serde_json::to_string(&values)
                })
                .transpose()?;
            Ok((&row.file.path, stamp))
        })
        .collect::<LibraryResult<Vec<_>>>()?;
    let mut query = QueryBuilder::<Sqlite>::new("WITH input(path,parsed) AS(");
    query.push_values(&parsed, |mut row, (path, parsed)| {
        row.push_bind(path).push_bind(parsed);
    });
    query.push(") UPDATE local_files SET parsed_stamp=input.parsed FROM input WHERE local_files.path=input.path AND source_key=").push_bind(source)
            .push(" AND observed_stamp=").push_bind(observed).push(" AND input.parsed IS NOT NULL AND parsed_stamp IS NOT input.parsed");
    query.build().execute(&mut *connection).await?;
    let dependencies = files
        .iter()
        .flat_map(|row| {
            row.dependencies
                .iter()
                .map(move |dependency| (&row.file.path, dependency))
        })
        .map(|(path, dependency)| -> LibraryResult<_> {
            let accepted = dependency
                .accepted
                .as_ref()
                .map(|stamp| {
                    serde_json::to_string(&(stamp.size_bytes, stamp.mtime_ns, &stamp.revision))
                })
                .transpose()?;
            Ok((path, &dependency.path, accepted))
        })
        .collect::<LibraryResult<Vec<_>>>()?;
    if !dependencies.is_empty() {
        let mut query = QueryBuilder::<Sqlite>::new("WITH input(path,dependency,accepted) AS(");
        query.push_values(&dependencies, |mut row, (path, dependency, accepted)| {
            row.push_bind(path)
                .push_bind(dependency)
                .push_bind(accepted);
        });
        query.push(") UPDATE local_file_dependencies AS edge SET accepted_inputs=input.accepted FROM input JOIN local_files file ON file.path=input.path AND file.source_key=").push_bind(source)
                .push(" AND file.observed_stamp=").push_bind(observed).push(" WHERE edge.local_file_key=file.local_file_key AND edge.dependency_path=input.dependency AND input.accepted IS NOT NULL AND edge.accepted_inputs IS NOT input.accepted");
        query.build().execute(&mut *connection).await?;
    }
    Ok(changed)
}

async fn withdrawn_tracks(
    connection: &mut SqliteConnection,
    source: i64,
    files: &[(LocalFileWrite, Vec<String>)],
    songs: &[TrackObservation],
    observed: i64,
) -> LibraryResult<Vec<i64>> {
    let owners = serde_json::to_string(
        &files
            .iter()
            .filter(|(file, dependencies)| {
                !dependencies.is_empty()
                    || matches!(
                        file.state,
                        LocalFileState::Accepted | LocalFileState::Rejected
                    )
            })
            .map(|(file, dependencies)| {
                (
                    &file.path,
                    file.state,
                    file.kind,
                    dependencies,
                    serde_json::to_string(&(file.size_bytes, file.mtime_ns, &file.revision))
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let incoming = serde_json::to_string(
        &songs
            .iter()
            .map(|song| (&song.object_id, &song.source_path, &song.cue_path))
            .collect::<Vec<_>>(),
    )?;
    let unreferenced = "NOT EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key)
        AND NOT EXISTS(SELECT 1 FROM queue_entries entry WHERE entry.track_key=track.track_key)
        AND NOT EXISTS(SELECT 1 FROM user_media_state state WHERE state.media_uri=track.media_uri AND(state.favorite=1 OR state.rating IS NOT NULL))
        AND NOT EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import')";
    // Resolve this batch's physical paths before reading the affected tracks.
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("WITH owners AS MATERIALIZED(SELECT json_extract(value,'$[0]') path,json_extract(value,'$[1]') state,json_extract(value,'$[2]') kind,json_extract(value,'$[3]') dependencies,json_extract(value,'$[4]') parsed FROM json_each(?2)),incoming AS MATERIALIZED(SELECT json_extract(value,'$[0]') object_id,json_extract(value,'$[1]') path,json_extract(value,'$[2]') cue FROM json_each(?3)),
        paths AS(SELECT path FROM owners UNION SELECT dependency.value FROM owners CROSS JOIN json_each(owners.dependencies) dependency),
        physical AS MATERIALIZED(SELECT file.local_file_key,access.path,file.path canonical_path,access.observed_stamp,CASE WHEN access.accepted_facts IS file.parsed_stamp THEN access.parsed_stamp END parsed_stamp,access.size_bytes,access.mtime_ns,access.revision FROM paths input CROSS JOIN local_file_observations access ON access.path=input.path CROSS JOIN local_files file ON file.local_file_key=access.local_file_key WHERE file.source_key=?1),
        candidates AS(SELECT track.track_key FROM physical file CROSS JOIN tracks track ON track.source_key=?1 AND track.source_path=file.canonical_path UNION SELECT track.track_key FROM physical file CROSS JOIN tracks track ON track.source_key=?1 AND track.cue_path=file.canonical_path),
        locations AS MATERIALIZED(SELECT track.track_key,backing.path source_path,owner.path cue_path,CASE WHEN track.cue_path IS NULL THEN backing.local_file_key ELSE owner.local_file_key END local_file_key FROM candidates CROSS JOIN tracks track USING(track_key) JOIN local_files canonical ON canonical.source_key=track.source_key AND canonical.path=track.source_path JOIN local_file_observations backing ON backing.local_file_key=canonical.local_file_key LEFT JOIN local_files cue ON cue.source_key=track.source_key AND cue.path=track.cue_path LEFT JOIN local_file_observations owner ON owner.local_file_key=cue.local_file_key),
        edges AS MATERIALIZED(SELECT edge.* FROM physical file CROSS JOIN observed_file_dependencies edge ON edge.local_file_key=file.local_file_key)
        SELECT track.track_key FROM locations location CROSS JOIN tracks track USING(track_key) WHERE track.source_key=?1 AND(track.indexed=1 OR location.local_file_key IS NOT NULL) AND track.observed_stamp<=?4 AND track.object_id NOT IN(SELECT object_id FROM incoming) AND EXISTS(
          SELECT 1 FROM owners LEFT JOIN physical ON physical.path=owners.path WHERE coalesce(physical.observed_stamp,0)<=?4 AND(
            (owners.state='rejected' AND coalesce(location.cue_path,location.source_path)=owners.path)
            OR(owners.state='accepted' AND owners.kind='cue' AND location.cue_path=owners.path AND track.observed_stamp<?4 AND(
              EXISTS(SELECT 1 FROM incoming WHERE cue=owners.path) OR json_array(json_extract(physical.parsed_stamp,'$[0]'),json_extract(physical.parsed_stamp,'$[1]'),json_extract(physical.parsed_stamp,'$[2]')) IS NOT owners.parsed OR EXISTS(SELECT 1 FROM tracks accepted JOIN locations accepted_location USING(track_key) WHERE accepted_location.local_file_key=physical.local_file_key AND accepted_location.cue_path=owners.path AND accepted.observed_stamp=?4) OR EXISTS(
                SELECT 1 FROM json_each(owners.dependencies) declared LEFT JOIN edges edge ON edge.local_file_key=physical.local_file_key AND edge.dependency_path=declared.value LEFT JOIN physical backing ON backing.path=declared.value WHERE edge.accepted_inputs IS NOT json_array(backing.size_bytes,backing.mtime_ns,backing.revision))))
            OR(owners.kind='cue' AND {unreferenced} AND location.cue_path IS NULL AND location.source_path IN(SELECT value FROM json_each(owners.dependencies)) AND(
              EXISTS(SELECT 1 FROM incoming WHERE cue=owners.path AND path=location.source_path) OR EXISTS(SELECT 1 FROM tracks segment JOIN locations segment_location USING(track_key) WHERE segment_location.local_file_key=physical.local_file_key AND segment_location.cue_path=owners.path AND segment_location.source_path=location.source_path)))
            OR(owners.state='accepted' AND owners.kind='media' AND {unreferenced} AND location.cue_path IS NULL AND location.source_path=owners.path AND EXISTS(SELECT 1 FROM incoming WHERE path=owners.path AND cue IS NOT NULL))
          ))")))
        .bind(source).bind(owners).bind(incoming).bind(observed).fetch_all(connection).await?)
}

fn descendant_prefix(path: &str) -> (char, String) {
    let separator = if url::Url::parse(path).is_ok_and(|uri| uri.path_segments().is_some()) {
        '/'
    } else {
        std::path::MAIN_SEPARATOR
    };
    (
        separator,
        format!("{}{separator}", path.trim_end_matches(separator)),
    )
}

async fn write_track_membership(
    connection: &mut SqliteConnection,
    tracks: &[i64],
    indexed: bool,
    files_removed: bool,
    observed: Option<i64>,
) -> LibraryResult<bool> {
    if tracks.is_empty() {
        return Ok(false);
    }
    let input = serde_json::to_string(tracks)?;
    let albums: Vec<i64>=sqlx::query_scalar("SELECT DISTINCT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL").bind(&input).fetch_all(&mut *connection).await?;
    if indexed {
        sqlx::query("UPDATE tracks SET observed_stamp=max(observed_stamp,coalesce(?2,observed_stamp)) WHERE track_key IN(SELECT value FROM json_each(?1)) AND observed_stamp<coalesce(?2,observed_stamp)")
            .bind(&input).bind(observed).execute(&mut *connection).await?;
    }
    let file_paths:Vec<(i64,String)>=sqlx::query_as("SELECT DISTINCT file.source_key,file.path FROM tracks track JOIN local_files file ON file.source_key=track.source_key AND file.path=coalesce(track.cue_path,track.source_path) WHERE track.track_key IN(SELECT value FROM json_each(?1)) AND(track.indexed<>?2 OR(?3 AND track.local_file_key IS NOT NULL))")
        .bind(&input).bind(indexed).bind(files_removed).fetch_all(&mut *connection).await?;
    let changed:Vec<i64>=sqlx::query_scalar("UPDATE tracks SET indexed=?2,local_file_key=CASE WHEN ?3 THEN NULL ELSE local_file_key END WHERE track_key IN(SELECT value FROM json_each(?1)) AND(indexed<>?2 OR(?3 AND local_file_key IS NOT NULL )) RETURNING track_key")
        .bind(&input).bind(indexed).bind(files_removed).fetch_all(&mut *connection).await?;
    let changed = !changed.is_empty();
    let mut files = std::collections::BTreeMap::<i64, Vec<&str>>::new();
    for (source, path) in &file_paths {
        files.entry(*source).or_default().push(path);
    }
    for (source, paths) in files {
        crate::connect::catalog::mark_files_on(connection, source, &paths, false).await?;
    }

    crate::loudness::invalidate_album_audio(
        connection,
        &albums
            .iter()
            .map(|key| crate::AlbumKey::from_raw(*key))
            .collect::<Vec<_>>(),
        true,
    )
    .await?;
    if changed {
        super::index::albums(connection, &albums).await?;
        super::artwork::refresh_choices(connection, tracks, &albums, &[], false).await?;
    }
    Ok(changed)
}

pub(crate) async fn delete_physical_files(
    connection: &mut SqliteConnection,
    source: i64,
    keys: &[i64],
    bound: i64,
) -> LibraryResult<()> {
    let input = serde_json::to_string(keys)?;
    let modified:Vec<String>=sqlx::query_scalar("UPDATE local_files SET state='observed' WHERE source_key=?1 AND local_file_key IN(SELECT dependency.local_file_key FROM local_file_dependencies dependency JOIN local_files missing ON missing.path=dependency.dependency_path AND missing.source_key=?1 WHERE missing.local_file_key IN(SELECT value FROM json_each(?2))) AND observed_stamp<=?3 AND state<>'observed' RETURNING path")
        .bind(source).bind(&input).bind(bound).fetch_all(&mut *connection).await?;
    crate::connect::catalog::mark_files_on(
        connection,
        source,
        &modified.iter().map(String::as_str).collect::<Vec<_>>(),
        false,
    )
    .await?;
    let deleted: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM local_files WHERE local_file_key IN(SELECT value FROM json_each(?1))",
    )
    .bind(&input)
    .fetch_all(&mut *connection)
    .await?;
    crate::connect::catalog::mark_files_on(
        connection,
        source,
        &deleted.iter().map(String::as_str).collect::<Vec<_>>(),
        true,
    )
    .await?;
    sqlx::query("DELETE FROM local_files WHERE local_file_key IN(SELECT value FROM json_each(?1))")
        .bind(input)
        .execute(connection)
        .await?;
    Ok(())
}

pub(crate) async fn remove_files_page_on(
    connection: &mut SqliteConnection,
    source: i64,
    path: &str,
    bound: i64,
) -> LibraryResult<(CatalogUpdate, bool)> {
    let (separator, prefix) = descendant_prefix(path);
    let mut update = CatalogUpdate::default();
    let songs:Vec<i64>=sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND indexed=1 AND observed_stamp<=?4 AND(location.source_path=?2 OR(location.source_path>=?3 AND location.source_path<?3||char(1114111)) OR location.cue_path=?2 OR(location.cue_path>=?3 AND location.cue_path<?3||char(1114111))) AND NOT EXISTS(SELECT 1 FROM observed_local_files file WHERE file.source_key=?1 AND file.path=coalesce(location.cue_path,location.source_path) AND file.observed_stamp>?4) ORDER BY track_key LIMIT 128").bind(source).bind(path).bind(&prefix).bind(bound).fetch_all(&mut *connection).await?;
    let files: Vec<(i64, bool)> = sqlx::query_as("SELECT local_file_key,kind='directory' FROM observed_local_files file WHERE source_key=?1 AND EXISTS(SELECT 1 FROM local_file_access_roots access WHERE access.source_key=file.source_key AND access.root=file.root) AND(path=?2 OR(path>=?3 AND path<?3||char(1114111))) AND observed_stamp<=?4 AND(kind<>'directory' OR NOT EXISTS(SELECT 1 FROM observed_local_files child WHERE child.source_key=?1 AND child.path>=rtrim(file.path,?5)||?5 AND child.path<rtrim(file.path,?5)||?5||char(1114111) AND child.observed_stamp>?4)) ORDER BY path DESC LIMIT 128").bind(source).bind(path).bind(&prefix).bind(bound).bind(separator.to_string()).fetch_all(&mut *connection).await?;
    if songs.is_empty() && files.is_empty() {
        return Ok((update, true));
    }
    update.metadata |= write_track_membership(connection, &songs, false, true, None).await?;
    if !files.is_empty() {
        update.metadata |= files.iter().any(|(_, directory)| *directory);
        delete_physical_files(
            connection,
            source,
            &files.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            bound,
        )
        .await?;
    }
    Ok((update, false))
}

impl Database {
    pub async fn source_file_reuse_candidates(
        &self,
        source_id: &str,
        files: &[LocalFileWrite],
    ) -> LibraryResult<Vec<crate::LocalFileRow>> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let source = super::cache::source_key(connection, source_id).await?;
        let mut rows = crate::local::local_file_reuse_candidates_on(
            connection,
            SourceKey::from_raw(source),
            files,
        )
        .await?;
        let stamps:std::collections::HashMap<String,String>=sqlx::query_as::<_,(String,String)>("SELECT path,parsed_stamp FROM observed_local_files WHERE source_key=?1 AND parsed_stamp IS NOT NULL AND path IN(SELECT value FROM json_each(?2))")
            .bind(source).bind(serde_json::to_string(&rows.iter().map(|row|&row.path).collect::<Vec<_>>())?).fetch_all(&mut *connection).await?.into_iter().collect();
        for row in &mut rows {
            if let Some(stamp) = stamps.get(&row.path) {
                let (size, mtime, revision, version): (
                    Option<i64>,
                    i64,
                    Option<String>,
                    Option<i64>,
                ) = serde_json::from_value(serde_json::Value::Array(
                    serde_json::from_str::<Vec<serde_json::Value>>(stamp)?
                        .into_iter()
                        .take(4)
                        .collect(),
                ))?;
                row.size_bytes = size;
                row.mtime_ns = mtime;
                row.revision = revision;
                row.parse_version = version;
            }
        }
        Ok(rows)
    }
}

pub(crate) async fn write_file_observations_on(
    connection: &mut SqliteConnection,
    (source, source_id): (i64, &str),
    observed: Option<i64>,
    files: &[(LocalFileWrite, Vec<String>)],
    mut songs: Vec<TrackObservation>,
    admit: bool,
    accessible: bool,
) -> LibraryResult<CatalogUpdate> {
    let observed = Some(super::cache::observation_stamp(&mut *connection, source, observed).await?);
    let mut update = CatalogUpdate::default();
    let physical_files = files
        .iter()
        .map(|(file, _)| (file.path.as_str(), (file.device_id, file.inode)))
        .collect::<std::collections::HashMap<_, _>>();
    let whole_files = songs
        .iter()
        .filter(|track| {
            track.cue_path.is_none()
                && track
                    .media_uri
                    .as_deref()
                    .is_none_or(|uri| crate::cue_media_parts(uri).is_none())
        })
        .filter_map(|track| {
            let path = track.source_path.as_ref()?;
            let (device, inode) = physical_files
                .get(path.as_str())
                .copied()
                .unwrap_or((None, None));
            Some((
                track.media_uri.clone().unwrap_or_else(|| {
                    crate::source_entity_uri(&SourceId::new(source_id), "track", &track.object_id)
                }),
                path.clone(),
                device,
                inode,
            ))
        })
        .collect::<Vec<_>>();
    let aliases = if accessible {
        super::file_reads::equivalent_cue_uris_on(&mut *connection, source, &whole_files).await?
    } else {
        Vec::new()
    };
    let reused: std::collections::HashSet<_> =
        aliases.iter().map(|(uri, _, _)| uri.as_str()).collect();
    songs.retain(|track| {
        !reused.contains(
            track
                .media_uri
                .as_deref()
                .unwrap_or(&crate::source_entity_uri(
                    &SourceId::new(source_id),
                    "track",
                    &track.object_id,
                )),
        )
    });
    let withdrawn = if accessible {
        withdrawn_tracks(&mut *connection, source, files, &songs, observed.unwrap()).await?
    } else {
        Vec::new()
    };
    if accessible {
        update.metadata = physical(&mut *connection, source, files, observed, true).await?;
    }
    update.metadata |=
        write_track_membership(&mut *connection, &withdrawn, false, true, None).await?;
    if admit && accessible {
        let admitted: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND track.object_id IS NOT NULL AND indexed=0 AND observed_stamp<=?3 AND location.local_file_key IN(SELECT local_file_key FROM observed_local_files WHERE source_key=?1 AND path IN(SELECT json_extract(value,'$[0]') FROM json_each(?2)))")
                .bind(source).bind(serde_json::to_string(&files.iter().filter(|(file,_)|file.state==LocalFileState::Accepted).map(|(file,_)|(&file.path,)).collect::<Vec<_>>())?).bind(observed).fetch_all(&mut *connection).await?;
        update.metadata |=
            write_track_membership(&mut *connection, &admitted, true, false, observed).await?;
    }
    let observed_paths = songs
        .iter()
        .filter_map(|track| track.source_path.clone())
        .collect::<Vec<_>>();
    if accessible && !songs.is_empty() {
        let input = serde_json::to_string(
            &songs
                .iter()
                .map(|track| (&track.object_id, &track.source_path, &track.cue_path))
                .collect::<Vec<_>>(),
        )?;
        let canonical:std::collections::HashMap<String,(Option<String>,Option<String>)>=sqlx::query_as::<_,(String,Option<String>,Option<String>)>("SELECT json_extract(input.value,'$[0]'),(SELECT file.path FROM local_file_observations access JOIN local_files file USING(local_file_key) LEFT JOIN tracks existing ON existing.source_key=?1 AND existing.object_id=json_extract(input.value,'$[0]') WHERE file.source_key=?1 AND access.path=json_extract(input.value,'$[1]') ORDER BY file.path IS existing.source_path DESC,file.local_file_key LIMIT 1),(SELECT file.path FROM local_file_observations access JOIN local_files file USING(local_file_key) LEFT JOIN tracks existing ON existing.source_key=?1 AND existing.object_id=json_extract(input.value,'$[0]') WHERE file.source_key=?1 AND access.path=json_extract(input.value,'$[2]') ORDER BY file.path IS existing.cue_path DESC,file.local_file_key LIMIT 1) FROM json_each(?2) input")
                .bind(source).bind(&input).fetch_all(&mut *connection).await?.into_iter().map(|(id,path,cue)|(id,(path,cue))).collect();
        for track in &mut songs {
            if let Some((Some(path), cue)) = canonical.get(&track.object_id) {
                track.source_path = Some(path.clone());
                track.cue_path = cue.clone();
            }
        }
    }
    let (track_update, previous_albums) = super::cache::tracks(
        &mut *connection,
        source,
        source_id,
        &songs,
        observed,
        admit,
        true,
    )
    .await?;
    update.merge(track_update);
    if accessible && !files.is_empty() {
        update.metadata |= sqlx::query("WITH paths AS MATERIALIZED(
                SELECT DISTINCT file.local_file_key,file.path FROM json_each(?2) input
                JOIN local_file_observations access ON access.path=json_extract(input.value,'$[0].path')
                JOIN local_files file USING(local_file_key) WHERE file.source_key=?1),expected AS(
                SELECT track.track_key,file.local_file_key FROM paths file
                JOIN tracks track ON track.source_key=?1 AND track.source_path=file.path AND track.cue_path IS NULL WHERE track.observed_stamp<=?3
                UNION ALL SELECT track.track_key,file.local_file_key FROM paths file
                JOIN tracks track ON track.source_key=?1 AND track.cue_path=file.path WHERE track.observed_stamp<=?3)
              UPDATE tracks SET local_file_key=expected.local_file_key FROM expected
              WHERE tracks.track_key=expected.track_key AND tracks.local_file_key IS NOT expected.local_file_key")
                .bind(source)
                .bind(serde_json::to_string(files)?)
                .bind(observed)
                .execute(&mut *connection)
                .await?
                .rows_affected() > 0;
    }
    update.merge(merge_local_file_uris_on(&mut *connection, &aliases).await?);
    let accepted_cues = files
        .iter()
        .filter(|(file, _)| {
            file.kind == crate::LocalFileKind::Cue && file.state == LocalFileState::Accepted
        })
        .map(|(file, _)| file.path.clone())
        .collect::<Vec<_>>();
    if accessible && (!aliases.is_empty() || !accepted_cues.is_empty()) {
        let paths = whole_files
            .iter()
            .map(|(_, path, _, _)| path.clone())
            .chain(
                files
                    .iter()
                    .filter(|(file, _)| accepted_cues.contains(&file.path))
                    .flat_map(|(_, dependencies)| dependencies.iter().cloned()),
            )
            .chain(observed_paths)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut after = 0;
        loop {
            let page =
                super::file_reads::referenced_file_page_on(&mut *connection, source, &paths, after)
                    .await?;
            let Some(last) = page.last() else { break };
            after = last.0;
            let files = page
                .into_iter()
                .map(|(_, uri, path)| (uri, path, None, None))
                .collect::<Vec<_>>();
            let aliases =
                super::file_reads::equivalent_cue_uris_on(&mut *connection, source, &files).await?;
            update.merge(merge_local_file_uris_on(&mut *connection, &aliases).await?);
        }
    }
    if update.metadata || update.artwork {
        update.artwork |= super::artwork::refresh_batch(
            &mut *connection,
            source,
            &songs,
            &[],
            &[],
            &previous_albums,
            &update,
        )
        .await?;
    }
    if update.metadata && accessible {
        let paths:Vec<String>=sqlx::query_scalar("SELECT DISTINCT file.path FROM json_each(?2) input JOIN local_file_observations access ON access.path=json_extract(input.value,'$[0].path') JOIN local_files file USING(local_file_key) WHERE file.source_key=?1")
                .bind(source).bind(serde_json::to_string(files)?).fetch_all(&mut *connection).await?;
        crate::connect::catalog::mark_files_on(
            &mut *connection,
            source,
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        )
        .await?;
    }
    if update.changed() {
        sqlx::query("UPDATE sources SET catalog_revision=catalog_revision+1 WHERE source_key=?1")
            .bind(source)
            .execute(&mut *connection)
            .await?;
    }
    Ok(update)
}

impl super::Catalog<'_> {
    pub async fn write_file_observations(
        &mut self,
        files: &[(LocalFileWrite, Vec<String>)],
        songs: Vec<TrackObservation>,
        admit: bool,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = self
            .begin_write(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
            .await?;
        let update = write_file_observations_on(
            &mut tx,
            (self.source.raw(), self.source_id),
            self.observed,
            files,
            songs,
            admit,
            !self.incoming,
        )
        .await?;
        self.commit_write(tx).await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn purge_source_file_absences(&mut self, observed: i64) -> LibraryResult<()> {
        let source_id = self.source_id;
        let mut after = String::new();
        loop {
            let paths: Vec<String> = {
                let mut connection = self.database.acquire_reader().await?;
                sqlx::query_scalar("SELECT child.path FROM observed_local_files parent JOIN sources source ON source.source_key=parent.source_key CROSS JOIN observed_local_files child ON child.source_key=parent.source_key AND child.parent_path=rtrim(parent.path,'/'||char(92)) WHERE source.object_id=?1 AND parent.kind='directory' AND parent.observed_stamp=?2 AND json_extract(parent.parsed_stamp,'$[4]')=?2 AND child.observed_stamp<?2 AND child.path>?3 ORDER BY child.path LIMIT 128")
                    .bind(&source_id).bind(observed).bind(&after).fetch_all(&mut *connection).await?
            };
            if paths.is_empty() {
                break;
            }
            after = paths.last().expect("nonempty missing paths").clone();
            self.remove_source_files(&paths, observed - 1).await?;
        }
        Ok(())
    }
    pub async fn update_source_file_membership(
        &mut self,
        roots: &[String],
        excluded: &[std::path::PathBuf],
    ) -> LibraryResult<()> {
        let roots = serde_json::to_string(
            &roots
                .iter()
                .map(|root| {
                    let (_, prefix) = descendant_prefix(root);
                    (root, prefix)
                })
                .collect::<Vec<_>>(),
        )?;
        let excluded = serde_json::to_string(
            &excluded
                .iter()
                .map(|path| {
                    let path = path.to_string_lossy();
                    let (_, prefix) = descendant_prefix(&path);
                    (path, prefix)
                })
                .collect::<Vec<_>>(),
        )?;
        for indexed in [false, true] {
            let mut after = 0_i64;
            loop {
                let mut writer = self.database.writer().await?;
                let mut tx = writer
                    .as_mut()
                    .ok_or(LibraryError::WriterUnavailable)?
                    .begin()
                    .await?;
                let tracks: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND indexed=?2 AND track.track_key>?3 AND coalesce(location.cue_path,location.source_path) IS NOT NULL AND CASE WHEN ?4 THEN location.local_file_key IS NOT NULL AND EXISTS(SELECT 1 FROM observed_local_files file WHERE file.local_file_key=location.local_file_key AND file.state='accepted') ELSE 1 END AND (track.object_id IS NOT NULL AND (EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import') OR(EXISTS(SELECT 1 FROM json_each(?5) root WHERE coalesce(location.cue_path,location.source_path)=json_extract(root.value,'$[0]') OR(coalesce(location.cue_path,location.source_path)>=json_extract(root.value,'$[1]') AND coalesce(location.cue_path,location.source_path)<json_extract(root.value,'$[1]')||char(1114111))) AND NOT EXISTS(SELECT 1 FROM json_each(?6) excluded WHERE coalesce(location.cue_path,location.source_path)=json_extract(excluded.value,'$[0]') OR(coalesce(location.cue_path,location.source_path)>=json_extract(excluded.value,'$[1]') AND coalesce(location.cue_path,location.source_path)<json_extract(excluded.value,'$[1]')||char(1114111))))))=?4 ORDER BY track_key LIMIT 128")
                .bind(self.source).bind(!indexed).bind(after).bind(indexed).bind(&roots).bind(&excluded).fetch_all(&mut *tx).await?;
                let Some(last) = tracks.last() else {
                    tx.commit().await?;
                    break;
                };
                after = *last;
                let changed =
                    write_track_membership(&mut tx, &tracks, indexed, false, self.observed).await?;
                tx.commit().await?;
                self.changes.metadata |= changed;
            }
        }
        Ok(())
    }
    pub async fn remove_source_files(
        &mut self,
        paths: &[String],
        observed: i64,
    ) -> LibraryResult<()> {
        for path in paths {
            loop {
                let mut writer = self.database.writer().await?;
                let mut tx = self
                    .begin_write(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
                    .await?;
                let (update, done) =
                    remove_files_page_on(&mut tx, self.source.raw(), path, observed).await?;
                self.commit_write(tx).await?;
                self.changes.merge(update);
                if done {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl super::Catalog<'_> {
    pub async fn merge_local_file_uris(
        &mut self,
        aliases: &[(String, String, String)],
    ) -> LibraryResult<()> {
        if aliases.is_empty() {
            return Ok(());
        }
        let mut writer = self.database.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let update = merge_local_file_uris_on(&mut transaction, aliases).await?;
        transaction.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
}

async fn merge_local_file_uris_on(
    connection: &mut SqliteConnection,
    aliases: &[(String, String, String)],
) -> LibraryResult<CatalogUpdate> {
    let mut update = CatalogUpdate::default();
    if aliases.is_empty() {
        return Ok(update);
    }
    let candidates: Vec<(i64,Option<i64>)> = sqlx::query_as("SELECT track_key,album_key FROM tracks WHERE media_uri IN(SELECT json_extract(value,'$[0]') FROM json_each(?1) UNION SELECT json_extract(value,'$[1]') FROM json_each(?1))")
        .bind(serde_json::to_string(aliases)?).fetch_all(&mut *connection).await?;
    let albums = candidates
        .iter()
        .filter_map(|(_, album)| *album)
        .collect::<Vec<_>>();
    let mut rekeys = Vec::new();
    for (previous, current, object_id) in aliases {
        if previous == current {
            continue;
        }
        let old_identity=sqlx::query_as::<_,(i64,String,String)>("SELECT track.source_key,source.object_id,track.object_id FROM tracks track JOIN sources source USING(source_key) WHERE track.media_uri=?1 AND track.object_id IS NOT NULL")
            .bind(previous).fetch_optional(&mut *connection).await?;
        let target: Option<(TrackKey, bool)> =
            sqlx::query_as("SELECT track_key,cue_path IS NOT NULL FROM tracks WHERE media_uri=?1")
                .bind(current)
                .fetch_optional(&mut *connection)
                .await?;
        let old = sqlx::query_as::<_, (TrackKey, Option<i64>, bool)>(
            "SELECT track_key,source_key,indexed FROM tracks WHERE media_uri=?1
              AND(NOT ?2 OR object_id IS NOT NULL OR indexed=1 OR local_file_key IS NOT NULL
                OR EXISTS(SELECT 1 FROM playlist_entries entry WHERE entry.track_key=tracks.track_key)
                OR EXISTS(SELECT 1 FROM queue_entries entry WHERE entry.track_key=tracks.track_key)
                OR EXISTS(SELECT 1 FROM home_entries entry WHERE entry.entity_kind='track' AND entry.entity_key=tracks.track_key))",
        )
        .bind(previous).bind(target.is_some_and(|(_,cue)| cue))
        .fetch_optional(&mut *connection)
        .await?;
        let current_key = if let Some((key, _)) = target {
            key
        } else if let Some((old_key, _, _)) = old {
            sqlx::query("UPDATE tracks SET media_uri=?2,object_id=CASE WHEN source_key IS NOT NULL THEN ?3 ELSE object_id END,source_path=CASE WHEN source_path IS NOT NULL THEN coalesce((SELECT file.path FROM local_file_observations access JOIN local_files file USING(local_file_key) WHERE file.source_key=tracks.source_key AND access.path=?4 LIMIT 1),source_path) ELSE source_path END WHERE track_key=?1")
                .bind(old_key).bind(current).bind(object_id).bind(crate::file_media_path(current).map(|path|path.to_string_lossy().into_owned())).execute(&mut *connection).await?;
            old_key
        } else {
            crate::catalog::remember_track(
                &mut *connection,
                &crate::TrackReference {
                    media_uri: current.clone(),
                    ..Default::default()
                },
            )
            .await?
        };
        if let Some((old_key, source, indexed)) = old.filter(|(key, _, _)| *key != current_key) {
            sqlx::query(
                "UPDATE tracks SET object_id=NULL,indexed=0,local_file_key=NULL WHERE track_key=?1",
            )
            .bind(old_key)
            .execute(&mut *connection)
            .await?;
            sqlx::query("UPDATE tracks AS canonical SET metadata=CASE WHEN canonical.indexed=0 THEN previous.metadata ELSE canonical.metadata END,native_artwork=CASE WHEN canonical.indexed=1 THEN canonical.native_artwork ELSE COALESCE(canonical.native_artwork,previous.native_artwork) END,first_seen_at=COALESCE(canonical.first_seen_at,previous.first_seen_at),source_key=COALESCE(canonical.source_key,?3),object_id=COALESCE(canonical.object_id,CASE WHEN ?3 IS NOT NULL THEN ?4 END),indexed=max(canonical.indexed,?5),album_key=CASE WHEN canonical.indexed=0 THEN COALESCE(canonical.album_key,previous.album_key) ELSE canonical.album_key END,source_path=COALESCE(canonical.source_path,previous.source_path),source_loudness_analysis_key=CASE WHEN canonical.source_key IS NULL THEN previous.source_loudness_analysis_key ELSE canonical.source_loudness_analysis_key END,loudness_analysis_key=CASE WHEN canonical.source_key IS NULL THEN previous.loudness_analysis_key ELSE canonical.loudness_analysis_key END FROM tracks previous WHERE canonical.track_key=?1 AND previous.track_key=?2")
                .bind(current_key).bind(old_key).bind(source).bind(object_id).bind(indexed).execute(&mut *connection).await?;
            for table in [
                "track_artists",
                "track_album_artists",
                "track_genres",
                "track_moods",
                "track_folders",
            ] {
                let relation = match table {
                    "track_artists" | "track_album_artists" => "artist_key",
                    "track_genres" => "genre_key",
                    "track_moods" => "mood_key",
                    _ => "folder_key",
                };
                sqlx::query(sqlx::AssertSqlSafe(format!("INSERT OR IGNORE INTO {table}(track_key,{relation},position) SELECT ?2,{relation},position FROM {table} WHERE track_key=?1 AND (SELECT metadata FROM tracks WHERE track_key=?1) IS (SELECT metadata FROM tracks WHERE track_key=?2) AND NOT EXISTS(SELECT 1 FROM {table} target WHERE target.track_key=?2)")))
                    .bind(old_key).bind(current_key).execute(&mut *connection).await?;
            }
            update.metadata |= indexed;
            update.playlists |=
                sqlx::query("UPDATE playlist_entries SET track_key=?2 WHERE track_key=?1")
                    .bind(old_key)
                    .bind(current_key)
                    .execute(&mut *connection)
                    .await?
                    .rows_affected()
                    > 0;
            sqlx::query("UPDATE queue_entries SET track_key=?2 WHERE track_key=?1")
                .bind(old_key)
                .bind(current_key)
                .execute(&mut *connection)
                .await?;
            update.home |= sqlx::query(
                "UPDATE home_entries SET entity_key=?2 WHERE entity_kind='track' AND entity_key=?1",
            )
            .bind(old_key)
            .bind(current_key)
            .execute(&mut *connection)
            .await?
            .rows_affected()
                > 0;
            rekeys.push((old_key, current_key));
        }
        update.metadata |= sqlx::query(
            "INSERT INTO main.user_media_state(media_uri,favorite,rating,extra)
            SELECT ?2,favorite,rating,extra FROM main.user_media_state WHERE media_uri=?1
            ON CONFLICT(media_uri) DO UPDATE SET
                favorite=COALESCE(user_media_state.favorite,excluded.favorite),
                rating=COALESCE(user_media_state.rating,excluded.rating),
                extra=COALESCE(user_media_state.extra,excluded.extra)
            WHERE (user_media_state.favorite IS NULL AND excluded.favorite IS NOT NULL)
               OR (user_media_state.rating IS NULL AND excluded.rating IS NOT NULL)
               OR (user_media_state.extra IS NULL AND excluded.extra IS NOT NULL)",
        )
        .bind(previous)
        .bind(current)
        .execute(&mut *connection)
        .await?
        .rows_affected()
            > 0;
        sqlx::query("INSERT INTO native_media_state(media_uri,favorite,rating,play_count,skip_count,last_played) SELECT ?2,favorite,rating,play_count,skip_count,last_played FROM native_media_state WHERE media_uri=?1 ON CONFLICT(media_uri) DO NOTHING").bind(previous).bind(current).execute(&mut *connection).await?;
        sqlx::query("DELETE FROM native_media_state WHERE media_uri=?1")
            .bind(previous)
            .execute(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM main.user_media_state WHERE media_uri=?1")
            .bind(previous)
            .execute(&mut *connection)
            .await?;
        update.activity |= sqlx::query(
            "UPDATE main.listens SET
                captured_media_uri=COALESCE(captured_media_uri,media_uri),media_uri=?2
            WHERE media_uri=?1",
        )
        .bind(previous)
        .bind(current)
        .execute(&mut *connection)
        .await?
        .rows_affected()
            > 0;
        sqlx::query(
            "UPDATE tracks SET local_play_count=(
                SELECT count(*) FROM main.listens WHERE media_uri=tracks.media_uri)
            WHERE media_uri IN (?1,?2) AND local_play_count IS NOT (
                SELECT count(*) FROM main.listens WHERE media_uri=tracks.media_uri)",
        )
        .bind(previous)
        .bind(current)
        .execute(&mut *connection)
        .await?;

        if let Some((old_source, old_source_id, old_object)) = old_identity {
            let (source, source_id, object) = sqlx::query_as::<_, (i64, String, String)>(
                "SELECT track.source_key,source.object_id,track.object_id
                 FROM tracks track JOIN sources source USING(source_key) WHERE track.media_uri=?1",
            )
            .bind(current)
            .fetch_optional(&mut *connection)
            .await?
            .unwrap_or_else(|| (old_source, old_source_id.clone(), object_id.clone()));
            if (old_source, old_object.as_str()) != (source, object.as_str()) {
                sqlx::query("INSERT INTO main.legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at)
                    SELECT ?3,period,item_kind,?4,play_count,skip_count,last_played_at
                    FROM main.legacy_activity WHERE source_id=?1 AND track_object_id=?2 AND item_kind='track'
                    ON CONFLICT(source_id,period,item_kind,track_object_id) DO UPDATE SET
                        play_count=legacy_activity.play_count+excluded.play_count,
                        skip_count=legacy_activity.skip_count+excluded.skip_count,
                        last_played_at=COALESCE(max(legacy_activity.last_played_at,excluded.last_played_at),legacy_activity.last_played_at,excluded.last_played_at)")
                    .bind(&old_source_id).bind(&old_object).bind(&source_id).bind(&object)
                    .execute(&mut *connection).await?;
                sqlx::query("DELETE FROM main.legacy_activity WHERE source_id=?1 AND track_object_id=?2 AND item_kind='track'")
                    .bind(&old_source_id).bind(&old_object).execute(&mut *connection).await?;
            }
        }
        if crate::cue_media_parts(current).is_some() {
            sqlx::query("INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) SELECT source_key,?2,origin,path,root,relative_path,access_uri FROM local_locators WHERE media_uri=?1 ON CONFLICT(media_uri,origin) DO NOTHING")
                .bind(previous).bind(current).execute(&mut *connection).await?;
        }
        if let Some(path) = crate::file_media_path(current) {
            let path = path.to_string_lossy().into_owned();
            let existing:Option<(Option<String>,String,String)>=sqlx::query_as("SELECT source.object_id,locator.root,locator.relative_path FROM local_locators locator LEFT JOIN sources source USING(source_key) WHERE locator.media_uri=?1 AND locator.origin='import'").bind(previous).fetch_optional(&mut *connection).await?;
            let (source_id, root, relative_path) =
                existing.unwrap_or((None, String::new(), String::new()));
            crate::local::write_local_locator(
                &mut *connection,
                &LocalLocatorWrite {
                    source_id,
                    media_uri: previous.clone(),
                    origin: "import".into(),
                    path,
                    root,
                    relative_path,
                    access_uri: current.clone(),
                },
            )
            .await?;
            sqlx::query("UPDATE local_locators SET access_uri=?2,path=?3 WHERE media_uri=?1 AND access_uri=?1 AND origin='local'")
                .bind(previous).bind(current).bind(crate::file_media_path(current).map(|path|path.to_string_lossy().into_owned())).execute(&mut *connection).await?;
        }
        sqlx::query("INSERT INTO lyrics_cache(media_uri,authority,role,language,script,cache_input_digest,lyrics,updated_at) SELECT ?2,authority,role,language,script,cache_input_digest,lyrics,updated_at FROM lyrics_cache WHERE media_uri=?1 ON CONFLICT DO NOTHING").bind(previous).bind(current).execute(&mut *connection).await?;
        sqlx::query("DELETE FROM lyrics_cache WHERE media_uri=?1")
            .bind(previous)
            .execute(&mut *connection)
            .await?;
        sqlx::query("INSERT INTO favorite_outbox(media_uri,favorite,previous_favorite,attempts,next_attempt_at) SELECT ?2,favorite,previous_favorite,attempts,next_attempt_at FROM favorite_outbox WHERE media_uri=?1 ON CONFLICT DO NOTHING").bind(previous).bind(current).execute(&mut *connection).await?;
        sqlx::query("DELETE FROM favorite_outbox WHERE media_uri=?1")
            .bind(previous)
            .execute(&mut *connection)
            .await?;
    }
    for table in ["playlists", "smart_playlists"] {
        update.playlists |= sqlx::query(sqlx::AssertSqlSafe(format!("WITH aliases AS(
            SELECT json_extract(value,'$[0]') previous,json_extract(value,'$[1]') current FROM json_each(?1)
        ) UPDATE {table} AS owner SET artwork_binding=CAST((
            SELECT json_group_array(json(CASE WHEN json_type(candidate.value,'$.image.automatic.chosen')='array'
                THEN json_set(candidate.value,'$.image.automatic.chosen',json((
                    SELECT json_group_array(uri ORDER BY ordinal) FROM (
                        SELECT coalesce((SELECT current FROM aliases WHERE previous=chosen.value LIMIT 1),chosen.value) uri,
                               min(CAST(chosen.key AS INTEGER)) ordinal
                        FROM json_each(candidate.value,'$.image.automatic.chosen') chosen GROUP BY uri)))
                ) ELSE candidate.value END) ORDER BY CAST(candidate.key AS INTEGER))
            FROM json_each(owner.artwork_binding) candidate
        ) AS BLOB) WHERE EXISTS(
            SELECT 1 FROM json_each(owner.artwork_binding) candidate
            CROSS JOIN json_each(candidate.value,'$.image.automatic.chosen') chosen
            JOIN aliases ON aliases.previous=chosen.value)")))
            .bind(serde_json::to_string(aliases)?).execute(&mut *connection).await?.rows_affected()>0;
    }
    if !rekeys.is_empty() {
        super::index::albums(connection, &albums).await?;
        super::artwork::refresh_choices(
            connection,
            &candidates.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            &albums,
            &[],
            true,
        )
        .await?;
    }
    update.track_rekeys = rekeys.into();
    Ok(update)
}
