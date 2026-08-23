use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::UNIX_EPOCH;

use library::{
    Album, AlbumId, AlbumRelations, Artist, ArtistCredit, ArtistId, CandidateBatch, Genre, GenreId,
    LocalComponentBaseline, LocalComponentReplacement, LocalComponentSeed, LocalFile,
    LocalFileBaseline, LocalFileKind, LocalFileSeed, LocalFileState, Track,
};
use walkdir::WalkDir;

use super::artwork;
use super::cue::{CueFile, CueSheet, CueTrack, parse_cue_sheet};
use super::media::{self, MediaRead, ScannedTrack};
use crate::source::{BatchEmitter, SourceReadProgress, SourceReadStage};
use crate::{SourceError, SourceResult};

const LOCAL_LIBRARY_PARSER_VERSION: u32 = 8;
const LOCAL_ACCESS_PARSER_VERSION: u32 = 6;
const LOCAL_CUE_MAX_BYTES: usize = 1024 * 1024;
const LOCAL_BATCH_SIZE: usize = 1024;

pub(super) struct LocalCheck {
    file_seeds: Vec<LocalFileSeed>,
    inventory: Inventory,
    cue_sheets: HashMap<String, Option<CueSheet>>,
}

impl LocalCheck {
    pub(super) fn file_seeds(&self) -> &[LocalFileSeed] {
        &self.file_seeds
    }
}

pub(super) struct LocalChange {
    component_seeds: Vec<LocalComponentSeed>,
    inventory: Inventory,
    changed_paths: BTreeSet<String>,
    affected_paths: HashSet<String>,
    changed_facts: CollectedFacts,
}

impl LocalChange {
    pub(super) fn component_seeds(&self) -> &[LocalComponentSeed] {
        &self.component_seeds
    }
}

struct Inventory {
    entries: BTreeMap<String, InventoryEntry>,
    artwork_by_directory: BTreeMap<PathBuf, library::LocalArtworkRef>,
    accepted_media_counts_by_directory: BTreeMap<PathBuf, usize>,
}

struct InventoryEntry {
    path: PathBuf,
    file: LocalFile,
}

#[derive(Clone)]
struct CuePlan {
    cue_path: PathBuf,
    album_title: Option<String>,
    album_performer: Option<String>,
    files: Vec<CueFile>,
}

struct ParsedCues {
    files: Vec<LocalFile>,
    plans: Vec<CuePlan>,
}

struct MediaJob {
    paths: Vec<PathBuf>,
    cues: Vec<CuePlan>,
}

struct MediaJobResult {
    reads: Vec<(PathBuf, MediaRead)>,
    cues: Vec<CuePlan>,
}

trait FactOutput {
    fn emit(&mut self, batch: CandidateBatch) -> SourceResult<()>;
}

struct EmitterOutput<'a> {
    emitter: &'a BatchEmitter,
}

impl FactOutput for EmitterOutput<'_> {
    fn emit(&mut self, batch: CandidateBatch) -> SourceResult<()> {
        self.emitter.emit(batch)
    }
}

#[derive(Default)]
struct CollectedFacts {
    albums: Vec<Album>,
    tracks: Vec<Track>,
    artists: Vec<Artist>,
    genres: Vec<Genre>,
    files: Vec<LocalFile>,
}

impl FactOutput for CollectedFacts {
    fn emit(&mut self, batch: CandidateBatch) -> SourceResult<()> {
        match batch {
            CandidateBatch::Albums(values) => self.albums.extend(values),
            CandidateBatch::Tracks(values) => self.tracks.extend(values),
            CandidateBatch::Artists(values) => self.artists.extend(values),
            CandidateBatch::Genres(values) => self.genres.extend(values),
            CandidateBatch::LocalFiles(values) => self.files.extend(values),
            CandidateBatch::MusicFolders(_) | CandidateBatch::Playlists(_) => {
                return Err(SourceError::Other(
                    "Local scan emitted a remote-only fact".to_string(),
                ));
            }
        }
        Ok(())
    }
}

pub(super) fn acquire_complete(
    roots: &[PathBuf],
    emitter: &BatchEmitter,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let inventory = inventory(roots, progress, cancelled)?;
    let mut output = EmitterOutput { emitter };
    scan_inventory(
        &inventory,
        None,
        &HashMap::new(),
        &mut output,
        progress,
        cancelled,
    )
}

pub(super) fn acquire_local_access(
    root: &Path,
    baseline: &[library::LocalAccessFile],
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<Vec<library::LocalAccessFile>> {
    let inventory = inventory_entries(
        &[root.to_path_buf()],
        |kind| kind == LocalFileKind::Media,
        progress,
        cancelled,
    )?;
    let root_text = root.to_string_lossy().into_owned();
    let baseline = baseline
        .iter()
        .filter(|file| file.root == root_text)
        .map(|file| (file.path.clone(), file))
        .collect::<HashMap<_, _>>();
    let mut accepted = BTreeMap::new();
    let mut changed = Vec::new();
    for (path, entry) in &inventory {
        match baseline.get(path) {
            Some(previous) if local_access_file_matches(&entry.file, previous) => {
                accepted.insert(path.clone(), (*previous).clone());
            }
            _ => changed.push(entry.path.clone()),
        }
    }
    if changed.is_empty() {
        return Ok(accepted.into_values().collect());
    }
    let total = changed.len();
    progress(SourceReadProgress {
        stage: SourceReadStage::Tracks,
        completed: 0,
        total: Some(total),
    });
    let mut completed = 0;
    run_ordered_jobs(
        changed,
        media::Worker::default,
        |worker, path| {
            let metadata = media::read_basic_audio(worker, path.clone());
            Ok((path, metadata))
        },
        |(path, metadata)| {
            completed += 1;
            if let Some(metadata) = metadata {
                let entry = inventory
                    .get(path.to_string_lossy().as_ref())
                    .expect("a Local-access media path comes from its inventory");
                let file = library::LocalAccessFile {
                    path: entry.file.path.clone(),
                    root: entry.file.root.clone(),
                    relative_path: entry.file.relative_path.clone(),
                    size_bytes: entry.file.size_bytes.unwrap_or_default(),
                    mtime_ns: entry.file.mtime_ns,
                    device_id: entry.file.device_id,
                    inode: entry.file.inode,
                    parser_version: LOCAL_ACCESS_PARSER_VERSION,
                    title: metadata.title,
                    album: metadata.album,
                    artist: metadata.artist,
                    disc_number: metadata.disc_number,
                    track_number: metadata.track_number,
                    duration_seconds: metadata.duration_seconds,
                };
                accepted.insert(file.path.clone(), file);
            }
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: Some(total),
            });
            Ok(())
        },
        cancelled,
    )?;
    progress(SourceReadProgress {
        stage: SourceReadStage::Finalizing,
        completed: 1,
        total: Some(1),
    });
    Ok(accepted.into_values().collect())
}

fn local_access_file_matches(current: &LocalFile, accepted: &library::LocalAccessFile) -> bool {
    current.size_bytes == Some(accepted.size_bytes)
        && current.mtime_ns == accepted.mtime_ns
        && current.device_id == accepted.device_id
        && current.inode == accepted.inode
        && accepted.parser_version == LOCAL_ACCESS_PARSER_VERSION
}

pub(super) fn check_automatic(
    roots: &[PathBuf],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<LocalCheck> {
    let file_seeds = roots
        .iter()
        .map(|root| LocalFileSeed::DirectoryTree(root.to_string_lossy().into_owned()))
        .collect();
    Ok(LocalCheck {
        file_seeds,
        inventory: inventory(roots, &|_| {}, cancelled)?,
        cue_sheets: HashMap::new(),
    })
}

pub(super) fn check_exact(
    roots: &[PathBuf],
    evidence: BTreeSet<PathBuf>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<LocalCheck> {
    let (file_seeds, cue_sheets) = exact_file_seeds(roots, evidence);
    let inventory = inventory_for_file_seeds(roots, &file_seeds, cancelled)?;
    Ok(LocalCheck {
        file_seeds,
        inventory,
        cue_sheets,
    })
}

pub(super) fn confirm_change(
    mut check: LocalCheck,
    baseline: LocalFileBaseline,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<Option<LocalChange>> {
    let LocalFileBaseline {
        files: accepted_files,
        tracked_media_paths,
        accepted_media_counts_by_directory,
    } = baseline;
    observe_accepted_files(&mut check.inventory, &accepted_files, cancelled)?;
    let changed_paths = changed_paths(&check.inventory, &accepted_files);
    if changed_paths.is_empty() {
        return Ok(None);
    }
    read_changed_cues(
        &changed_paths,
        &check.inventory,
        &mut check.cue_sheets,
        cancelled,
    )?;
    let affected_paths = selected_change_paths(
        &changed_paths,
        &accepted_files,
        &check.inventory,
        &check.cue_sheets,
    );
    read_changed_cues(
        &affected_paths,
        &check.inventory,
        &mut check.cue_sheets,
        cancelled,
    )?;
    let mut changed_facts = CollectedFacts::default();
    scan_inventory(
        &check.inventory,
        Some(affected_paths.clone()),
        &check.cue_sheets,
        &mut changed_facts,
        progress,
        cancelled,
    )?;
    apply_accepted_media_counts(
        &mut check.inventory,
        &accepted_media_counts_by_directory,
        &tracked_media_paths,
        &changed_paths,
        &changed_facts.files,
    );
    let component_seeds = component_seeds(
        &changed_paths,
        &accepted_files,
        &check.inventory,
        &check.cue_sheets,
        &changed_facts,
    );
    Ok(Some(LocalChange {
        component_seeds,
        inventory: check.inventory,
        changed_paths,
        affected_paths,
        changed_facts,
    }))
}

pub(super) fn complete_change(
    mut change: LocalChange,
    baseline: LocalComponentBaseline,
    observed_at: i64,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<LocalComponentReplacement> {
    observe_accepted_files(&mut change.inventory, &baseline.files, cancelled)?;
    build_local_replacement(change, baseline, observed_at)
}

fn inventory(
    roots: &[PathBuf],
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<Inventory> {
    let entries = inventory_entries(roots, |_| true, progress, cancelled)?;
    Ok(inventory_from_entries(entries))
}

fn exact_file_seeds(
    roots: &[PathBuf],
    evidence: BTreeSet<PathBuf>,
) -> (Vec<LocalFileSeed>, HashMap<String, Option<CueSheet>>) {
    let mut seeds = Vec::new();
    let mut seen = HashSet::new();
    let mut cue_sheets = HashMap::new();
    for path in evidence
        .into_iter()
        .filter(|path| super::path_is_in_roots(path, roots))
    {
        push_seed(
            &mut seeds,
            &mut seen,
            LocalFileSeed::Path(path.to_string_lossy().into_owned()),
        );
        // A directory notification may be the only evidence for an added or
        // removed subtree. Its album can still use artwork in any parent up to
        // the configured root, so carry the same bounded parent facts as a
        // file notification.
        push_artwork_ancestor_seeds(roots, &path, &mut seeds, &mut seen);
        if path.is_dir() {
            push_seed(
                &mut seeds,
                &mut seen,
                LocalFileSeed::DirectoryTree(path.to_string_lossy().into_owned()),
            );
        }
        if artwork::supported_image(&path)
            && let Some(parent) = path.parent()
            && super::path_is_in_roots(parent, roots)
        {
            push_seed(
                &mut seeds,
                &mut seen,
                LocalFileSeed::ArtworkDirectory(parent.to_string_lossy().into_owned()),
            );
        }
        if is_cue(&path) {
            let path_text = path.to_string_lossy().into_owned();
            let sheet = read_cue(&path);
            if let Some(sheet) = &sheet {
                for file in &sheet.files {
                    if !super::path_is_in_roots(&file.path, roots) {
                        continue;
                    }
                    push_seed(
                        &mut seeds,
                        &mut seen,
                        LocalFileSeed::Path(file.path.to_string_lossy().into_owned()),
                    );
                    push_artwork_ancestor_seeds(roots, &file.path, &mut seeds, &mut seen);
                }
            }
            cue_sheets.insert(path_text, sheet);
        }
    }
    (seeds, cue_sheets)
}

fn push_artwork_ancestor_seeds(
    roots: &[PathBuf],
    path: &Path,
    seeds: &mut Vec<LocalFileSeed>,
    seen: &mut HashSet<LocalFileSeed>,
) {
    let Some(root) = roots
        .iter()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
    else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    for directory in parent.ancestors().take_while(|path| path.starts_with(root)) {
        push_seed(
            seeds,
            seen,
            LocalFileSeed::ArtworkDirectory(directory.to_string_lossy().into_owned()),
        );
    }
}

fn inventory_for_file_seeds(
    roots: &[PathBuf],
    seeds: &[LocalFileSeed],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<Inventory> {
    let mut entries = BTreeMap::new();
    let mut checked_roots = HashSet::new();
    for seed in seeds {
        check_cancelled(cancelled)?;
        let (path, depth) = match seed {
            LocalFileSeed::Path(path) => (Path::new(path), Some(0)),
            LocalFileSeed::DirectoryTree(path) => (Path::new(path), None),
            LocalFileSeed::ArtworkDirectory(_) => continue,
        };
        let Some(root) = roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.as_os_str().len())
        else {
            continue;
        };
        if checked_roots.insert(root.clone()) {
            require_root(root)?;
        }
        if !path.exists() {
            continue;
        }
        let mut walk = WalkDir::new(path).follow_links(true).sort_by_file_name();
        if let Some(depth) = depth {
            walk = walk.max_depth(depth);
        }
        for entry in walk.into_iter() {
            check_cancelled(cancelled)?;
            let entry = entry.map_err(|error| {
                SourceError::Other(format!(
                    "Could not completely read {}: {error}",
                    path.display()
                ))
            })?;
            let path = entry.path();
            let Some(kind) = recognized_file(
                path,
                entry.file_type().is_dir(),
                entry.file_type().is_file(),
            ) else {
                continue;
            };
            let metadata = entry.metadata().map_err(|error| {
                SourceError::Other(format!("Could not inspect {}: {error}", path.display()))
            })?;
            let path = path.to_path_buf();
            let path_text = path.to_string_lossy().into_owned();
            entries.entry(path_text).or_insert_with(|| InventoryEntry {
                file: local_file(root, &path, kind, &metadata),
                path,
            });
        }
    }
    Ok(inventory_from_entries(entries))
}

fn inventory_from_entries(entries: BTreeMap<String, InventoryEntry>) -> Inventory {
    let mut images_by_directory = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    for entry in entries.values() {
        if entry.file.kind == LocalFileKind::Image
            && let Some(directory) = entry.path.parent()
        {
            images_by_directory
                .entry(directory.to_path_buf())
                .or_default()
                .push(entry.path.clone());
        }
    }
    for images in images_by_directory.values_mut() {
        images.sort();
        images.dedup();
    }
    let artwork_by_directory = images_by_directory
        .into_iter()
        .filter_map(|(directory, images)| {
            let path = artwork::sidecar(&images)?;
            let file = entries.get(path.to_string_lossy().as_ref())?;
            Some((
                directory,
                artwork::file_reference(&path, file_revision(&file.file)),
            ))
        })
        .collect();
    Inventory {
        entries,
        artwork_by_directory,
        accepted_media_counts_by_directory: BTreeMap::new(),
    }
}

fn inventory_entries(
    roots: &[PathBuf],
    include: impl Fn(LocalFileKind) -> bool,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<BTreeMap<String, InventoryEntry>> {
    progress(SourceReadProgress {
        stage: SourceReadStage::Files,
        completed: 0,
        total: None,
    });
    let mut entries = BTreeMap::new();
    let mut visited = 0;
    for root in roots {
        require_root(root)?;
        for entry in WalkDir::new(root)
            .follow_links(true)
            .sort_by_file_name()
            .into_iter()
        {
            check_cancelled(cancelled)?;
            let entry = entry.map_err(|error| {
                SourceError::Other(format!(
                    "Could not completely read {}: {error}",
                    root.display()
                ))
            })?;
            let path = entry.path();
            let recognized = recognized_file(
                path,
                entry.file_type().is_dir(),
                entry.file_type().is_file(),
            );
            let Some(kind) = recognized.filter(|kind| include(*kind)) else {
                continue;
            };
            let metadata = entry.metadata().map_err(|error| {
                SourceError::Other(format!("Could not inspect {}: {error}", path.display()))
            })?;
            let path = path.to_path_buf();
            let path_text = path.to_string_lossy().into_owned();
            let file = local_file(root, &path, kind, &metadata);
            entries
                .entry(path_text)
                .or_insert_with(|| InventoryEntry { path, file });
            visited += 1;
            if visited % LOCAL_BATCH_SIZE == 0 {
                progress(SourceReadProgress {
                    stage: SourceReadStage::Files,
                    completed: visited,
                    total: None,
                });
            }
        }
    }
    progress(SourceReadProgress {
        stage: SourceReadStage::Files,
        completed: visited,
        total: Some(visited),
    });
    Ok(entries)
}

fn local_file(root: &Path, path: &Path, kind: LocalFileKind, metadata: &fs::Metadata) -> LocalFile {
    LocalFile {
        path: path.to_string_lossy().into_owned(),
        root: root.to_string_lossy().into_owned(),
        relative_path: path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned(),
        kind,
        size_bytes: (kind != LocalFileKind::Directory).then_some(metadata.len()),
        mtime_ns: modified_ns(metadata),
        device_id: metadata_device(metadata),
        inode: metadata_inode(metadata),
        parse_version: matches!(kind, LocalFileKind::Media | LocalFileKind::Cue)
            .then_some(LOCAL_LIBRARY_PARSER_VERSION),
        state: if kind == LocalFileKind::Directory || kind == LocalFileKind::Image {
            LocalFileState::Observed
        } else {
            LocalFileState::Accepted
        },
        dependencies: Vec::new(),
    }
}

fn observe_accepted_files(
    inventory: &mut Inventory,
    baseline: &[LocalFile],
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    for accepted in baseline {
        check_cancelled(cancelled)?;
        if inventory.entries.contains_key(&accepted.path) {
            continue;
        }
        let path = PathBuf::from(&accepted.path);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SourceError::Other(format!(
                    "Could not inspect {}: {error}",
                    path.display()
                )));
            }
        };
        let Some(kind) = recognized_file(&path, metadata.is_dir(), metadata.is_file()) else {
            continue;
        };
        let current = local_file(Path::new(&accepted.root), &path, kind, &metadata);
        let file = if local_file_matches(&current, accepted) {
            accepted.clone()
        } else {
            current
        };
        inventory
            .entries
            .insert(accepted.path.clone(), InventoryEntry { path, file });
    }
    let entries = std::mem::take(&mut inventory.entries);
    let accepted_media_counts = std::mem::take(&mut inventory.accepted_media_counts_by_directory);
    *inventory = inventory_from_entries(entries);
    inventory
        .accepted_media_counts_by_directory
        .extend(accepted_media_counts);
    Ok(())
}

fn apply_accepted_media_counts(
    inventory: &mut Inventory,
    accepted_counts: &BTreeMap<String, usize>,
    tracked_media_paths: &BTreeSet<String>,
    changed_paths: &BTreeSet<String>,
    changed_files: &[LocalFile],
) {
    let changed_files = changed_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    for (directory, accepted_count) in accepted_counts {
        let directory = Path::new(directory);
        let mut count = *accepted_count;
        for path in changed_paths
            .iter()
            .filter(|path| Path::new(path).starts_with(directory))
        {
            let was_accepted = tracked_media_paths.contains(path);
            let is_accepted = changed_files.get(path.as_str()).is_some_and(|file| {
                file.kind == LocalFileKind::Media
                    && (file.state == LocalFileState::Accepted
                        || (file.state == LocalFileState::Unreadable && was_accepted))
            });
            match (was_accepted, is_accepted) {
                (true, false) => count = count.saturating_sub(1),
                (false, true) => count += 1,
                _ => {}
            }
        }
        inventory
            .accepted_media_counts_by_directory
            .insert(directory.to_path_buf(), count);
    }
}

fn changed_paths(inventory: &Inventory, baseline: &[LocalFile]) -> BTreeSet<String> {
    let baseline = baseline
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    let mut paths = BTreeSet::new();
    for (path, current) in &inventory.entries {
        if baseline
            .get(path.as_str())
            .is_none_or(|accepted| !local_file_matches(&current.file, accepted))
        {
            paths.insert(path.clone());
        }
    }
    paths.extend(
        baseline
            .into_keys()
            .filter(|path| !inventory.entries.contains_key(*path))
            .map(str::to_string),
    );
    paths
}

fn local_file_matches(current: &LocalFile, accepted: &LocalFile) -> bool {
    if current.kind != accepted.kind
        || current.device_id != accepted.device_id
        || current.inode != accepted.inode
    {
        return false;
    }
    if current.kind == LocalFileKind::Directory {
        return true;
    }
    current.size_bytes == accepted.size_bytes
        && current.mtime_ns == accepted.mtime_ns
        && current.parse_version == accepted.parse_version
}

fn push_seed(
    seeds: &mut Vec<LocalFileSeed>,
    seen: &mut HashSet<LocalFileSeed>,
    seed: LocalFileSeed,
) {
    if seen.insert(seed.clone()) {
        seeds.push(seed);
    }
}

fn selected_change_paths(
    changed_paths: &BTreeSet<String>,
    accepted_files: &[LocalFile],
    inventory: &Inventory,
    cue_sheets: &HashMap<String, Option<CueSheet>>,
) -> HashSet<String> {
    let mut selected = changed_paths.iter().cloned().collect::<HashSet<_>>();
    let mut pending = changed_paths.iter().cloned().collect::<VecDeque<_>>();
    let accepted = accepted_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    let mut cue_dependents = HashMap::<&str, Vec<&str>>::new();
    for file in accepted_files {
        if file.kind == LocalFileKind::Cue {
            for dependency in &file.dependencies {
                cue_dependents
                    .entry(dependency)
                    .or_default()
                    .push(file.path.as_str());
            }
        }
    }

    while let Some(path) = pending.pop_front() {
        if let Some(file) = accepted.get(path.as_str())
            && file.kind == LocalFileKind::Cue
        {
            for dependency in &file.dependencies {
                if selected.insert(dependency.clone()) {
                    pending.push_back(dependency.clone());
                }
            }
        }
        if let Some(entry) = inventory.entries.get(&path)
            && entry.file.kind == LocalFileKind::Cue
            && let Some(Some(sheet)) = cue_sheets.get(&path)
        {
            for dependency in sheet
                .files
                .iter()
                .map(|file| file.path.to_string_lossy().into_owned())
            {
                if selected.insert(dependency.clone()) {
                    pending.push_back(dependency);
                }
            }
        }
        for cue in cue_dependents.get(path.as_str()).into_iter().flatten() {
            if selected.insert((*cue).to_string()) {
                pending.push_back((*cue).to_string());
            }
        }
    }
    selected
}

fn component_seeds(
    changed_paths: &BTreeSet<String>,
    accepted_files: &[LocalFile],
    inventory: &Inventory,
    cue_sheets: &HashMap<String, Option<CueSheet>>,
    changed_facts: &CollectedFacts,
) -> Vec<LocalComponentSeed> {
    let accepted = accepted_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    let mut seeds = Vec::new();
    let mut seen = HashSet::new();
    for path in changed_paths {
        push_component_seed(
            &mut seeds,
            &mut seen,
            LocalComponentSeed::Path(path.clone()),
        );
        let old_kind = accepted.get(path.as_str()).map(|file| file.kind);
        let current_kind = inventory.entries.get(path).map(|entry| entry.file.kind);
        if old_kind == Some(LocalFileKind::Directory)
            || current_kind == Some(LocalFileKind::Directory)
        {
            push_component_seed(
                &mut seeds,
                &mut seen,
                LocalComponentSeed::DirectoryTree(path.clone()),
            );
        }
        if (old_kind == Some(LocalFileKind::Image) || current_kind == Some(LocalFileKind::Image))
            && let Some(parent) = Path::new(path).parent()
        {
            push_component_seed(
                &mut seeds,
                &mut seen,
                LocalComponentSeed::ArtworkDirectory(parent.to_string_lossy().into_owned()),
            );
        }
        if let Some(file) = accepted.get(path.as_str())
            && file.kind == LocalFileKind::Cue
        {
            for dependency in &file.dependencies {
                push_component_seed(
                    &mut seeds,
                    &mut seen,
                    LocalComponentSeed::Path(dependency.clone()),
                );
            }
        }
        if let Some(entry) = inventory.entries.get(path)
            && entry.file.kind == LocalFileKind::Cue
            && let Some(Some(sheet)) = cue_sheets.get(path)
        {
            for dependency in sheet
                .files
                .iter()
                .map(|file| file.path.to_string_lossy().into_owned())
            {
                push_component_seed(&mut seeds, &mut seen, LocalComponentSeed::Path(dependency));
            }
        }
    }
    for album in &changed_facts.albums {
        push_component_seed(
            &mut seeds,
            &mut seen,
            LocalComponentSeed::Album(album.id.clone()),
        );
    }
    seeds
}

fn read_changed_cues<'a>(
    paths: impl IntoIterator<Item = &'a String>,
    inventory: &Inventory,
    cue_sheets: &mut HashMap<String, Option<CueSheet>>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    for path in paths {
        check_cancelled(cancelled)?;
        if cue_sheets.contains_key(path) {
            continue;
        }
        let Some(entry) = inventory
            .entries
            .get(path)
            .filter(|entry| entry.file.kind == LocalFileKind::Cue)
        else {
            continue;
        };
        cue_sheets.insert(path.clone(), read_cue(&entry.path));
    }
    Ok(())
}

fn push_component_seed(
    seeds: &mut Vec<LocalComponentSeed>,
    seen: &mut HashSet<LocalComponentSeed>,
    seed: LocalComponentSeed,
) {
    if seen.insert(seed.clone()) {
        seeds.push(seed);
    }
}

fn build_local_replacement(
    change: LocalChange,
    baseline: LocalComponentBaseline,
    observed_at: i64,
) -> SourceResult<LocalComponentReplacement> {
    let LocalChange {
        component_seeds,
        inventory,
        changed_paths,
        affected_paths,
        mut changed_facts,
    } = change;
    let baseline_albums = baseline
        .albums
        .iter()
        .map(|album| (album.id.clone(), album))
        .collect::<HashMap<_, _>>();
    let changed_albums = changed_facts
        .albums
        .iter()
        .map(|album| (album.id.clone(), album))
        .collect::<HashMap<_, _>>();
    let unreadable_audio = changed_facts
        .files
        .iter()
        .filter(|file| {
            file.kind == LocalFileKind::Media && file.state == LocalFileState::Unreadable
        })
        .map(|file| file.path.as_str())
        .collect::<HashSet<_>>();
    let mut final_tracks = baseline
        .tracks
        .iter()
        .cloned()
        .map(|track| (track.id.clone(), track))
        .collect::<HashMap<_, _>>();
    let affected_old_tracks = baseline
        .tracks
        .iter()
        .filter(|track| track_owner_changed(track, &affected_paths))
        .map(|track| track.id.clone())
        .collect::<HashSet<_>>();
    for track in &baseline.tracks {
        let retained_after_read_failure = track
            .source_path
            .as_deref()
            .is_some_and(|path| unreadable_audio.contains(path));
        if affected_old_tracks.contains(&track.id) && !retained_after_read_failure {
            final_tracks.remove(&track.id);
        }
    }

    let mut emitted_tracks = std::mem::take(&mut changed_facts.tracks);
    for track in &emitted_tracks {
        final_tracks.insert(track.id.clone(), track.clone());
    }

    let artwork_directories = component_seeds
        .iter()
        .filter_map(|seed| match seed {
            LocalComponentSeed::ArtworkDirectory(path) => Some(PathBuf::from(path)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !artwork_directories.is_empty() {
        for track in final_tracks.values_mut() {
            let Some(source_path) = track.source_path.as_deref() else {
                continue;
            };
            let Some(directory) = Path::new(source_path).parent() else {
                continue;
            };
            if !artwork_directories
                .iter()
                .any(|changed| directory == changed)
            {
                continue;
            }
            if matches!(
                track.local_artwork,
                Some(library::LocalArtworkRef::Embedded { .. })
            ) {
                continue;
            }
            let replacement = inventory.artwork_by_directory.get(directory).cloned();
            if track.local_artwork != replacement {
                track.local_artwork = replacement;
                emitted_tracks.push(track.clone());
            }
        }
    }
    emitted_tracks.sort_by(|left, right| left.id.cmp(&right.id));
    emitted_tracks.dedup_by(|left, right| left.id == right.id);

    let accepted_album_artwork = baseline
        .albums
        .iter()
        .filter_map(|album| {
            album
                .local_artwork
                .clone()
                .map(|artwork| (album.id.clone(), artwork))
        })
        .collect();
    let mut aggregates = Aggregates::new(&inventory, accepted_album_artwork);
    let mut aggregate_tracks = final_tracks.values().collect::<Vec<_>>();
    aggregate_tracks.sort_by(|left, right| {
        left.source_path
            .cmp(&right.source_path)
            .then_with(|| {
                left.cue
                    .as_ref()
                    .map(|cue| cue.cue_path.as_str())
                    .cmp(&right.cue.as_ref().map(|cue| cue.cue_path.as_str()))
            })
            .then(left.disc_number.cmp(&right.disc_number))
            .then(left.track_number.cmp(&right.track_number))
            .then(left.id.cmp(&right.id))
    });
    for track in aggregate_tracks {
        let album = track
            .album_id
            .as_ref()
            .and_then(|id| changed_albums.get(id).copied())
            .or_else(|| {
                track
                    .album_id
                    .as_ref()
                    .and_then(|id| baseline_albums.get(id).copied())
            });
        aggregates.add(&scanned_from_track(track, album));
    }
    let mut aggregate_facts = CollectedFacts::default();
    aggregates.finish(&inventory, &mut aggregate_facts)?;

    let current_track_ids = emitted_tracks
        .iter()
        .map(|track| &track.id)
        .collect::<HashSet<_>>();
    let final_track_ids = final_tracks.keys().collect::<HashSet<_>>();
    let removed_track_ids = sorted(
        affected_old_tracks
            .into_iter()
            .filter(|id| !final_track_ids.contains(id) && !current_track_ids.contains(id)),
    );

    let final_album_ids = aggregate_facts
        .albums
        .iter()
        .map(|album| &album.id)
        .collect::<HashSet<_>>();
    let removed_album_ids = sorted(
        baseline
            .albums
            .iter()
            .filter(|album| !final_album_ids.contains(&album.id))
            .map(|album| album.id.clone()),
    );
    let (old_artist_ids, old_genre_ids) =
        relationship_ids(baseline.tracks.iter(), baseline.albums.iter());
    let new_artist_ids = aggregate_facts
        .artists
        .iter()
        .map(|artist| artist.id.clone())
        .collect::<HashSet<_>>();
    let new_genre_ids = aggregate_facts
        .genres
        .iter()
        .map(|genre| genre.id.clone())
        .collect::<HashSet<_>>();

    let current_paths = inventory.entries.keys().collect::<HashSet<_>>();
    let removed_paths = sorted(
        baseline
            .files
            .iter()
            .filter(|file| {
                changed_paths.contains(&file.path) && !current_paths.contains(&file.path)
            })
            .map(|file| file.path.clone()),
    );
    let mut files = changed_facts
        .files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<BTreeMap<_, _>>();
    for path in &changed_paths {
        if let Some(entry) = inventory.entries.get(path)
            && matches!(
                entry.file.kind,
                LocalFileKind::Directory | LocalFileKind::Image
            )
        {
            files
                .entry(path.clone())
                .or_insert_with(|| entry.file.clone());
        }
    }

    Ok(LocalComponentReplacement {
        observed_at,
        files: files.into_values().collect(),
        removed_paths,
        albums: aggregate_facts.albums,
        tracks: emitted_tracks,
        artists: aggregate_facts.artists,
        genres: aggregate_facts.genres,
        removed_album_ids,
        removed_track_ids,
        removed_artist_ids: sorted(
            old_artist_ids
                .into_iter()
                .filter(|id| !new_artist_ids.contains(id)),
        ),
        removed_genre_ids: sorted(
            old_genre_ids
                .into_iter()
                .filter(|id| !new_genre_ids.contains(id)),
        ),
    })
}

fn track_owner_changed(track: &Track, affected_paths: &HashSet<String>) -> bool {
    track
        .source_path
        .as_ref()
        .is_some_and(|path| affected_paths.contains(path))
        || track
            .cue
            .as_ref()
            .is_some_and(|cue| affected_paths.contains(&cue.cue_path))
}

fn scanned_from_track(track: &Track, album: Option<&Album>) -> ScannedTrack {
    ScannedTrack {
        track: track.clone(),
        album_artist: album
            .map(|album| album.artist.clone())
            .or_else(|| {
                track
                    .relations
                    .album_artists
                    .first()
                    .map(|credit| credit.name.clone())
            })
            .unwrap_or_else(|| track.artist.clone()),
        musicbrainz_album_id: album.and_then(|album| album.musicbrainz_album_id.clone()),
        musicbrainz_release_group_id: album
            .and_then(|album| album.musicbrainz_release_group_id.clone()),
        release_types: album
            .map(|album| album.release_types.clone())
            .unwrap_or_default(),
        is_compilation: album.and_then(|album| album.is_compilation),
    }
}

fn relationship_ids<'a>(
    tracks: impl IntoIterator<Item = &'a Track>,
    albums: impl IntoIterator<Item = &'a Album>,
) -> (HashSet<ArtistId>, HashSet<GenreId>) {
    let mut artists = HashSet::new();
    let mut genres = HashSet::new();
    for track in tracks {
        artists.extend(
            track
                .relations
                .artists
                .iter()
                .chain(track.relations.album_artists.iter())
                .map(|credit| credit.id.clone()),
        );
        genres.extend(
            track
                .relations
                .genres
                .iter()
                .map(|credit| credit.id.clone()),
        );
    }
    for album in albums {
        artists.extend(
            album
                .relations
                .artists
                .iter()
                .chain(album.relations.album_artists.iter())
                .map(|credit| credit.id.clone()),
        );
        genres.extend(
            album
                .relations
                .genres
                .iter()
                .map(|credit| credit.id.clone()),
        );
    }
    (artists, genres)
}

fn scan_inventory(
    inventory: &Inventory,
    selected: Option<HashSet<String>>,
    cue_sheets: &HashMap<String, Option<CueSheet>>,
    output: &mut dyn FactOutput,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()> {
    let mut selected = selected;
    let mut cue_paths = inventory
        .entries
        .iter()
        .filter(|(path, entry)| {
            path_is_selected(selected.as_ref(), path) && entry.file.kind == LocalFileKind::Cue
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    cue_paths.sort();
    let mut cues = parse_cues(inventory, &cue_paths, cue_sheets, &mut selected, cancelled)?;
    let audio_directories = inventory
        .entries
        .iter()
        .filter(|(path, entry)| {
            path_is_selected(selected.as_ref(), path) && entry.file.kind == LocalFileKind::Media
        })
        .filter_map(|(_, entry)| entry.path.parent().map(Path::to_path_buf))
        .collect::<HashSet<_>>();
    if let Some(selected) = selected.as_mut() {
        for (path, entry) in &inventory.entries {
            if entry.file.kind == LocalFileKind::Image
                && entry
                    .path
                    .parent()
                    .is_some_and(|parent| audio_directories.contains(parent))
            {
                selected.insert(path.clone());
            }
        }
    }

    emit_observed_files(inventory, selected.as_ref(), output)?;

    let mut aggregates = Aggregates::new(inventory, HashMap::new());
    let mut track_batch = Vec::new();

    let media_total = inventory
        .entries
        .iter()
        .filter(|(path, entry)| {
            path_is_selected(selected.as_ref(), path) && entry.file.kind == LocalFileKind::Media
        })
        .count();
    progress(SourceReadProgress {
        stage: SourceReadStage::Tracks,
        completed: 0,
        total: Some(media_total),
    });

    let successful_cues = stream_media(
        inventory,
        selected.as_ref(),
        std::mem::take(&mut cues.plans),
        &mut aggregates,
        &mut track_batch,
        output,
        progress,
        media_total,
        cancelled,
    )?;
    if !track_batch.is_empty() {
        output.emit(CandidateBatch::Tracks(track_batch))?;
    }

    for file in &mut cues.files {
        if file.state == LocalFileState::Accepted && !successful_cues.contains(&file.path) {
            file.state = LocalFileState::Rejected;
        }
    }
    emit_chunks(cues.files, CandidateBatch::LocalFiles, output)?;

    aggregates.finish(inventory, output)?;
    progress(SourceReadProgress {
        stage: SourceReadStage::Finalizing,
        completed: 1,
        total: Some(1),
    });
    Ok(())
}

fn emit_observed_files(
    inventory: &Inventory,
    selected: Option<&HashSet<String>>,
    output: &mut dyn FactOutput,
) -> SourceResult<()> {
    let mut batch = Vec::with_capacity(LOCAL_BATCH_SIZE);
    for (path, entry) in &inventory.entries {
        if path_is_selected(selected, path)
            && matches!(
                entry.file.kind,
                LocalFileKind::Directory | LocalFileKind::Image
            )
        {
            batch.push(entry.file.clone());
            if batch.len() == LOCAL_BATCH_SIZE {
                output.emit(CandidateBatch::LocalFiles(std::mem::take(&mut batch)))?;
            }
        }
    }
    if !batch.is_empty() {
        output.emit(CandidateBatch::LocalFiles(batch))?;
    }
    Ok(())
}

fn path_is_selected(selected: Option<&HashSet<String>>, path: &str) -> bool {
    selected.is_none_or(|selected| selected.contains(path))
}

fn parse_cues(
    inventory: &Inventory,
    cue_paths: &[String],
    cue_sheets: &HashMap<String, Option<CueSheet>>,
    selected: &mut Option<HashSet<String>>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<ParsedCues> {
    let mut files = Vec::new();
    let mut plans = Vec::new();
    for cue_path in cue_paths {
        check_cancelled(cancelled)?;
        let entry = &inventory.entries[cue_path];
        let mut file = entry.file.clone();
        let sheet = cue_sheets
            .get(cue_path)
            .cloned()
            .unwrap_or_else(|| read_cue(&entry.path));
        let Some(sheet) = sheet else {
            file.state = LocalFileState::Rejected;
            files.push(file);
            continue;
        };
        let mut dependencies = sheet
            .files
            .iter()
            .map(|cue_file| cue_file.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        dependencies.sort();
        dependencies.dedup();
        file.dependencies = dependencies.clone();
        let all_backing_files_exist = dependencies.iter().all(|dependency| {
            inventory
                .entries
                .get(dependency)
                .is_some_and(|entry| entry.file.kind == LocalFileKind::Media)
        });
        if let Some(selected) = selected.as_mut() {
            selected.extend(dependencies.iter().filter_map(|dependency| {
                inventory
                    .entries
                    .get(dependency)
                    .is_some_and(|entry| entry.file.kind == LocalFileKind::Media)
                    .then(|| dependency.clone())
            }));
        }
        if !all_backing_files_exist {
            file.state = LocalFileState::Rejected;
            files.push(file);
            continue;
        }
        file.state = LocalFileState::Accepted;
        plans.push(CuePlan {
            cue_path: entry.path.clone(),
            album_title: sheet.album_title,
            album_performer: sheet.album_performer,
            files: sheet.files,
        });
        files.push(file);
    }
    Ok(ParsedCues { files, plans })
}

fn read_cue(path: &Path) -> Option<CueSheet> {
    let mut file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((LOCAL_CUE_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > LOCAL_CUE_MAX_BYTES {
        return None;
    }
    parse_cue_sheet(path, &String::from_utf8_lossy(&bytes))
}

fn run_ordered_jobs<J, R, W>(
    jobs: impl IntoIterator<Item = J>,
    create_worker: impl Fn() -> W + Sync,
    read: impl Fn(&mut W, J) -> SourceResult<R> + Sync,
    mut accept: impl FnMut(R) -> SourceResult<()>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<()>
where
    J: Send,
    R: Send,
{
    let worker_count = local_worker_count(
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
    );
    std::thread::scope(|scope| {
        let (job_send, job_receive) = mpsc::sync_channel::<(usize, J)>(worker_count * 2);
        let (result_send, result_receive) =
            mpsc::sync_channel::<(usize, SourceResult<R>)>(worker_count * 2);
        let job_receive = Arc::new(Mutex::new(job_receive));

        for _ in 0..worker_count {
            let job_receive = Arc::clone(&job_receive);
            let result_send = result_send.clone();
            let create_worker = &create_worker;
            let read = &read;
            scope.spawn(move || {
                let mut worker = create_worker();
                loop {
                    let job = {
                        let receive = job_receive
                            .lock()
                            .expect("Local media job receiver is not poisoned");
                        receive.recv()
                    };
                    let Ok((order, job)) = job else {
                        break;
                    };
                    let result = check_cancelled(cancelled).and_then(|()| read(&mut worker, job));
                    if result_send.send((order, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(result_send);

        let mut jobs = jobs.into_iter().enumerate();
        loop {
            check_cancelled(cancelled)?;
            let mut sent = 0;
            for _ in 0..worker_count {
                let Some(job) = jobs.next() else {
                    break;
                };
                job_send
                    .send(job)
                    .map_err(|_| SourceError::Other("Local media readers stopped.".to_string()))?;
                sent += 1;
            }
            if sent == 0 {
                break;
            }
            let mut wave = Vec::with_capacity(sent);
            for _ in 0..sent {
                wave.push(
                    result_receive.recv().map_err(|_| {
                        SourceError::Other("Local media readers stopped.".to_string())
                    })?,
                );
            }
            check_cancelled(cancelled)?;
            wave.sort_by_key(|(order, _)| *order);
            for (_, result) in wave {
                accept(result?)?;
            }
        }
        drop(job_send);
        Ok(())
    })
}

fn local_worker_count(available_parallelism: usize) -> usize {
    available_parallelism.clamp(1, 4)
}

fn stream_media(
    inventory: &Inventory,
    selected: Option<&HashSet<String>>,
    plans: Vec<CuePlan>,
    aggregates: &mut Aggregates,
    track_batch: &mut Vec<Track>,
    output: &mut dyn FactOutput,
    progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
    media_total: usize,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
) -> SourceResult<HashSet<String>> {
    let (cue_jobs, cue_media) = cue_jobs(plans);
    let mut completed = 0;
    let mut successful_cues = HashSet::new();
    let ordinary_jobs = inventory.entries.iter().filter_map(|(path, entry)| {
        (entry.file.kind == LocalFileKind::Media
            && path_is_selected(selected, path)
            && !cue_media.contains(path))
        .then(|| MediaJob {
            paths: vec![entry.path.clone()],
            cues: Vec::new(),
        })
    });
    let jobs = cue_jobs.into_iter().chain(ordinary_jobs);
    run_ordered_jobs(
        jobs,
        media::Worker::default,
        |worker, job| {
            let mut reads = Vec::with_capacity(job.paths.len());
            for path in &job.paths {
                check_cancelled(cancelled)?;
                reads.push((path.clone(), read_media(worker, inventory, path)));
            }
            Ok(MediaJobResult {
                reads,
                cues: job.cues,
            })
        },
        |result| {
            completed += result.reads.len();
            accept_media_result(
                inventory,
                result,
                aggregates,
                track_batch,
                output,
                &mut successful_cues,
            )?;
            progress(SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed,
                total: Some(media_total),
            });
            Ok(())
        },
        cancelled,
    )?;
    if completed != media_total {
        return Err(SourceError::Other(
            "Local media readers did not finish the selected files.".to_string(),
        ));
    }
    Ok(successful_cues)
}

fn cue_jobs(plans: Vec<CuePlan>) -> (Vec<MediaJob>, HashSet<String>) {
    if plans.is_empty() {
        return (Vec::new(), HashSet::new());
    }

    let mut plan_audio = HashMap::<String, Vec<usize>>::new();
    let mut all_audio = HashSet::new();
    for (index, plan) in plans.iter().enumerate() {
        for file in &plan.files {
            let path = file.path.to_string_lossy().into_owned();
            all_audio.insert(path.clone());
            plan_audio.entry(path).or_default().push(index);
        }
    }

    let mut plans = plans.into_iter().map(Some).collect::<Vec<_>>();
    let mut visited = vec![false; plans.len()];
    let mut jobs = Vec::new();
    for start in 0..plans.len() {
        if visited[start] {
            continue;
        }
        let mut pending = VecDeque::from([start]);
        let mut indices = Vec::new();
        let mut paths = BTreeSet::new();
        visited[start] = true;
        while let Some(index) = pending.pop_front() {
            indices.push(index);
            let plan = plans[index]
                .as_ref()
                .expect("an uncollected CUE plan is present");
            for file in &plan.files {
                let path = file.path.to_string_lossy().into_owned();
                paths.insert(path.clone());
                if let Some(neighbors) = plan_audio.get(&path) {
                    for &neighbor in neighbors {
                        if !visited[neighbor] {
                            visited[neighbor] = true;
                            pending.push_back(neighbor);
                        }
                    }
                }
            }
        }
        jobs.push(MediaJob {
            paths: paths.into_iter().map(PathBuf::from).collect(),
            cues: indices
                .into_iter()
                .map(|index| {
                    plans[index]
                        .take()
                        .expect("a CUE plan belongs to one connected job")
                })
                .collect(),
        });
    }
    (jobs, all_audio)
}

fn read_media(worker: &mut media::Worker, inventory: &Inventory, path: &Path) -> MediaRead {
    let path_text = path.to_string_lossy();
    let entry = inventory
        .entries
        .get(path_text.as_ref())
        .expect("a queued Local media path comes from the inventory");
    let sidecar = path
        .parent()
        .and_then(|directory| inventory.artwork_by_directory.get(directory))
        .cloned();
    media::read_media(
        worker,
        path.to_path_buf(),
        sidecar,
        file_revision(&entry.file),
    )
}

fn file_revision(file: &LocalFile) -> String {
    format!(
        "file:{:016x}",
        crate::policy::stable_hash(&format!(
            "{}:{}:{}:{}:{}",
            file.path,
            file.size_bytes.unwrap_or_default(),
            file.mtime_ns,
            file.device_id.unwrap_or_default(),
            file.inode.unwrap_or_default()
        ))
    )
}

fn accept_media_result(
    inventory: &Inventory,
    result: MediaJobResult,
    aggregates: &mut Aggregates,
    track_batch: &mut Vec<Track>,
    output: &mut dyn FactOutput,
    successful_cues: &mut HashSet<String>,
) -> SourceResult<()> {
    let reads = result
        .reads
        .iter()
        .map(|(path, read)| (path.to_string_lossy().into_owned(), read))
        .collect::<HashMap<_, _>>();
    let scanned = reads
        .iter()
        .filter_map(|(path, read)| match read {
            MediaRead::Accepted(scanned) => Some((path.clone(), scanned)),
            MediaRead::Rejected | MediaRead::Unreadable => None,
        })
        .collect::<HashMap<_, _>>();
    let mut suppressed_media = HashSet::new();
    for plan in &result.cues {
        let Some(cue_tracks) = cue_tracks_for_plan(plan, &scanned) else {
            continue;
        };
        successful_cues.insert(plan.cue_path.to_string_lossy().into_owned());
        suppressed_media.extend(
            plan.files
                .iter()
                .map(|file| file.path.to_string_lossy().into_owned()),
        );
        for track in cue_tracks {
            accept_scanned(track, aggregates, track_batch, output)?;
        }
    }

    let mut file_batch = Vec::with_capacity(result.reads.len().min(LOCAL_BATCH_SIZE));
    for (path, read) in result.reads {
        let path_text = path.to_string_lossy();
        let mut file = inventory
            .entries
            .get(path_text.as_ref())
            .expect("a read Local media path comes from the inventory")
            .file
            .clone();
        file.state = match read {
            MediaRead::Accepted(scanned) => {
                if !suppressed_media.contains(path_text.as_ref()) {
                    accept_scanned(scanned, aggregates, track_batch, output)?;
                }
                LocalFileState::Accepted
            }
            MediaRead::Rejected => LocalFileState::Rejected,
            MediaRead::Unreadable => LocalFileState::Unreadable,
        };
        file_batch.push(file);
        if file_batch.len() == LOCAL_BATCH_SIZE {
            output.emit(CandidateBatch::LocalFiles(std::mem::take(&mut file_batch)))?;
        }
    }
    if !file_batch.is_empty() {
        output.emit(CandidateBatch::LocalFiles(file_batch))?;
    }
    Ok(())
}

fn accept_scanned(
    scanned: ScannedTrack,
    aggregates: &mut Aggregates,
    track_batch: &mut Vec<Track>,
    output: &mut dyn FactOutput,
) -> SourceResult<()> {
    aggregates.add(&scanned);
    track_batch.push(scanned.track);
    if track_batch.len() == LOCAL_BATCH_SIZE {
        output.emit(CandidateBatch::Tracks(std::mem::take(track_batch)))?;
    }
    Ok(())
}

fn cue_tracks_for_plan(
    plan: &CuePlan,
    backing: &HashMap<String, &ScannedTrack>,
) -> Option<Vec<ScannedTrack>> {
    let mut numbers = HashSet::new();
    let mut tracks = Vec::new();
    for file in &plan.files {
        let source = backing.get(file.path.to_string_lossy().as_ref())?;
        let duration_millis = u64::from(source.track.duration_seconds).checked_mul(1_000)?;
        if duration_millis == 0 || file.tracks.is_empty() {
            return None;
        }
        for (position, cue_track) in file.tracks.iter().enumerate() {
            if !numbers.insert(cue_track.number) {
                return None;
            }
            let end = file
                .tracks
                .get(position + 1)
                .map(|track| track.index_start_ms)
                .unwrap_or(duration_millis);
            if cue_track.index_start_ms >= duration_millis
                || end > duration_millis
                || end <= cue_track.index_start_ms
            {
                return None;
            }
            tracks.push(cue_track_from(plan, cue_track, source, end));
        }
    }
    Some(tracks)
}

fn cue_track_from(
    plan: &CuePlan,
    cue_track: &CueTrack,
    backing: &ScannedTrack,
    end_millis: u64,
) -> ScannedTrack {
    let album = plan
        .album_title
        .clone()
        .unwrap_or_else(|| backing.track.album.clone());
    let album_artist = plan
        .album_performer
        .clone()
        .unwrap_or_else(|| backing.album_artist.clone());
    let artist = cue_track
        .performer
        .clone()
        .unwrap_or_else(|| album_artist.clone());
    let artists = media::split_artists(&artist)
        .iter()
        .map(|name| media::artist_credit(name, None))
        .collect::<Vec<_>>();
    let album_artists = media::split_artists(&album_artist)
        .iter()
        .map(|name| media::artist_credit(name, None))
        .collect::<Vec<_>>();
    let album_id = media::album_id(
        &album_artists,
        &album,
        backing.musicbrainz_album_id.as_deref(),
        Some(&plan.cue_path),
    );
    let duration = end_millis
        .saturating_sub(cue_track.index_start_ms)
        .div_euclid(1_000)
        .max(1)
        .min(u64::from(u32::MAX)) as u32;
    let mut track = backing.track.clone();
    track.id = media::cue_track_id(&plan.cue_path, cue_track.number);
    track.album_id = Some(album_id);
    track.title = cue_track
        .title
        .clone()
        .unwrap_or_else(|| format!("Track {}", cue_track.number));
    track.artist = artist;
    track.album = album;
    track.duration_seconds = duration;
    track.disc_number = track.disc_number.max(1);
    track.track_number = cue_track.number;
    track.musicbrainz_recording_id = None;
    track.musicbrainz_release_track_id = None;
    track.comment = None;
    track.relations.artists = artists;
    track.relations.album_artists = album_artists;
    track.cue = Some(library::CueSegment {
        cue_path: plan.cue_path.to_string_lossy().into_owned(),
        start_millis: cue_track.index_start_ms,
        end_millis,
    });
    ScannedTrack {
        track,
        album_artist,
        release_types: backing.release_types.clone(),
        is_compilation: backing.is_compilation,
        musicbrainz_album_id: backing.musicbrainz_album_id.clone(),
        musicbrainz_release_group_id: backing.musicbrainz_release_group_id.clone(),
    }
}

#[derive(Default)]
struct Aggregates {
    albums: BTreeMap<AlbumId, AlbumAggregate>,
    artists: BTreeMap<ArtistId, ArtistAggregate>,
    genres: BTreeMap<GenreId, GenreAggregate>,
    accepted_album_artwork: HashMap<AlbumId, library::LocalArtworkRef>,
}

struct AlbumAggregate {
    album: Album,
    album_artists: BTreeMap<ArtistId, ArtistCredit>,
    artists: BTreeMap<ArtistId, ArtistCredit>,
    genres: BTreeMap<GenreId, library::GenreCredit>,
    source_paths: BTreeSet<PathBuf>,
}

#[derive(Default)]
struct ArtistAggregate {
    name: String,
    musicbrainz_artist_id: Option<String>,
}

#[derive(Default)]
struct GenreAggregate {
    name: String,
}

impl Aggregates {
    fn new(
        inventory: &Inventory,
        accepted_album_artwork: HashMap<AlbumId, library::LocalArtworkRef>,
    ) -> Self {
        let accepted_album_artwork = accepted_album_artwork
            .into_iter()
            .filter(|(_, artwork)| artwork_matches_inventory(inventory, artwork))
            .collect();
        Self {
            accepted_album_artwork,
            ..Self::default()
        }
    }

    fn add(&mut self, scanned: &ScannedTrack) {
        let track = &scanned.track;
        let Some(album_id) = track.album_id.clone() else {
            return;
        };
        {
            let entry = self
                .albums
                .entry(album_id.clone())
                .or_insert_with(|| AlbumAggregate {
                    album: Album {
                        id: album_id.clone(),
                        title: track.album.clone(),
                        artist: scanned.album_artist.clone(),
                        year: track.year,
                        release_date: track.release_date.clone(),
                        date_added: None,
                        last_played: None,
                        play_count: None,
                        user_rating: None,
                        favorite: false,
                        color_seed: crate::policy::stable_hash(album_id.as_str()) as u32,
                        image_ref: None,
                        local_artwork: track
                            .local_artwork
                            .clone()
                            .or_else(|| self.accepted_album_artwork.get(&album_id).cloned()),
                        release_types: scanned.release_types.clone(),
                        is_compilation: scanned.is_compilation,
                        musicbrainz_album_id: scanned.musicbrainz_album_id.clone(),
                        musicbrainz_release_group_id: scanned.musicbrainz_release_group_id.clone(),
                        relations: AlbumRelations::default(),
                    },
                    album_artists: BTreeMap::new(),
                    artists: BTreeMap::new(),
                    genres: BTreeMap::new(),
                    source_paths: BTreeSet::new(),
                });
            if let Some(path) = &track.source_path {
                entry.source_paths.insert(PathBuf::from(path));
            }
            if entry.album.local_artwork.is_none() {
                entry.album.local_artwork.clone_from(&track.local_artwork);
            }
            if entry.album.year == 0 {
                entry.album.year = track.year;
            }
            if entry.album.musicbrainz_album_id.is_none() {
                entry
                    .album
                    .musicbrainz_album_id
                    .clone_from(&scanned.musicbrainz_album_id);
            }
            if entry.album.musicbrainz_release_group_id.is_none() {
                entry
                    .album
                    .musicbrainz_release_group_id
                    .clone_from(&scanned.musicbrainz_release_group_id);
            }
            entry
                .album
                .release_types
                .extend(scanned.release_types.iter().cloned());
            entry.album.release_types.sort();
            entry.album.release_types.dedup();
            entry.album.is_compilation =
                merge_compilation(entry.album.is_compilation, scanned.is_compilation);
            for credit in &track.relations.album_artists {
                entry
                    .album_artists
                    .entry(credit.id.clone())
                    .or_insert_with(|| credit.clone());
            }
            for credit in &track.relations.artists {
                entry
                    .artists
                    .entry(credit.id.clone())
                    .or_insert_with(|| credit.clone());
            }
            for credit in &track.relations.genres {
                entry
                    .genres
                    .entry(credit.id.clone())
                    .or_insert_with(|| credit.clone());
            }
        }
        for credit in track
            .relations
            .album_artists
            .iter()
            .chain(&track.relations.artists)
        {
            self.add_artist(credit);
        }
        for credit in &track.relations.genres {
            self.genres
                .entry(credit.id.clone())
                .or_insert_with(|| GenreAggregate {
                    name: credit.name.clone(),
                    ..GenreAggregate::default()
                });
        }
    }

    fn add_artist(&mut self, credit: &ArtistCredit) {
        self.artists
            .entry(credit.id.clone())
            .or_insert_with(|| ArtistAggregate {
                name: credit.name.clone(),
                musicbrainz_artist_id: credit.musicbrainz_artist_id.clone(),
                ..ArtistAggregate::default()
            });
    }

    fn finish(self, inventory: &Inventory, output: &mut dyn FactOutput) -> SourceResult<()> {
        let accepted_media = self
            .albums
            .values()
            .flat_map(|aggregate| aggregate.source_paths.iter().cloned())
            .collect::<BTreeSet<_>>();
        let albums = self.albums.into_values().map(|mut aggregate| {
            if aggregate.album.local_artwork.is_none() {
                aggregate.album.local_artwork =
                    common_album_artwork(inventory, &aggregate.source_paths, &accepted_media);
            }
            aggregate.album.relations = AlbumRelations {
                album_artists: aggregate.album_artists.into_values().collect(),
                artists: aggregate.artists.into_values().collect(),
                genres: aggregate.genres.into_values().collect(),
            };
            aggregate.album
        });
        emit_iter_chunks(albums, CandidateBatch::Albums, output)?;
        let artists = self.artists.into_iter().map(|(id, aggregate)| Artist {
            id,
            name: aggregate.name,
            favorite: false,
            last_played: None,
            play_count: None,
            user_rating: None,
            musicbrainz_artist_id: aggregate.musicbrainz_artist_id,
            image_ref: None,
            local_artwork: None,
        });
        emit_iter_chunks(artists, CandidateBatch::Artists, output)?;
        let genres = self.genres.into_iter().map(|(id, aggregate)| Genre {
            id,
            name: aggregate.name,
            image_ref: None,
        });
        emit_iter_chunks(genres, CandidateBatch::Genres, output)
    }
}

fn merge_compilation(current: Option<bool>, incoming: Option<bool>) -> Option<bool> {
    match (current, incoming) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), _) | (_, Some(false)) => Some(false),
        (None, None) => None,
    }
}

fn common_album_artwork(
    inventory: &Inventory,
    source_paths: &BTreeSet<PathBuf>,
    accepted_media: &BTreeSet<PathBuf>,
) -> Option<library::LocalArtworkRef> {
    let first_directory = source_paths.first()?.parent()?;
    let directory = first_directory.ancestors().find(|directory| {
        inventory.artwork_by_directory.contains_key(*directory)
            && source_paths.iter().all(|path| path.starts_with(directory))
    })?;
    let accepted_count = inventory
        .accepted_media_counts_by_directory
        .get(directory)
        .copied()
        .unwrap_or_else(|| {
            accepted_media
                .iter()
                .filter(|path| path.starts_with(directory))
                .count()
        });
    if accepted_count == source_paths.len() && source_paths.is_subset(accepted_media) {
        inventory.artwork_by_directory.get(directory).cloned()
    } else {
        None
    }
}

fn artwork_matches_inventory(inventory: &Inventory, artwork: &library::LocalArtworkRef) -> bool {
    inventory
        .entries
        .get(artwork.path())
        .is_some_and(|entry| file_revision(&entry.file) == artwork.revision())
}

fn emit_chunks<T>(
    values: Vec<T>,
    wrap: impl Fn(Vec<T>) -> CandidateBatch,
    output: &mut dyn FactOutput,
) -> SourceResult<()> {
    emit_iter_chunks(values, wrap, output)
}

fn emit_iter_chunks<T>(
    values: impl IntoIterator<Item = T>,
    wrap: impl Fn(Vec<T>) -> CandidateBatch,
    output: &mut dyn FactOutput,
) -> SourceResult<()> {
    let mut values = values.into_iter();
    loop {
        let batch = values.by_ref().take(LOCAL_BATCH_SIZE).collect::<Vec<_>>();
        if batch.is_empty() {
            return Ok(());
        }
        output.emit(wrap(batch))?;
    }
}

fn sorted<T: Ord>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values
}

fn require_root(root: &Path) -> SourceResult<()> {
    if !root.is_dir() {
        return Err(SourceError::Other(format!(
            "Could not read Local music folder {}",
            root.display()
        )));
    }
    fs::read_dir(root).map_err(|error| {
        SourceError::Other(format!(
            "Could not read Local music folder {}: {error}",
            root.display()
        ))
    })?;
    Ok(())
}

fn recognized_file(path: &Path, is_directory: bool, is_file: bool) -> Option<LocalFileKind> {
    if is_directory {
        Some(LocalFileKind::Directory)
    } else if is_file && is_cue(path) {
        Some(LocalFileKind::Cue)
    } else if is_file && artwork::supported_image(path) {
        Some(LocalFileKind::Image)
    } else if is_file && !ignored_file(path) {
        Some(LocalFileKind::Media)
    } else {
        None
    }
}

fn ignored_file(path: &Path) -> bool {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("qt_temp"))
    {
        return true;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            [
                "tmp", "tar", "gz", "bz2", "xz", "tbz", "tgz", "z", "zip", "rar", "wvc", "zst",
                "lrc", "amz", "asx", "asxini", "m3u", "m3u8", "pla", "pls", "ram", "vlc", "wax",
                "wmx", "wvx", "xspf",
            ]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn is_cue(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"))
}

fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| {
            i128::from(duration.as_secs())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(duration.subsec_nanos()))
                .min(i128::from(i64::MAX)) as i64
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn metadata_inode(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.ino())
}

#[cfg(not(unix))]
fn metadata_inode(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
fn metadata_device(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
fn metadata_device(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> SourceResult<()> {
    if cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod ordered_job_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn local_reader_parallelism_is_capped_at_four() {
        assert_eq!(local_worker_count(1), 1);
        assert_eq!(local_worker_count(4), 4);
        assert_eq!(local_worker_count(usize::MAX), 4);
    }

    #[test]
    fn cancellation_stops_bounded_work_between_files() {
        let worker_count = AtomicUsize::new(0);
        let read_count = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let result = run_ordered_jobs(
            0..32,
            || {
                worker_count.fetch_add(1, Ordering::SeqCst);
            },
            |(), job| {
                read_count.fetch_add(1, Ordering::SeqCst);
                if job == 0 {
                    cancelled.store(true, Ordering::SeqCst);
                }
                Ok(job)
            },
            |_| Ok(()),
            &|| cancelled.load(Ordering::SeqCst),
        );

        assert!(matches!(result, Err(SourceError::Cancelled)));
        assert!(worker_count.load(Ordering::SeqCst) <= 4);
        assert!(read_count.load(Ordering::SeqCst) <= 4);
    }

    #[test]
    fn cancellation_is_checked_inside_a_connected_cue_job() {
        let read_count = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let result = run_ordered_jobs(
            [vec![0, 1, 2]],
            || (),
            |(), paths| {
                for path in paths {
                    check_cancelled(&|| cancelled.load(Ordering::SeqCst))?;
                    read_count.fetch_add(1, Ordering::SeqCst);
                    if path == 0 {
                        cancelled.store(true, Ordering::SeqCst);
                    }
                }
                Ok(())
            },
            |()| Ok(()),
            &|| cancelled.load(Ordering::SeqCst),
        );

        assert!(matches!(result, Err(SourceError::Cancelled)));
        assert_eq!(read_count.load(Ordering::SeqCst), 1);
    }
}
