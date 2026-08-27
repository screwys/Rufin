use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use sources::SourceId;

use crate::selection::Candidate;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const CACHE_LAYOUT: &str = "v1";
const MAX_EXTERNAL_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXTERNAL_CACHE_FILES: usize = 50_000;
const PRUNE_TARGET_PERCENT: u64 = 90;

pub(crate) fn current_layout(root: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let current = root.join(CACHE_LAYOUT);
    fs::create_dir_all(&current)?;
    let root = root.to_path_buf();
    if root
        .read_dir()?
        .flatten()
        .any(|entry| entry.path() != current)
    {
        let cleanup = root.clone();
        if let Err(error) = thread::Builder::new()
            .name("artwork-cache-migration".to_string())
            .spawn(move || remove_legacy_layout(&cleanup))
        {
            tracing::warn!(%error, path = %root.display(), "failed to start legacy artwork cache cleanup");
        }
    }
    Ok(current)
}

fn remove_legacy_layout(root: &Path) {
    let Ok(entries) = root.read_dir() else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == CACHE_LAYOUT) {
            continue;
        }
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(error) = result {
            tracing::warn!(%error, path = %path.display(), "failed to remove legacy artwork cache entry");
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CacheLimits {
    bytes: u64,
    files: usize,
}

impl CacheLimits {
    fn prune_target(self) -> Self {
        Self {
            bytes: (self.bytes * PRUNE_TARGET_PERCENT / 100).max(1),
            files: (self.files * PRUNE_TARGET_PERCENT as usize / 100).max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CacheUsage {
    bytes: u64,
    files: usize,
}

impl CacheUsage {
    const fn exceeds(self, limits: CacheLimits) -> bool {
        self.bytes > limits.bytes || self.files > limits.files
    }
}

#[derive(Debug)]
struct CacheMaintenance {
    state: Mutex<Option<CacheUsage>>,
    limits: CacheLimits,
}

#[derive(Clone, Debug)]
pub(crate) struct FilesystemCache {
    root: PathBuf,
    external_maintenance: Arc<CacheMaintenance>,
}

impl FilesystemCache {
    pub(crate) fn begin_source_manifest(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> io::Result<PathBuf> {
        let path = self.root.join("manifest-staging").join(format!(
            "{}-{}-{}-{}",
            digest(source_id.as_str()),
            revision,
            std::process::id(),
            TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    pub(crate) fn mark_source_manifest_identity(
        &self,
        staging: &Path,
        identity: &str,
    ) -> io::Result<()> {
        if !identity.is_empty() {
            fs::write(staging.join(digest(identity)), [])?;
        }
        Ok(())
    }

    pub(crate) fn complete_source_manifest_staging(
        &self,
        source_id: &SourceId,
        revision: u64,
        staging: &Path,
    ) -> io::Result<()> {
        let source = digest(source_id.as_str());
        reconcile_source_directory_marked(
            &self.root.join("ready/native").join(&source),
            staging,
            true,
        )?;
        reconcile_source_directory_marked(
            &self.root.join("missing/native").join(source),
            staging,
            false,
        )?;
        atomic_write(
            &self.source_manifest_path(source_id),
            revision.to_string().as_bytes(),
        )?;
        remove_dir_if_present(staging)
    }
    pub(crate) fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let cache = Self {
            root,
            external_maintenance: Arc::new(CacheMaintenance {
                state: Mutex::new(None),
                limits: CacheLimits {
                    bytes: MAX_EXTERNAL_CACHE_BYTES,
                    files: MAX_EXTERNAL_CACHE_FILES,
                },
            }),
        };
        let initializer = cache.clone();
        if let Err(error) = thread::Builder::new()
            .name("artwork-cache-prune".to_string())
            .spawn(move || {
                if let Err(error) = initializer.initialize_usage() {
                    tracing::warn!(%error, path = %initializer.root.display(), "failed to prune artwork cache");
                }
            })
        {
            tracing::warn!(%error, path = %cache.root.display(), "failed to start artwork cache pruning");
            cache.initialize_usage()?;
        }
        Ok(cache)
    }

    pub(crate) fn ready_entry(
        &self,
        source_id: &SourceId,
        candidate: &Candidate,
        requested_size: u32,
    ) -> Option<CacheEntry> {
        for size in reusable_sizes(requested_size) {
            let path = self.ready_path(source_id, candidate, size);
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.is_file() && metadata.len() > 0 {
                return Some(CacheEntry { path });
            }
            self.remove_file_tracked(&path);
        }
        None
    }

    pub(crate) fn write_ready(
        &self,
        source_id: &SourceId,
        candidate: &Candidate,
        size: u32,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artwork response was empty",
            ));
        }
        let path = self.ready_path(source_id, candidate, size);
        if candidate.is_external() {
            self.write_external_tracked(&path, bytes)?;
            self.remove_file_external_tracked(&self.missing_path(source_id, candidate, size));
        } else {
            atomic_write(&path, bytes)?;
            remove_file_if_present(&self.missing_path(source_id, candidate, size))?;
        }
        Ok(path)
    }

    pub(crate) fn remove_ready(&self, path: &Path) {
        self.remove_file_tracked(path);
    }

    pub(crate) fn is_missing(
        &self,
        source_id: &SourceId,
        candidate: &Candidate,
        size: u32,
    ) -> bool {
        reusable_sizes(size)
            .into_iter()
            .any(|size| self.missing_path(source_id, candidate, size).is_file())
    }

    pub(crate) fn mark_missing(
        &self,
        source_id: &SourceId,
        candidate: &Candidate,
        size: u32,
    ) -> io::Result<()> {
        let path = self.missing_path(source_id, candidate, size);
        if candidate.is_external() {
            self.write_external_tracked(&path, b"missing\n")
        } else {
            atomic_write(&path, b"missing\n")
        }
    }

    pub(crate) fn retry_external(&self) -> io::Result<()> {
        self.remove_dir_external_tracked(&self.root.join("missing/external"))
    }

    pub(crate) fn invalidate_source(&self, source_id: &SourceId) -> io::Result<()> {
        let source = digest(source_id.as_str());
        remove_dir_if_present(&self.root.join("ready/native").join(&source))?;
        remove_dir_if_present(&self.root.join("missing/native").join(source))
    }

    pub(crate) fn source_manifest_complete(
        &self,
        source_id: &SourceId,
        revision: u64,
    ) -> io::Result<bool> {
        let manifest = self.source_manifest_path(source_id);
        match fs::read_to_string(manifest) {
            Ok(value) => Ok(value.trim() == revision.to_string()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn ready_path(&self, source_id: &SourceId, candidate: &Candidate, size: u32) -> PathBuf {
        self.candidate_path("ready", source_id, candidate, size, "img")
    }

    fn missing_path(&self, source_id: &SourceId, candidate: &Candidate, size: u32) -> PathBuf {
        self.candidate_path("missing", source_id, candidate, size, "missing")
    }

    fn source_manifest_path(&self, source_id: &SourceId) -> PathBuf {
        self.root
            .join("ready/native")
            .join(digest(source_id.as_str()))
            .join(".manifest")
    }

    fn candidate_path(
        &self,
        state: &str,
        source_id: &SourceId,
        candidate: &Candidate,
        size: u32,
        extension: &str,
    ) -> PathBuf {
        let identity = digest(&candidate.stable_identity());
        if candidate.is_external() {
            self.root
                .join(state)
                .join("external")
                .join(identity)
                .join(format!("{size}.{extension}"))
        } else {
            self.root
                .join(state)
                .join("native")
                .join(digest(source_id.as_str()))
                .join(identity)
                .join(format!("{size}.{extension}"))
        }
    }

    fn initialize_usage(&self) -> io::Result<()> {
        let mut state = lock(&self.external_maintenance.state)?;
        let usage = prune_external_cache(
            &self.root,
            self.external_maintenance.limits,
            self.external_maintenance.limits,
            None,
        )?;
        *state = Some(usage);
        Ok(())
    }

    fn write_external_tracked(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut state = lock(&self.external_maintenance.state)?;
        let previous = file_usage(path);
        atomic_write(path, bytes)?;
        let current = file_usage(path);
        let usage = if let Some(mut usage) = *state {
            usage.bytes = usage
                .bytes
                .saturating_sub(previous.bytes)
                .saturating_add(current.bytes);
            usage.files = usage
                .files
                .saturating_sub(previous.files)
                .saturating_add(current.files);
            if usage.exceeds(self.external_maintenance.limits) {
                prune_external_cache(
                    &self.root,
                    self.external_maintenance.limits,
                    self.external_maintenance.limits.prune_target(),
                    Some(path),
                )?
            } else {
                usage
            }
        } else {
            prune_external_cache(
                &self.root,
                self.external_maintenance.limits,
                self.external_maintenance.limits,
                Some(path),
            )?
        };
        *state = Some(usage);
        Ok(())
    }

    fn remove_file_tracked(&self, path: &Path) {
        if is_external_path(&self.root, path) {
            self.remove_file_external_tracked(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }

    fn remove_file_external_tracked(&self, path: &Path) {
        let Ok(mut state) = lock(&self.external_maintenance.state) else {
            return;
        };
        let previous = file_usage(path);
        if fs::remove_file(path).is_ok()
            && let Some(usage) = state.as_mut()
        {
            usage.bytes = usage.bytes.saturating_sub(previous.bytes);
            usage.files = usage.files.saturating_sub(previous.files);
        }
    }

    fn remove_dir_external_tracked(&self, path: &Path) -> io::Result<()> {
        let mut state = lock(&self.external_maintenance.state)?;
        let previous = path_usage(path)?;
        remove_dir_if_present(path)?;
        if let Some(usage) = state.as_mut() {
            usage.bytes = usage.bytes.saturating_sub(previous.bytes);
            usage.files = usage.files.saturating_sub(previous.files);
        }
        Ok(())
    }
}

struct CacheFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

fn prune_external_cache(
    root: &Path,
    trigger: CacheLimits,
    target: CacheLimits,
    preserve: Option<&Path>,
) -> io::Result<CacheUsage> {
    let mut files = Vec::new();
    collect_external_cache_files(root, &mut files)?;
    let mut bytes = files.iter().map(|file| file.bytes).sum::<u64>();
    if bytes <= trigger.bytes && files.len() <= trigger.files {
        return Ok(CacheUsage {
            bytes,
            files: files.len(),
        });
    }
    files.sort_by_key(|file| file.modified);
    let mut remaining = files.len();
    for file in files {
        if bytes <= target.bytes && remaining <= target.files {
            break;
        }
        if preserve.is_some_and(|preserve| preserve == file.path) {
            continue;
        }
        if fs::remove_file(&file.path).is_ok() {
            bytes = bytes.saturating_sub(file.bytes);
            remaining = remaining.saturating_sub(1);
        }
    }
    Ok(CacheUsage {
        bytes,
        files: remaining,
    })
}

fn collect_external_cache_files(root: &Path, files: &mut Vec<CacheFile>) -> io::Result<()> {
    for path in [root.join("ready/external"), root.join("missing/external")] {
        if path.is_dir() {
            collect_cache_files(&path, files)?;
        }
    }
    Ok(())
}

fn collect_cache_files(root: &Path, files: &mut Vec<CacheFile>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_cache_files(&entry.path(), files)?;
        } else if metadata.is_file() {
            files.push(CacheFile {
                path: entry.path(),
                bytes: metadata.len(),
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            });
        }
    }
    Ok(())
}

fn file_usage(path: &Path) -> CacheUsage {
    fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| CacheUsage {
            bytes: metadata.len(),
            files: 1,
        })
        .unwrap_or_default()
}

fn path_usage(path: &Path) -> io::Result<CacheUsage> {
    if !path.exists() {
        return Ok(CacheUsage::default());
    }
    let mut files = Vec::new();
    collect_cache_files(path, &mut files)?;
    Ok(CacheUsage {
        bytes: files.iter().map(|file| file.bytes).sum(),
        files: files.len(),
    })
}

fn is_external_path(root: &Path, path: &Path) -> bool {
    path.starts_with(root.join("ready/external")) || path.starts_with(root.join("missing/external"))
}

fn reconcile_source_directory_marked(
    path: &Path,
    staging: &Path,
    keep_manifest: bool,
) -> io::Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if keep_manifest && name == ".manifest" {
            continue;
        }
        if staging.join(&name).is_file() {
            continue;
        }
        let entry_path = entry.path();
        if entry_path.is_dir() {
            remove_dir_if_present(&entry_path)?;
        } else {
            remove_file_if_present(&entry_path)?;
        }
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("artwork cache maintenance lock was poisoned"))
}

#[derive(Clone, Debug)]
pub(crate) struct CacheEntry {
    pub(crate) path: PathBuf,
}

fn reusable_sizes(requested: u32) -> Vec<u32> {
    let mut sizes = vec![requested.max(1)];
    for standard in [96, 256, 512] {
        if standard >= requested && !sizes.contains(&standard) {
            sizes.push(standard);
        }
    }
    sizes.sort_unstable();
    sizes
}

fn digest(value: &str) -> String {
    format!("{:x}", md5::compute(value.as_bytes()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "artwork cache path has no parent",
        ));
    };
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".artwork-{}-{}.tmp",
        std::process::id(),
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&temporary, bytes)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) if path.is_file() => {
            let _ = fs::remove_file(&temporary);
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sources::NativeImageRef;

    fn native() -> Candidate {
        Candidate::Native(NativeImageRef::new("album", Some("tag".to_string())))
    }

    #[test]
    fn a_ready_larger_cover_is_reused_for_a_smaller_preview() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        let source = SourceId::new("source");
        cache
            .write_ready(&source, &native(), 256, b"normalized")
            .expect("cache cover");

        assert!(cache.ready_entry(&source, &native(), 96).is_some());
        assert!(cache.ready_entry(&source, &native(), 512).is_none());
    }

    #[test]
    fn source_invalidation_removes_only_that_native_binding_family() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        let source = SourceId::new("source");
        let other = SourceId::new("other");
        cache
            .write_ready(&source, &native(), 256, b"source")
            .expect("cache source cover");
        cache
            .write_ready(&other, &native(), 256, b"other")
            .expect("cache other cover");

        cache.invalidate_source(&source).expect("invalidate source");
        assert!(cache.ready_entry(&source, &native(), 256).is_none());
        assert!(cache.ready_entry(&other, &native(), 256).is_some());
    }
}

fn remove_dir_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
