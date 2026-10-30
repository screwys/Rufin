use super::merge::Touched;
use super::records::{Source, Track};
use super::write::Write;
use super::write::{Family, Scope};
use super::*;
use crate::{LocalFileState, LocalFileWrite, LocalLocatorWrite, TrackObservation};
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
        let mut shared:Vec<(String,String)>=sqlx::query_as("SELECT access.root_id,coalesce((SELECT root FROM local_files file WHERE file.source_key=access.source_key AND file.root_id=access.root_id LIMIT 1),access.root) FROM local_file_access_roots access WHERE source_key=?1 AND access.root=?2")
            .bind(source).bind(root).fetch_all(&mut *connection).await?;
        if shared.is_empty() {
            let id = blake3::hash(root.as_bytes()).to_hex().to_string();
            sqlx::query("INSERT OR IGNORE INTO local_file_access_roots(source_key,root,root_id) VALUES(?1,?2,?3)")
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
    let mut listed = false;
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
        // Folder pages list directories by path; a new or moved directory changes them, its
        // stamps do not.
        listed |= sqlx::query_scalar::<_, bool>("WITH input AS(SELECT json_extract(value,'$[0]') file FROM json_each(?2)) SELECT EXISTS(SELECT 1 FROM input LEFT JOIN local_files old ON old.source_key=?1 AND old.path=json_extract(input.file,'$.path') WHERE json_extract(input.file,'$.kind')='directory' AND coalesce(old.observed_stamp,0)<=coalesce(?3,0) AND(old.path IS NULL OR(old.root,old.relative_path,old.kind) IS NOT(json_extract(input.file,'$.root'),json_extract(input.file,'$.relative_path'),json_extract(input.file,'$.kind'))))")
            .bind(source).bind(serde_json::to_string(page)?).bind(observed).fetch_one(&mut *connection).await?;
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
            continue;
        }
        let input = serde_json::to_string(&parsed)?;
        let expected = "WITH owners AS(SELECT json_extract(value,'$[0]') path,json_extract(value,'$[1]') dependencies,json_extract(value,'$[2]') accepted FROM json_each(?2)),expected AS(SELECT file.local_file_key,item.value dependency_path,CAST(item.key AS INTEGER) position FROM owners CROSS JOIN local_files file ON file.source_key=?1 AND file.path=owners.path AND file.observed_stamp<=?3 CROSS JOIN json_each(owners.dependencies) item) ";
        sqlx::query(sqlx::AssertSqlSafe(format!("{expected} DELETE FROM local_file_dependencies AS current WHERE local_file_key IN(SELECT file.local_file_key FROM owners CROSS JOIN local_files file ON file.source_key=?1 AND file.path=owners.path WHERE file.observed_stamp<=?3) AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.local_file_key=current.local_file_key AND expected.dependency_path=current.dependency_path AND expected.position=current.position)"))).bind(source).bind(&input).bind(observed).execute(&mut *connection).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("{expected} INSERT OR IGNORE INTO local_file_dependencies(local_file_key,dependency_path,position) SELECT local_file_key,dependency_path,position FROM expected"))).bind(source).bind(&input).bind(observed).execute(&mut *connection).await?;
        sqlx::query("WITH accepted AS MATERIALIZED(SELECT edge.local_file_key,edge.dependency_path,CASE WHEN dependency.path IS NOT NULL THEN json_array(dependency.size_bytes,dependency.mtime_ns,dependency.revision) END inputs FROM json_each(?2) input CROSS JOIN local_files owner ON owner.source_key=?1 AND owner.path=json_extract(input.value,'$[0]') CROSS JOIN local_file_dependencies edge ON edge.local_file_key=owner.local_file_key LEFT JOIN local_files dependency ON dependency.source_key=?1 AND dependency.path=edge.dependency_path WHERE json_extract(input.value,'$[2]') AND owner.observed_stamp<=?3 AND(dependency.path IS NULL OR dependency.observed_stamp<=?3)) UPDATE local_file_dependencies AS edge SET accepted_inputs=accepted.inputs FROM accepted WHERE edge.local_file_key=accepted.local_file_key AND edge.dependency_path=accepted.dependency_path AND edge.accepted_inputs IS NOT accepted.inputs")
            .bind(source).bind(&input).bind(observed).execute(&mut *connection).await?;
    }
    Ok(listed)
}

async fn withdrawn_tracks(
    write: &mut Write<'_>,
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
    // Resolve this batch's physical paths before reading the affected tracks.
    let scope = "WITH owners AS MATERIALIZED(SELECT json_extract(value,'$[0]') path,json_extract(value,'$[1]') state,json_extract(value,'$[2]') kind,json_extract(value,'$[3]') dependencies,json_extract(value,'$[4]') parsed FROM json_each(?2)),incoming AS MATERIALIZED(SELECT json_extract(value,'$[0]') object_id,json_extract(value,'$[1]') path,json_extract(value,'$[2]') cue FROM json_each(?3)),
        paths AS(SELECT path FROM owners UNION SELECT dependency.value FROM owners CROSS JOIN json_each(owners.dependencies) dependency),
        physical AS MATERIALIZED(SELECT file.local_file_key,access.path,file.path canonical_path,access.observed_stamp,CASE WHEN access.accepted_facts IS file.parsed_stamp THEN access.parsed_stamp END parsed_stamp,access.size_bytes,access.mtime_ns,access.revision FROM paths input CROSS JOIN local_file_observations access ON access.path=input.path CROSS JOIN local_files file ON file.local_file_key=access.local_file_key WHERE file.source_key=?1),
        candidates AS(SELECT details.track_key FROM physical file CROSS JOIN track_details details ON details.source_path=file.canonical_path UNION SELECT details.track_key FROM physical file CROSS JOIN track_details details ON details.cue_path=file.canonical_path),
        locations AS MATERIALIZED(SELECT track.track_key,backing.path source_path,owner.path cue_path,CASE WHEN details.cue_path IS NULL THEN backing.local_file_key ELSE owner.local_file_key END local_file_key FROM candidates CROSS JOIN tracks track USING(track_key) JOIN track_details details USING(track_key) JOIN local_files canonical ON canonical.source_key=track.source_key AND canonical.path=details.source_path JOIN local_file_observations backing ON backing.local_file_key=canonical.local_file_key LEFT JOIN local_files cue ON cue.source_key=track.source_key AND cue.path=details.cue_path LEFT JOIN local_file_observations owner ON owner.local_file_key=cue.local_file_key WHERE track.source_key=?1),
        edges AS MATERIALIZED(SELECT edge.* FROM physical file CROSS JOIN observed_file_dependencies edge ON edge.local_file_key=file.local_file_key)";
    // Seen sets live in memory, so the affected tracks are split by them first: a live change
    // wrote the newer ones, and the walk saw the others it marked.
    let affected: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "{scope} SELECT track_key FROM locations"
    )))
    .bind(source)
    .bind(&owners)
    .bind(&incoming)
    .fetch_all(write.connection().await?)
    .await?;
    let newer = affected
        .iter()
        .copied()
        .filter(|key| write.newer(Family::Track, *key))
        .collect::<Vec<_>>();
    let seen = affected
        .into_iter()
        .filter(|key| write.saw(Family::Track, *key))
        .collect::<Vec<_>>();
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("{scope}
        SELECT track.track_key FROM locations location CROSS JOIN tracks track USING(track_key) WHERE track.source_key=?1 AND(track.indexed=1 OR location.local_file_key IS NOT NULL) AND track.track_key NOT IN(SELECT value FROM json_each(?5)) AND track.object_id NOT IN(SELECT object_id FROM incoming) AND EXISTS(
          SELECT 1 FROM owners LEFT JOIN physical ON physical.path=owners.path WHERE coalesce(physical.observed_stamp,0)<=?4 AND(
            (owners.state='rejected' AND coalesce(location.cue_path,location.source_path)=owners.path)
            OR(owners.state='accepted' AND owners.kind='cue' AND location.cue_path=owners.path AND track.track_key NOT IN(SELECT value FROM json_each(?6)) AND(
              EXISTS(SELECT 1 FROM incoming WHERE cue=owners.path) OR json_array(json_extract(physical.parsed_stamp,'$[0]'),json_extract(physical.parsed_stamp,'$[1]'),json_extract(physical.parsed_stamp,'$[2]')) IS NOT owners.parsed OR EXISTS(SELECT 1 FROM tracks accepted JOIN locations accepted_location USING(track_key) WHERE accepted_location.local_file_key=physical.local_file_key AND accepted_location.cue_path=owners.path AND accepted.track_key IN(SELECT value FROM json_each(?6)) AND accepted.track_key NOT IN(SELECT value FROM json_each(?5))) OR EXISTS(
                SELECT 1 FROM json_each(owners.dependencies) declared LEFT JOIN edges edge ON edge.local_file_key=physical.local_file_key AND edge.dependency_path=declared.value LEFT JOIN physical backing ON backing.path=declared.value WHERE edge.accepted_inputs IS NOT CASE WHEN backing.path IS NOT NULL THEN json_array(backing.size_bytes,backing.mtime_ns,backing.revision) END)))
            OR(owners.kind='cue' AND location.cue_path IS NULL AND location.source_path IN(SELECT value FROM json_each(owners.dependencies)) AND(
              EXISTS(SELECT 1 FROM incoming WHERE cue=owners.path AND path=location.source_path) OR EXISTS(SELECT 1 FROM tracks segment JOIN locations segment_location USING(track_key) WHERE segment_location.local_file_key=physical.local_file_key AND segment_location.cue_path=owners.path AND segment_location.source_path=location.source_path)))
            OR(owners.state='accepted' AND owners.kind='media' AND location.cue_path IS NULL AND location.source_path=owners.path AND EXISTS(SELECT 1 FROM incoming WHERE path=owners.path AND cue IS NOT NULL))
          ))")))
        .bind(source).bind(owners).bind(incoming).bind(observed).bind(serde_json::to_string(&newer)?).bind(serde_json::to_string(&seen)?)
        .fetch_all(write.connection().await?).await?)
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

/// Admits or withdraws file tracks.
async fn write_track_membership(
    write: &mut Write<'_>,
    tracks: &[i64],
    indexed: bool,
    touched: &mut Touched,
) -> LibraryResult<()> {
    if tracks.is_empty() {
        return Ok(());
    }
    let mut albums = Vec::new();
    let mut changed = false;
    write.read_keys::<Track>(tracks).await?;
    for key in tracks {
        let Some(mut track) = write.get::<Track>(*key).await? else {
            continue;
        };
        albums.extend(track.album);
        if indexed {
            write.see(&track);
        }
        if track.indexed != indexed {
            track.indexed = indexed;
            touched.tracks.insert(*key);
            changed = true;
            write.put(track);
        }
    }
    albums.sort_unstable();
    albums.dedup();
    if changed {
        touched
            .albums
            .extend(albums.iter().map(|album| album.raw()));
    }
    Ok(())
}

pub(crate) async fn delete_physical_files(
    write: &mut Write<'_>,
    source: i64,
    keys: &[i64],
    bound: i64,
) -> LibraryResult<()> {
    let input = serde_json::to_string(keys)?;
    let connection = write.connection().await?;
    sqlx::query("UPDATE local_files SET state='observed' WHERE source_key=?1 AND local_file_key IN(SELECT dependency.local_file_key FROM local_file_dependencies dependency JOIN local_files missing ON missing.path=dependency.dependency_path AND missing.source_key=?1 WHERE missing.local_file_key IN(SELECT value FROM json_each(?2))) AND observed_stamp<=?3 AND state<>'observed'")
        .bind(source).bind(&input).bind(bound).execute(&mut *connection).await?;
    sqlx::query("DELETE FROM local_files WHERE local_file_key IN(SELECT value FROM json_each(?1))")
        .bind(input)
        .execute(connection)
        .await?;
    Ok(())
}

pub(crate) async fn remove_files_page_on(
    write: &mut Write<'_>,
    source: i64,
    path: &str,
    bound: i64,
    touched: &mut Touched,
) -> LibraryResult<bool> {
    let (separator, prefix) = descendant_prefix(path);
    let songs:Vec<i64>=sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND indexed=1 AND(location.source_path=?2 OR(location.source_path>=?3 AND location.source_path<?3||char(1114111)) OR location.cue_path=?2 OR(location.cue_path>=?3 AND location.cue_path<?3||char(1114111))) AND NOT EXISTS(SELECT 1 FROM observed_local_files file WHERE file.source_key=?1 AND file.path=coalesce(location.cue_path,location.source_path) AND file.observed_stamp>?4) ORDER BY track_key").bind(source).bind(path).bind(&prefix).bind(bound).fetch_all(write.connection().await?).await?;
    let songs = songs
        .into_iter()
        .filter(|key| !write.newer(Family::Track, *key))
        .take(128)
        .collect::<Vec<_>>();
    let files: Vec<(i64, bool)> = sqlx::query_as("SELECT local_file_key,kind='directory' FROM observed_local_files file WHERE source_key=?1 AND EXISTS(SELECT 1 FROM local_file_access_roots access WHERE access.source_key=file.source_key AND access.root=file.root) AND(path=?2 OR(path>=?3 AND path<?3||char(1114111))) AND observed_stamp<=?4 AND(kind<>'directory' OR NOT EXISTS(SELECT 1 FROM observed_local_files child WHERE child.source_key=?1 AND child.path>=rtrim(file.path,?5)||?5 AND child.path<rtrim(file.path,?5)||?5||char(1114111) AND child.observed_stamp>?4)) ORDER BY path DESC LIMIT 128").bind(source).bind(path).bind(&prefix).bind(bound).bind(separator.to_string()).fetch_all(write.connection().await?).await?;
    if songs.is_empty() && files.is_empty() {
        return Ok(true);
    }
    write_track_membership(write, &songs, false, touched).await?;
    if !files.is_empty() {
        write.keep();
        delete_physical_files(
            write,
            source,
            &files.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            bound,
        )
        .await?;
    }
    Ok(false)
}

impl Catalog {
    pub async fn source_file_reuse_candidates(
        &self,
        source_id: &str,
        files: &[LocalFileWrite],
    ) -> LibraryResult<Vec<crate::LocalFileRow>> {
        let mut writer = self.acquire_reader().await?;
        let source: i64 = sqlx::query_scalar("SELECT source_key FROM sources WHERE object_id=?1")
            .bind(source_id)
            .fetch_optional(&mut *writer)
            .await?
            .ok_or_else(|| LibraryError::InvalidRequest("source is no longer configured".into()))?;
        let mut rows = crate::local::local_file_reuse_candidates_on(
            &mut *writer,
            SourceKey::from_raw(source),
            files,
        )
        .await?;
        let stamps:std::collections::HashMap<String,String>=sqlx::query_as::<_,(String,String)>("SELECT path,parsed_stamp FROM observed_local_files WHERE source_key=?1 AND parsed_stamp IS NOT NULL AND path IN(SELECT value FROM json_each(?2))")
            .bind(source).bind(serde_json::to_string(&rows.iter().map(|row|&row.path).collect::<Vec<_>>())?).fetch_all(&mut *writer).await?.into_iter().collect();
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_file_observations_on(
    catalog: &Catalog,
    write: &mut Write<'_>,
    (source_key, source_id): (SourceKey, &str),
    observed: Option<i64>,
    files: &[(LocalFileWrite, Vec<String>)],
    mut songs: Vec<TrackObservation>,
    admit: bool,
    accessible: bool,
    touched: &mut Touched,
) -> LibraryResult<()> {
    let source = source_key.raw();
    let observed = Some(match observed {
        Some(observed) => observed,
        None => catalog.next_stamp(source_key),
    });
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
        super::file_reads::equivalent_cue_uris_on(write.connection().await?, source, &whole_files)
            .await?
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
        withdrawn_tracks(write, source, files, &songs, observed.unwrap()).await?
    } else {
        Vec::new()
    };
    if accessible && !files.is_empty() {
        // File rows are not resident, and the walk's later steps read their stamps, so they
        // are kept even when no client shows them. A changed directory listing shows in the
        // folder pages.
        write.keep();
        if physical(write.connection().await?, source, files, observed, true).await? {
            write.changed(Scope::Collections { source: source_key });
        }
    }
    write_track_membership(write, &withdrawn, false, touched).await?;
    if admit && accessible {
        let admitted: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND track.object_id IS NOT NULL AND indexed=0 AND location.local_file_key IN(SELECT local_file_key FROM observed_local_files WHERE source_key=?1 AND path IN(SELECT json_extract(value,'$[0]') FROM json_each(?2)))")
                .bind(source).bind(serde_json::to_string(&files.iter().filter(|(file,_)|file.state==LocalFileState::Accepted).map(|(file,_)|(&file.path,)).collect::<Vec<_>>())?).fetch_all(write.connection().await?).await?;
        let admitted = admitted
            .into_iter()
            .filter(|key| !write.newer(Family::Track, *key))
            .collect::<Vec<_>>();
        write_track_membership(write, &admitted, true, touched).await?;
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
        let canonical:std::collections::HashMap<String,(Option<String>,Option<String>)>=sqlx::query_as::<_,(String,Option<String>,Option<String>)>("SELECT json_extract(input.value,'$[0]'),(SELECT file.path FROM local_file_observations access JOIN local_files file USING(local_file_key) LEFT JOIN tracks existing ON existing.source_key=?1 AND existing.object_id=json_extract(input.value,'$[0]') LEFT JOIN track_details details ON details.track_key=existing.track_key WHERE file.source_key=?1 AND access.path=json_extract(input.value,'$[1]') ORDER BY file.path IS details.source_path DESC,file.local_file_key LIMIT 1),(SELECT file.path FROM local_file_observations access JOIN local_files file USING(local_file_key) LEFT JOIN tracks existing ON existing.source_key=?1 AND existing.object_id=json_extract(input.value,'$[0]') LEFT JOIN track_details details ON details.track_key=existing.track_key WHERE file.source_key=?1 AND access.path=json_extract(input.value,'$[2]') ORDER BY file.path IS details.cue_path DESC,file.local_file_key LIMIT 1) FROM json_each(?2) input")
                .bind(source).bind(&input).fetch_all(write.connection().await?).await?.into_iter().map(|(id,path,cue)|(id,(path,cue))).collect();
        for track in &mut songs {
            if let Some((Some(path), cue)) = canonical.get(&track.object_id) {
                track.source_path = Some(path.clone());
                track.cue_path = cue.clone();
            }
        }
    }
    super::merge::tracks(write, source_key, source_id, &songs, admit, true, touched).await?;
    merge_local_file_uris_on(write, &aliases, touched).await?;
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
            let page = super::file_reads::referenced_file_page_on(
                write.connection().await?,
                source,
                &paths,
                after,
            )
            .await?;
            let Some(last) = page.last() else { break };
            after = last.0;
            let files = page
                .into_iter()
                .map(|(_, uri, path)| (uri, path, None, None))
                .collect::<Vec<_>>();
            let aliases = super::file_reads::equivalent_cue_uris_on(
                write.connection().await?,
                source,
                &files,
            )
            .await?;
            merge_local_file_uris_on(write, &aliases, touched).await?;
        }
    }
    super::merge::resolve(write, touched).await?;
    Ok(())
}

impl super::Observation<'_> {
    pub async fn write_file_observations(
        &mut self,
        files: &[(LocalFileWrite, Vec<String>)],
        songs: Vec<TrackObservation>,
        admit: bool,
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let mut touched = Touched::default();
        write_file_observations_on(
            self.catalog,
            &mut write,
            (self.source, self.source_id),
            self.observed,
            files,
            songs,
            admit,
            true,
            &mut touched,
        )
        .await?;
        self.commit(write).await?;
        Ok(())
    }
    pub async fn purge_source_file_absences(&mut self, observed: i64) -> LibraryResult<()> {
        let source_id = self.source_id;
        let mut after = String::new();
        loop {
            let paths: Vec<String> = {
                let mut connection = self.catalog.acquire_reader().await?;
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
                let mut write = self.write().await?;
                let tracks: Vec<i64> = sqlx::query_scalar("SELECT track.track_key FROM tracks track JOIN local_track_access location USING(track_key) WHERE track.source_key=?1 AND indexed=?2 AND track.track_key>?3 AND coalesce(location.cue_path,location.source_path) IS NOT NULL AND CASE WHEN ?4 THEN location.local_file_key IS NOT NULL AND EXISTS(SELECT 1 FROM observed_local_files file WHERE file.local_file_key=location.local_file_key AND file.state='accepted') ELSE 1 END AND (track.object_id IS NOT NULL AND (EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='import') OR(EXISTS(SELECT 1 FROM json_each(?5) root WHERE coalesce(location.cue_path,location.source_path)=json_extract(root.value,'$[0]') OR(coalesce(location.cue_path,location.source_path)>=json_extract(root.value,'$[1]') AND coalesce(location.cue_path,location.source_path)<json_extract(root.value,'$[1]')||char(1114111))) AND NOT EXISTS(SELECT 1 FROM json_each(?6) excluded WHERE coalesce(location.cue_path,location.source_path)=json_extract(excluded.value,'$[0]') OR(coalesce(location.cue_path,location.source_path)>=json_extract(excluded.value,'$[1]') AND coalesce(location.cue_path,location.source_path)<json_extract(excluded.value,'$[1]')||char(1114111))))))=?4 ORDER BY track_key LIMIT 128")
                .bind(self.source).bind(!indexed).bind(after).bind(indexed).bind(&roots).bind(&excluded).fetch_all(write.connection().await?).await?;
                let Some(last) = tracks.last() else {
                    break;
                };
                after = *last;
                let mut touched = Touched::default();
                write_track_membership(&mut write, &tracks, indexed, &mut touched).await?;
                super::merge::resolve(&mut write, &mut touched).await?;
                self.commit(write).await?;
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
                let mut write = self.write().await?;
                let mut touched = Touched::default();
                let done = remove_files_page_on(
                    &mut write,
                    self.source.raw(),
                    path,
                    observed,
                    &mut touched,
                )
                .await?;
                super::merge::resolve(&mut write, &mut touched).await?;
                self.commit(write).await?;
                if done {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl super::Observation<'_> {
    pub async fn merge_local_file_uris(
        &mut self,
        aliases: &[(String, String, String)],
    ) -> LibraryResult<()> {
        if aliases.is_empty() {
            return Ok(());
        }
        let mut write = self.write().await?;
        let mut touched = Touched::default();
        merge_local_file_uris_on(&mut write, aliases, &mut touched).await?;
        self.commit(write).await?;
        Ok(())
    }
}

/// A track's source, the source's object id and the track's object id.
async fn track_identity(
    write: &mut Write<'_>,
    track: &Track,
) -> LibraryResult<Option<(SourceKey, String, String)>> {
    let (Some(source), Some(object)) = (track.source, track.object.clone()) else {
        return Ok(None);
    };
    Ok(write
        .get::<Source>(source.raw())
        .await?
        .map(|row| (source, row.object, object)))
}

/// Moves each previous URI to its current URI. The old row takes the new URI, or folds into
/// the row that already holds it, and what names the old URI follows.
async fn merge_local_file_uris_on(
    write: &mut Write<'_>,
    aliases: &[(String, String, String)],
    touched: &mut Touched,
) -> LibraryResult<()> {
    if aliases.is_empty() {
        return Ok(());
    }
    let uris = aliases
        .iter()
        .flat_map(|(previous, current, _)| [previous.as_str(), current.as_str()])
        .collect::<Vec<_>>();
    write.read_naturals::<Track>(&uris).await?;
    let mut candidates = Vec::new();
    for uri in &uris {
        candidates.extend(write.by_natural::<Track>(uri).await?);
    }
    // An old URI with no row that holds nothing moved on an earlier walk.
    let previous = aliases
        .iter()
        .map(|(previous, _, _)| previous.as_str())
        .collect::<Vec<_>>();
    let held: HashSet<String> = sqlx::query_scalar("SELECT value FROM json_each(?1) WHERE EXISTS(SELECT 1 FROM user_state WHERE media_uri=value) OR EXISTS(SELECT 1 FROM listens WHERE media_uri=value) OR EXISTS(SELECT 1 FROM lyrics_cache WHERE media_uri=value) OR EXISTS(SELECT 1 FROM downloads WHERE media_uri=value) OR EXISTS(SELECT 1 FROM local_locators WHERE media_uri=value AND origin<>'import')")
        .bind(serde_json::to_string(&previous)?)
        .fetch_all(write.connection().await?)
        .await?
        .into_iter()
        .collect();
    let mut merged = false;
    for (previous, current, object_id) in aliases {
        if previous == current {
            continue;
        }
        let target = write.by_natural::<Track>(current).await?;
        let mut old = write.by_natural::<Track>(previous).await?;
        if old.is_none() && !held.contains(previous) {
            continue;
        }
        let old_identity = match &old {
            Some(track) => track_identity(write, track).await?,
            None => None,
        };
        // A CUE track keeps its own row unless something still names the old one.
        if let Some(track) = &old
            && target.as_ref().is_some_and(|target| target.cue_path.is_some())
            && track.object.is_none()
            && !track.indexed
            && !sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM playlist_entries WHERE track_key=?1) OR EXISTS(SELECT 1 FROM queue_entries WHERE track_key=?1) OR EXISTS(SELECT 1 FROM home_entries WHERE entity_kind='track' AND entity_key=?1) OR EXISTS(SELECT 1 FROM files WHERE source_key=?2 AND path=?3)")
                .bind(track.key)
                .bind(track.source)
                .bind(track.member_path())
                .fetch_one(write.connection().await?)
                .await?
        {
            old = None;
        }
        let current_key = if let Some(target) = &target {
            target.key
        } else if let Some(old) = &old {
            // The old row takes the new URI.
            let mut track = old.clone();
            if track.source_path.is_some()
                && let Some(path) = sqlx::query_scalar::<_, String>("SELECT file.path FROM local_file_observations access JOIN local_files file USING(local_file_key) WHERE file.source_key=?1 AND access.path=?2 LIMIT 1")
                    .bind(track.source.map(|source| source.raw()))
                    .bind(crate::file_media_path(current).map(|path| path.to_string_lossy().into_owned()))
                    .fetch_optional(write.connection().await?)
                    .await?
            {
                track.source_path = Some(path);
            }
            track.uri.clone_from(current);
            if track.source.is_some() {
                track.object = Some(object_id.clone());
            }
            write.put(track);
            old.key
        } else {
            write
                .put_reference(&crate::TrackReference {
                    media_uri: current.clone(),
                    ..Default::default()
                })
                .await?
        };
        if let Some(previous_track) = old.filter(|track| track.key != current_key) {
            let old_key = previous_track.key;
            // The canonical row keeps what it has and takes the rest from the old row.
            let (details, same_details): (Option<String>, bool) = sqlx::query_as("SELECT json(previous.metadata),canonical.metadata IS previous.metadata FROM tracks old LEFT JOIN track_details previous ON previous.track_key=old.track_key,tracks new LEFT JOIN track_details canonical ON canonical.track_key=new.track_key WHERE old.track_key=?1 AND new.track_key=?2")
                .bind(old_key)
                .bind(current_key)
                .fetch_one(write.connection().await?)
                .await?;
            let mut canonical = write
                .get::<Track>(current_key.raw())
                .await?
                .ok_or_else(|| {
                    LibraryError::InvalidStore(format!("track {current_key} has no row"))
                })?;
            let same_metadata = same_details && canonical.facts == previous_track.facts;
            write.put(Track {
                object: None,
                indexed: false,
                ..previous_track.clone()
            });
            // The old row gives up its identity first, so the canonical row can take it.
            write.connection().await?;
            let copies_edges = !canonical.indexed || same_metadata;
            if !canonical.indexed {
                canonical.facts = previous_track.facts.clone();
                canonical.writes.metadata = Some(details.map_or(
                    Ok(serde_json::Value::Object(serde_json::Map::new())),
                    |json| serde_json::from_str(&json),
                )?);
                canonical.album = canonical.album.or(previous_track.album);
                if canonical.binding.is_none() {
                    canonical.binding.clone_from(&previous_track.binding);
                }
            }
            if canonical.source.is_none() {
                canonical
                    .audio_revision
                    .clone_from(&previous_track.audio_revision);
                canonical.audio_hash.clone_from(&previous_track.audio_hash);
            }
            if previous_track.source.is_some() {
                canonical.object = canonical.object.or_else(|| Some(object_id.clone()));
            }
            canonical.source = canonical.source.or(previous_track.source);
            canonical.first_seen = canonical.first_seen.or(previous_track.first_seen);
            canonical.indexed |= previous_track.indexed;
            canonical.source_path = canonical
                .source_path
                .or_else(|| previous_track.source_path.clone());
            let state = &mut canonical.state;
            state.favorite = state.favorite.or(previous_track.state.favorite);
            state.rating = state.rating.or(previous_track.state.rating);
            state.play_count = state.play_count.or(previous_track.state.play_count);
            state.last_played = state.last_played.or(previous_track.state.last_played);
            // Credits, tags and folders move when both rows describe the same metadata.
            if copies_edges {
                for (target, edges) in [
                    (&mut canonical.artists, &previous_track.artists),
                    (&mut canonical.album_artists, &previous_track.album_artists),
                ] {
                    if target.is_empty() {
                        target.clone_from(edges);
                    }
                }
                if canonical.folders.is_empty() {
                    canonical.folders.clone_from(&previous_track.folders);
                }
                for table in &crate::tags::TAG_TABLES {
                    if !canonical.tags.iter().any(|(kind, _)| *kind == table.kind) {
                        canonical.tags.extend(
                            previous_track
                                .tags
                                .iter()
                                .filter(|(kind, _)| *kind == table.kind),
                        );
                    }
                }
            }
            write.put(canonical);
            let entries: Vec<(i64, i64, Vec<u8>)> = sqlx::query_as(
                "UPDATE playlist_entries SET track_key=?2 WHERE track_key=?1 RETURNING playlist_key,playlist_entry_key,rank",
            )
            .bind(old_key)
            .bind(current_key)
            .fetch_all(write.connection().await?)
            .await?;
            for (playlist, key, rank) in entries {
                write.put_entries(
                    playlist,
                    vec![super::snapshot::Entry {
                        key,
                        rank: rank.into(),
                        track: current_key,
                    }],
                );
            }
            sqlx::query("UPDATE queue_entries SET track_key=?2 WHERE track_key=?1")
                .bind(old_key)
                .bind(current_key)
                .execute(write.connection().await?)
                .await?;
            let sources: Vec<SourceKey> = sqlx::query_scalar("UPDATE home_entries SET entity_key=?2 WHERE entity_kind='track' AND entity_key=?1 RETURNING source_key")
                .bind(old_key)
                .bind(current_key)
                .fetch_all(write.connection().await?)
                .await?;
            for source in sources {
                write.changed(Scope::Home { source });
            }
            write.rekey(old_key, current_key);
            merged = true;
        }
        let mut states: Vec<super::write::StateRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "INSERT INTO user_state(media_uri,favorite,rating,changed_at)
            SELECT ?2,favorite,rating,changed_at FROM user_state WHERE media_uri=?1
            ON CONFLICT(media_uri) DO UPDATE SET
                favorite=COALESCE(user_state.favorite,excluded.favorite),
                rating=COALESCE(user_state.rating,excluded.rating)
            WHERE (user_state.favorite IS NULL AND excluded.favorite IS NOT NULL)
               OR (user_state.rating IS NULL AND excluded.rating IS NOT NULL){}",
            super::write::STATE_RETURNING
        )))
        .bind(previous)
        .bind(current)
        .fetch_all(write.connection().await?)
        .await?;
        states.extend(
            sqlx::query_as::<_, super::write::StateRow>(sqlx::AssertSqlSafe(format!(
                "DELETE FROM user_state WHERE media_uri=?1{}",
                super::write::STATE_DELETED
            )))
            .bind(previous)
            .fetch_all(write.connection().await?)
            .await?,
        );
        write.set_states(states);
        sqlx::query("UPDATE listens SET media_uri=?2 WHERE media_uri=?1")
            .bind(previous)
            .bind(current)
            .execute(write.connection().await?)
            .await?;

        if let Some((old_source, old_source_id, old_object)) = old_identity {
            let moved = match write.by_natural::<Track>(current).await? {
                Some(track) => track_identity(write, &track).await?,
                None => None,
            };
            let (source, source_id, object) =
                moved.unwrap_or_else(|| (old_source, old_source_id.clone(), object_id.clone()));
            if (old_source, old_object.as_str()) != (source, object.as_str()) {
                sqlx::query("INSERT INTO legacy_activity(source_id,period,item_kind,track_object_id,play_count,skip_count,last_played_at,title,artist,album)
                    SELECT ?3,period,item_kind,?4,play_count,skip_count,last_played_at,title,artist,album
                    FROM legacy_activity WHERE source_id=?1 AND track_object_id=?2 AND item_kind='track'
                    ON CONFLICT(source_id,period,item_kind,track_object_id) DO UPDATE SET
                        play_count=legacy_activity.play_count+excluded.play_count,
                        skip_count=legacy_activity.skip_count+excluded.skip_count,
                        last_played_at=COALESCE(max(legacy_activity.last_played_at,excluded.last_played_at),legacy_activity.last_played_at,excluded.last_played_at)")
                    .bind(&old_source_id).bind(&old_object).bind(&source_id).bind(&object)
                    .execute(write.connection().await?).await?;
                sqlx::query("DELETE FROM legacy_activity WHERE source_id=?1 AND track_object_id=?2 AND item_kind='track'")
                    .bind(&old_source_id).bind(&old_object).execute(write.connection().await?).await?;
            }
        }
        write
            .recount(&[previous.as_str(), current.as_str()])
            .await?;
        if crate::cue_media_parts(current).is_some() {
            sqlx::query("INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) SELECT source_key,?2,origin,path,root,relative_path,access_uri FROM local_locators WHERE media_uri=?1 ON CONFLICT(media_uri,origin) DO NOTHING")
                .bind(previous).bind(current).execute(write.connection().await?).await?;
            sqlx::query("INSERT INTO downloads(media_uri,quality,revision,size,path,reason,source_id,root,relative_path,managed,retired) SELECT ?2,quality,revision,size,path,reason,source_id,root,relative_path,managed,retired FROM downloads WHERE media_uri=?1 ON CONFLICT DO NOTHING")
                .bind(previous).bind(current).execute(write.connection().await?).await?;
            write.read_downloads(&[current.as_str()]).await?;
        }
        if let Some(path) = crate::file_media_path(current) {
            let path = path.to_string_lossy().into_owned();
            let existing:Option<(Option<String>,String,String)>=sqlx::query_as("SELECT source.object_id,locator.root,locator.relative_path FROM local_locators locator LEFT JOIN sources source USING(source_key) WHERE locator.media_uri=?1 AND locator.origin='import'").bind(previous).fetch_optional(write.connection().await?).await?;
            let (source_id, root, relative_path) =
                existing.unwrap_or((None, String::new(), String::new()));
            crate::local::write_local_locator(
                write,
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
                .bind(previous).bind(current).bind(crate::file_media_path(current).map(|path|path.to_string_lossy().into_owned())).execute(write.connection().await?).await?;
        }
        crate::lyrics::move_lyrics_on(write, previous, current).await?;
        sqlx::query("INSERT INTO outbox(media_uri,kind,value,next_attempt_at) SELECT ?2,kind,value,next_attempt_at FROM outbox WHERE media_uri=?1 ON CONFLICT DO NOTHING").bind(previous).bind(current).execute(write.connection().await?).await?;
        sqlx::query("DELETE FROM outbox WHERE media_uri=?1")
            .bind(previous)
            .execute(write.connection().await?)
            .await?;
        write.keep();
    }
    for (table, key) in [
        ("playlists", "playlist_key"),
        ("smart_playlists", "smart_playlist_key"),
    ] {
        let keys: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("WITH aliases AS(
            SELECT json_extract(value,'$[0]') previous,json_extract(value,'$[1]') current FROM json_each(?1)
        ) UPDATE {table} AS owner SET mosaic=(
            SELECT json_group_array(uri ORDER BY ordinal) FROM (
                SELECT coalesce((SELECT current FROM aliases WHERE previous=chosen.value LIMIT 1),chosen.value) uri,
                       min(CAST(chosen.key AS INTEGER)) ordinal
                FROM json_each(owner.mosaic) chosen GROUP BY uri)
        ) WHERE EXISTS(
            SELECT 1 FROM json_each(owner.mosaic) chosen JOIN aliases ON aliases.previous=chosen.value) RETURNING {key}")))
            .bind(serde_json::to_string(aliases)?).fetch_all(write.connection().await?).await?;
        for key in keys {
            write.changed(if table == "playlists" {
                Scope::Playlist {
                    key: crate::PlaylistKey::from_raw(key),
                    entries: false,
                }
            } else {
                Scope::Smart {
                    key: crate::SmartPlaylistKey::from_raw(key),
                }
            });
        }
    }
    if merged {
        // Album resolution follows the folded rows.
        touched
            .tracks
            .extend(candidates.iter().map(|track| track.key.raw()));
        touched.albums.extend(
            candidates
                .iter()
                .filter_map(|track| track.album)
                .map(|album| album.raw()),
        );
        super::merge::resolve(write, touched).await?;
    }
    Ok(())
}
