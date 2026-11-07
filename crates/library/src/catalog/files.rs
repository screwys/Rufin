//! The file index of file sources: one row per listed file, the inputs of CUE sheets, and
//! the Write that a walk's staged files go through.

use super::merge::Touched;
use super::records::{Source, Track};
use super::write::Write;
use super::write::{Family, Scope};
use super::*;
use crate::{LocalFileKind, LocalFileState, LocalFileWrite, LocalLocatorWrite, TrackObservation};
use sqlx::{QueryBuilder, Sqlite};

const FILE_COLUMNS: &str = "path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state";

/// The walk order key: '/' sorts before every other character, so a directory's subtree
/// follows it directly, as a depth-first walk of name-sorted listings visits it.
pub fn walk_key(relative: &str) -> String {
    relative.replace('/', "\u{1}")
}

/// A stored file row, and whether an import locator names it.
#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct StoredFile {
    #[sqlx(flatten)]
    pub file: LocalFileWrite,
    pub imported: bool,
}

/// What a walk staged in one directory range: new and changed rows, the inputs of the CUE
/// sheets among them, vanished rows, and the tracks parsed from them.
#[derive(Debug, Default)]
pub struct FileBatch {
    pub files: Vec<LocalFileWrite>,
    /// An accepted CUE's path and its input paths, in sheet order.
    pub inputs: Vec<(String, Vec<String>)>,
    pub removed: Vec<LocalFileWrite>,
    /// Media paths whose whole-file tracks hide: an accepted CUE took them over, or the
    /// imported file is gone.
    pub hidden: Vec<String>,
    pub tracks: Vec<TrackObservation>,
    /// A vanished media path and the new path the same file was listed at.
    pub renames: Vec<(String, String)>,
    /// Unchanged files listed with a new device, inode or native id.
    pub identities: Vec<LocalFileWrite>,
}

impl FileBatch {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.removed.is_empty()
            && self.hidden.is_empty()
            && self.tracks.is_empty()
            && self.renames.is_empty()
            && self.identities.is_empty()
    }
}

impl super::Observation<'_> {
    /// Stored rows in walk order, from `start` on (`inclusive`) or after it. `walked` are
    /// the roots this walk lists; rows of the source's other `configured` roots are left out,
    /// and rows of roots no longer configured are included for the walk to vanish. A row no
    /// configured root holds is imported when an import locator names it or a CUE input.
    pub async fn file_rows(
        &self,
        walked: &[String],
        configured: &[String],
        start: Option<(&str, &str)>,
        inclusive: bool,
        limit: usize,
    ) -> LibraryResult<Vec<StoredFile>> {
        let mut connection = self.catalog.acquire_reader().await?;
        let (root, key) = start.map_or((String::new(), String::new()), |(root, relative)| {
            (root.to_owned(), walk_key(relative))
        });
        let import = "locator.origin='import' AND(locator.source_key IS NULL OR locator.source_key=file.source_key)";
        // Two arms, each a range of the walk index: the rest of the cursor's root, then the
        // roots after it. One OR'd cursor would scan the source from its first row.
        let arm = |cursor: &str| {
            format!(
                "SELECT * FROM(SELECT {FILE_COLUMNS},(EXISTS(SELECT 1 FROM local_locators locator WHERE locator.path=file.path AND {import})
                    OR file.kind='cue' AND EXISTS(SELECT 1 FROM file_inputs input JOIN local_locators locator ON locator.path=input.path WHERE input.file_key=file.file_key AND {import})) AND root NOT IN(SELECT value FROM json_each(?3)) imported
                 FROM files file WHERE source_key=?1
                   AND(root IN(SELECT value FROM json_each(?2)) OR root NOT IN(SELECT value FROM json_each(?3))) AND {cursor}
                 ORDER BY root,replace(relative_path,'/',char(1)) LIMIT ?6)"
            )
        };
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{} UNION ALL {} LIMIT ?6",
            arm("root=?4 AND replace(relative_path,'/',char(1))>=?5 AND(?7 OR replace(relative_path,'/',char(1))>?5)"),
            arm("root>?4")
        )))
        .bind(self.source)
        .bind(serde_json::to_string(walked)?)
        .bind(serde_json::to_string(configured)?)
        .bind(root)
        .bind(key)
        .bind(limit as i64)
        .bind(inclusive)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// One Write: file rows, CUE inputs, vanished rows deleted with their tracks hidden,
    /// tracks merged and admitted, and user data moved from the URIs a file left behind.
    /// Returns the inputs a CUE sheet released, which play as whole files again.
    pub async fn write_files(&mut self, batch: FileBatch) -> LibraryResult<Vec<String>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let playlist =
            |path: &String| crate::PlaylistFormat::from_path(std::path::Path::new(path)).is_some();
        self.playlist_files |= batch.files.iter().any(|file| playlist(&file.path))
            || batch.removed.iter().any(|file| playlist(&file.path))
            || batch
                .renames
                .iter()
                .any(|(old, new)| playlist(old) || playlist(new));
        let source = self.source;
        let mut write = self.write().await?;
        let mut touched = Touched::default();
        let cue = |file: &LocalFileWrite| file.kind == LocalFileKind::Cue;
        // CUE sheets that are read again or gone give up their inputs: a read sheet by its
        // path, whichever root it was stored under, and a gone row as the walk read it.
        let read = batch
            .files
            .iter()
            .filter(|file| cue(file) && file.state != LocalFileState::Unreadable)
            .map(|file| file.path.as_str())
            .chain(batch.inputs.iter().map(|(path, _)| path.as_str()))
            .collect::<Vec<_>>();
        let gone = batch
            .removed
            .iter()
            .filter(|file| cue(file))
            .map(|file| (&file.path, &file.root, &file.relative_path))
            .collect::<Vec<_>>();
        let mut released: Vec<String> = Vec::new();
        if !read.is_empty() || !gone.is_empty() {
            released = sqlx::query_scalar("DELETE FROM file_inputs WHERE file_key IN(SELECT file_key FROM files WHERE source_key=?1 AND path IN(SELECT value FROM json_each(?2))
                UNION ALL SELECT cue.file_key FROM json_each(?3) requested JOIN files cue ON cue.source_key=?1 AND cue.path=json_extract(requested.value,'$[0]') AND cue.root=json_extract(requested.value,'$[1]') AND cue.relative_path=json_extract(requested.value,'$[2]')) RETURNING path")
                .bind(source).bind(serde_json::to_string(&read)?).bind(serde_json::to_string(&gone)?).fetch_all(write.connection().await?).await?;
            released.sort_unstable();
            released.dedup();
        }
        // A row is deleted only as the walk read it: one another root's walk took over stays.
        let mut removed = Vec::new();
        if !batch.removed.is_empty() {
            let rows = batch
                .removed
                .iter()
                .map(|file| (&file.path, &file.root, &file.relative_path))
                .collect::<Vec<_>>();
            removed = sqlx::query_scalar("DELETE FROM files WHERE source_key=?1 AND(path,root,relative_path) IN(SELECT json_extract(value,'$[0]'),json_extract(value,'$[1]'),json_extract(value,'$[2]') FROM json_each(?2)) RETURNING path")
                .bind(source).bind(serde_json::to_string(&rows)?).fetch_all(write.connection().await?).await?;
        }
        for page in batch.files.chunks(128) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "INSERT INTO files(source_key,path,root,relative_path,kind,size_bytes,mtime_ns,device_id,inode,native_id,revision,picture_index,artist_pictures,parse_version,state) ",
            );
            query.push_values(page, |mut row, file| {
                row.push_bind(source)
                    .push_bind(&file.path)
                    .push_bind(&file.root)
                    .push_bind(&file.relative_path)
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
                    .push_bind(file.state);
            });
            query.push(" ON CONFLICT(source_key,path) DO UPDATE SET root=excluded.root,relative_path=excluded.relative_path,kind=excluded.kind,size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,device_id=excluded.device_id,inode=excluded.inode,native_id=excluded.native_id,revision=excluded.revision,picture_index=excluded.picture_index,artist_pictures=excluded.artist_pictures,parse_version=excluded.parse_version,state=excluded.state");
            query.build().execute(write.connection().await?).await?;
        }
        if !batch.identities.is_empty() {
            let rows = batch
                .identities
                .iter()
                .map(|file| (&file.path, file.device_id, file.inode, &file.native_id))
                .collect::<Vec<_>>();
            sqlx::query("UPDATE files SET device_id=json_extract(item.value,'$[1]'),inode=json_extract(item.value,'$[2]'),native_id=json_extract(item.value,'$[3]') FROM json_each(?2) item WHERE files.source_key=?1 AND files.path=json_extract(item.value,'$[0]')")
                .bind(source).bind(serde_json::to_string(&rows)?).execute(write.connection().await?).await?;
        }
        if !batch.inputs.is_empty() {
            sqlx::query("INSERT INTO file_inputs(file_key,position,path) SELECT cue.file_key,CAST(input.key AS INTEGER),input.value FROM json_each(?2) requested JOIN files cue ON cue.source_key=?1 AND cue.path=json_extract(requested.value,'$[0]') CROSS JOIN json_each(json_extract(requested.value,'$[1]')) input")
                .bind(source).bind(serde_json::to_string(&batch.inputs)?).execute(write.connection().await?).await?;
        }
        if !released.is_empty() {
            let owned: HashSet<String> = sqlx::query_scalar("SELECT input.path FROM json_each(?2) requested JOIN file_inputs input ON input.path=requested.value JOIN files cue USING(file_key) WHERE cue.source_key=?1 AND cue.state='accepted'")
                .bind(source).bind(serde_json::to_string(&released)?).fetch_all(write.connection().await?).await?.into_iter().collect();
            released.retain(|path| !owned.contains(path));
        }
        // A read file's tracks it no longer yields hide, as do the tracks of vanished files
        // and the whole-file tracks a CUE sheet took over. A failed read replaces nothing.
        let owners = batch
            .files
            .iter()
            .filter(|file| matches!(file.kind, LocalFileKind::Media | LocalFileKind::Cue))
            .filter(|file| file.state != LocalFileState::Unreadable)
            .map(|file| file.path.as_str())
            .chain(removed.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let incoming = batch
            .tracks
            .iter()
            .map(|track| track.object_id.as_str())
            .collect::<Vec<_>>();
        if !owners.is_empty() || !batch.hidden.is_empty() {
            let hidden: Vec<i64> = sqlx::query_scalar("WITH owners(path) AS(SELECT value FROM json_each(?2)),removed(path) AS(SELECT value FROM json_each(?3)),hidden(path) AS(SELECT value FROM json_each(?4)),candidates(track_key) AS(
                SELECT details.track_key FROM owners CROSS JOIN track_details details ON details.cue_path=owners.path
                UNION SELECT details.track_key FROM owners CROSS JOIN track_details details ON details.source_path=owners.path WHERE details.cue_path IS NULL
                UNION SELECT details.track_key FROM removed CROSS JOIN track_details details ON details.source_path=removed.path
                UNION SELECT details.track_key FROM hidden CROSS JOIN track_details details ON details.source_path=hidden.path WHERE details.cue_path IS NULL)
                SELECT track.track_key FROM candidates CROSS JOIN tracks track USING(track_key) WHERE track.source_key=?1 AND track.indexed=1 AND track.object_id NOT IN(SELECT value FROM json_each(?5)) ORDER BY track.track_key")
                .bind(source).bind(serde_json::to_string(&owners)?).bind(serde_json::to_string(&removed)?).bind(serde_json::to_string(&batch.hidden)?).bind(serde_json::to_string(&incoming)?)
                .fetch_all(write.connection().await?).await?;
            let hidden = hidden
                .into_iter()
                .filter(|key| !write.newer(Family::Track, *key))
                .collect::<Vec<_>>();
            write_track_membership(&mut write, &hidden, false, &mut touched).await?;
        }
        let source_id = self.source_id;
        super::merge::tracks(
            &mut write,
            source,
            source_id,
            &batch.tracks,
            true,
            true,
            &mut touched,
        )
        .await?;
        // Whole-file tracks of a CUE sheet's inputs move to the sheet's track when it plays
        // the whole file, and a renamed file's old track folds into the new one.
        let inputs = batch
            .inputs
            .iter()
            .flat_map(|(_, inputs)| inputs.iter().cloned())
            .collect::<Vec<_>>();
        let mut after = 0;
        while !inputs.is_empty() {
            let page = super::file_reads::referenced_file_page_on(
                write.connection().await?,
                source.raw(),
                &inputs,
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
                source.raw(),
                &files,
            )
            .await?;
            merge_local_file_uris_on(&mut write, &aliases, &mut touched).await?;
        }
        if !batch.renames.is_empty() {
            // A renamed file's track, or a renamed sheet's track of the same number.
            let aliases: Vec<(String, String, String)> = sqlx::query_as("SELECT old.media_uri,new.media_uri,new.object_id FROM json_each(?2) rename
                CROSS JOIN track_details old_details ON old_details.cue_path=json_extract(rename.value,'$[0]') OR old_details.source_path=json_extract(rename.value,'$[0]') AND old_details.cue_path IS NULL CROSS JOIN tracks old ON old.track_key=old_details.track_key AND(old.source_key=?1 OR old.source_key IS NULL)
                CROSS JOIN track_details new_details ON new_details.cue_path=json_extract(rename.value,'$[1]') AND old_details.cue_path IS NOT NULL OR new_details.source_path=json_extract(rename.value,'$[1]') AND new_details.cue_path IS NULL AND old_details.cue_path IS NULL CROSS JOIN tracks new ON new.track_key=new_details.track_key AND new.source_key=?1 AND new.indexed=1 AND(old_details.cue_path IS NULL OR new.track_number=old.track_number)
                WHERE old.media_uri<>new.media_uri AND new.object_id IS NOT NULL")
                .bind(source).bind(serde_json::to_string(&batch.renames)?).fetch_all(write.connection().await?).await?;
            merge_local_file_uris_on(&mut write, &aliases, &mut touched).await?;
        }
        if !batch.files.is_empty() || !removed.is_empty() || !batch.identities.is_empty() {
            // File rows are not resident. Folder pages list directories and the tracks in them.
            if batch
                .files
                .iter()
                .chain(
                    batch
                        .removed
                        .iter()
                        .filter(|file| removed.contains(&file.path)),
                )
                .any(|file| file.kind == LocalFileKind::Directory)
            {
                write.changed(Scope::Collections { source });
            }
        }
        super::merge::resolve(&mut write, &mut touched).await?;
        self.commit(write).await?;
        Ok(released)
    }

    /// Import locators for a playlist's tracks outside the `configured` roots, which keep them
    /// listed. A track inside a root hides and returns with its folder, entries kept.
    pub async fn record_imported_files(
        &mut self,
        playlist: crate::PlaylistKey,
        configured: &[String],
    ) -> LibraryResult<()> {
        let mut write = self.write().await?;
        let inserted = sqlx::query("INSERT INTO local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri)
            SELECT ?1,track.media_uri,'import',details.source_path,file.root,file.relative_path,file.path FROM playlist_entries entry CROSS JOIN tracks track USING(track_key) CROSS JOIN track_details details USING(track_key)
            CROSS JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
            WHERE entry.playlist_key=?2 AND track.source_key=?1 AND track.indexed=1 AND file.root NOT IN(SELECT value FROM json_each(?3))
            ON CONFLICT(media_uri,origin) DO NOTHING")
            .bind(self.source).bind(playlist).bind(serde_json::to_string(configured)?).execute(write.connection().await?).await?.rows_affected();
        if inserted != 0 {
            // Import locators are read by key only, so clients hear nothing of them.
        }
        self.commit(write).await
    }
}

impl Catalog {
    /// The accepted CUE sheets among `paths`' owners: (input path, sheet path).
    pub async fn cue_owners(
        &self,
        source: SourceKey,
        paths: &[&str],
    ) -> LibraryResult<Vec<(String, String)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT input.path,cue.path FROM json_each(?2) requested CROSS JOIN file_inputs input ON input.path=requested.value CROSS JOIN files cue USING(file_key) WHERE cue.source_key=?1 AND cue.kind='cue' AND cue.state='accepted'")
            .bind(source).bind(serde_json::to_string(paths)?).fetch_all(&mut *connection).await?)
    }

    /// The object ids of the tracks `paths` yield, by owner path, so a re-read keeps them.
    pub async fn file_track_objects(
        &self,
        source: SourceKey,
        paths: &[&str],
    ) -> LibraryResult<Vec<(String, String)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT requested.value,track.object_id FROM json_each(?2) requested CROSS JOIN track_details details ON details.cue_path=requested.value CROSS JOIN tracks track USING(track_key) WHERE track.source_key=?1 AND track.object_id IS NOT NULL
            UNION ALL SELECT requested.value,track.object_id FROM json_each(?2) requested CROSS JOIN track_details details ON details.source_path=requested.value AND details.cue_path IS NULL CROSS JOIN tracks track USING(track_key) WHERE track.source_key=?1 AND track.object_id IS NOT NULL")
            .bind(source).bind(serde_json::to_string(paths)?).fetch_all(&mut *connection).await?)
    }
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
pub(super) async fn merge_local_file_uris_on(
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
                && let Some(path) = sqlx::query_scalar::<_, String>(
                    "SELECT path FROM files WHERE source_key=?1 AND path=?2",
                )
                .bind(track.source.map(|source| source.raw()))
                .bind(
                    crate::file_media_path(current).map(|path| path.to_string_lossy().into_owned()),
                )
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
            // The track has been in the library since the old row was first seen.
            canonical.first_seen = canonical
                .first_seen
                .into_iter()
                .chain(previous_track.first_seen)
                .min();
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
        write.states_changed(states);
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
            sqlx::query("INSERT INTO downloads(media_uri,quality,size,path,reason,source_id,root,relative_path,managed,retired) SELECT ?2,quality,size,path,reason,source_id,root,relative_path,managed,retired FROM downloads WHERE media_uri=?1 ON CONFLICT DO NOTHING")
                .bind(previous).bind(current).execute(write.connection().await?).await?;
            write.read_downloads(&[current.as_str()]).await?;
        }
        if let Some(path) = crate::file_media_path(current) {
            let path = path.to_string_lossy().into_owned();
            // An imported file's locator follows it. A file a walk found renamed needs none:
            // its row backs the track, and a locator would keep it listed when its folder goes.
            let existing:Option<(Option<String>,String,String)>=sqlx::query_as("SELECT source.object_id,locator.root,locator.relative_path FROM local_locators locator LEFT JOIN sources source USING(source_key) WHERE locator.media_uri=?1 AND locator.origin='import'").bind(previous).fetch_optional(write.connection().await?).await?;
            if let Some((source_id, root, relative_path)) = existing {
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
            }
            sqlx::query("UPDATE local_locators SET access_uri=?2,path=?3 WHERE media_uri=?1 AND access_uri=?1 AND origin='local'")
                .bind(previous).bind(current).bind(crate::file_media_path(current).map(|path|path.to_string_lossy().into_owned())).execute(write.connection().await?).await?;
        }
        crate::lyrics::move_lyrics_on(write, previous, current).await?;
        sqlx::query("INSERT INTO outbox(media_uri,kind,value,next_attempt_at) SELECT ?2,kind,value,next_attempt_at FROM outbox WHERE media_uri=?1 ON CONFLICT DO NOTHING").bind(previous).bind(current).execute(write.connection().await?).await?;
        sqlx::query("DELETE FROM outbox WHERE media_uri=?1")
            .bind(previous)
            .execute(write.connection().await?)
            .await?;
    }
    for (table, key) in [
        ("playlists", "playlist_key"),
        ("smart_playlists", "smart_playlist_key"),
    ] {
        let keys: Vec<(i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!("WITH aliases AS(
            SELECT json_extract(value,'$[0]') previous,json_extract(value,'$[1]') current FROM json_each(?1)
        ) UPDATE {table} AS owner SET mosaic=(
            SELECT json_group_array(uri ORDER BY ordinal) FROM (
                SELECT coalesce((SELECT current FROM aliases WHERE previous=chosen.value LIMIT 1),chosen.value) uri,
                       min(CAST(chosen.key AS INTEGER)) ordinal
                FROM json_each(owner.mosaic) chosen GROUP BY uri)
        ) WHERE EXISTS(
            SELECT 1 FROM json_each(owner.mosaic) chosen JOIN aliases ON aliases.previous=chosen.value) RETURNING {key},object_id")))
            .bind(serde_json::to_string(aliases)?).fetch_all(write.connection().await?).await?;
        for (key, object) in keys {
            write.changed(if table == "playlists" {
                Scope::Playlist {
                    key: crate::PlaylistKey::from_raw(key),
                    entries: false,
                }
            } else {
                Scope::Smart {
                    key: crate::SmartPlaylistKey::from_raw(key),
                    object,
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
