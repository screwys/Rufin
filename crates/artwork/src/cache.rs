use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use md5_digest::{Digest, Md5};
use sources::{ImageSize, SourceId};

use crate::selection::Candidate;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const CACHE_LAYOUT: &str = "v1";
const MAX_DISCRETIONARY_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DISCRETIONARY_CACHE_FILES: usize = 50_000;
const PRUNE_TARGET_PERCENT: u64 = 90;

#[derive(Clone, Debug)]
pub struct ArtworkCacheEntry {
    pub key: String,
    pub path: PathBuf,
}

impl ArtworkCacheEntry {
    pub(crate) fn family(&self) -> &str {
        self.key
            .split_once('/')
            .unwrap()
            .1
            .rsplit_once('/')
            .unwrap()
            .0
    }
}

/// Walks the fixed cache layout without collecting the artwork library in memory.
pub struct ArtworkCacheEntries {
    root: PathBuf,
    directories: Vec<fs::ReadDir>,
}

impl Iterator for ArtworkCacheEntries {
    type Item = io::Result<ArtworkCacheEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let directory = self.directories.last_mut()?;
            let entry = match directory.next() {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => return Some(Err(error)),
                None => {
                    self.directories.pop();
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => return Some(Err(error)),
            };
            let path = entry.path();
            if file_type.is_dir() {
                let image_directory = path
                    .strip_prefix(&self.root)
                    .ok()
                    .and_then(|path| path.components().next())
                    .is_some_and(|part| {
                        part.as_os_str() == "ready" || part.as_os_str() == "originals"
                    });
                if image_directory && self.directories.len() < 5 {
                    match fs::read_dir(path) {
                        Ok(directory) => self.directories.push(directory),
                        Err(error) => return Some(Err(error)),
                    }
                }
            } else if file_type.is_file()
                && let Some(key) = cache_key(&self.root, &path)
            {
                match entry.metadata() {
                    Ok(metadata) if metadata.len() > 0 => {
                        return Some(Ok(ArtworkCacheEntry { key, path }));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Some(Err(error)),
                }
            }
        }
    }
}

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
    maintenance: Arc<CacheMaintenance>,
}

impl FilesystemCache {
    pub(crate) fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let cache = Self {
            root,
            maintenance: Arc::new(CacheMaintenance {
                state: Mutex::new(None),
                limits: CacheLimits {
                    bytes: MAX_DISCRETIONARY_CACHE_BYTES,
                    files: MAX_DISCRETIONARY_CACHE_FILES,
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

    pub(crate) fn entries(&self) -> io::Result<ArtworkCacheEntries> {
        Ok(ArtworkCacheEntries {
            root: self.root.clone(),
            directories: vec![fs::read_dir(&self.root)?],
        })
    }

    pub(crate) fn preview_index_path(&self) -> PathBuf {
        self.root.join("previews.sqlite3")
    }

    pub(crate) fn file(&self, key: &str) -> io::Result<Option<PathBuf>> {
        let path = self.path_for_key(key)?;
        match fs::metadata(&path) {
            Ok(metadata) => Ok((metadata.is_file() && metadata.len() > 0).then_some(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn family_file(
        &self,
        request: &crate::CacheImageRequest,
    ) -> io::Result<Option<ArtworkCacheEntry>> {
        self.path_for_key(&format!("ready/{}/1.png", request.family))?;
        let mut keys = Vec::new();
        for state in ["ready", "originals"] {
            let directory = match fs::read_dir(self.root.join(state).join(&request.family)) {
                Ok(directory) => directory,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for entry in directory {
                let entry = entry?;
                if entry.metadata()?.is_file()
                    && entry.metadata()?.len() > 0
                    && let Some(key) = cache_key(&self.root, &entry.path())
                {
                    keys.push(key);
                }
            }
        }
        request
            .select_key(keys.iter().map(String::as_str))
            .map(|key| {
                Ok(ArtworkCacheEntry {
                    key: key.to_owned(),
                    path: self.path_for_key(key)?,
                })
            })
            .transpose()
    }

    pub(crate) fn import_missing(&self, key: &str, source: &Path) -> io::Result<bool> {
        let path = self.path_for_key(key)?;
        if source.metadata()?.len() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artwork response was empty",
            ));
        }
        if self.file(key)?.is_some() {
            return Ok(false);
        }
        if is_budgeted_path(&self.root, &path) {
            self.write_tracked(&path, fs::File::open(source)?)?;
        } else {
            atomic_write(&path, fs::File::open(source)?)?;
        }
        Ok(true)
    }

    fn path_for_key(&self, key: &str) -> io::Result<PathBuf> {
        if valid_cache_key(key) {
            Ok(key
                .split('/')
                .fold(self.root.clone(), |path, part| path.join(part)))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid artwork cache key",
            ))
        }
    }

    pub(crate) fn remove_entries(&self, entries: &[ArtworkCacheEntry]) -> io::Result<()> {
        let mut usage = lock(&self.maintenance.state)?;
        for entry in entries {
            let previous = if is_budgeted_path(&self.root, &entry.path) {
                file_usage(&entry.path)
            } else {
                CacheUsage::default()
            };
            remove_file_if_present(&entry.path)?;
            if let Some(usage) = usage.as_mut() {
                usage.bytes = usage.bytes.saturating_sub(previous.bytes);
                usage.files = usage.files.saturating_sub(previous.files);
            }
        }
        Ok(())
    }

    pub(crate) fn clear_missing(&self) -> io::Result<()> {
        let mut usage = lock(&self.maintenance.state)?;
        *usage = None;
        remove_dir_if_present(&self.root.join("missing"))
    }

    pub(crate) fn ready_entry(
        &self,
        candidate: &Candidate,
        requested_size: ImageSize,
    ) -> Option<CacheEntry> {
        self.ready_entries(candidate, requested_size).next()
    }

    pub(crate) fn ready_entries(
        &self,
        candidate: &Candidate,
        requested_size: ImageSize,
    ) -> impl Iterator<Item = CacheEntry> + '_ {
        let paths = match requested_size {
            ImageSize::Original => self
                .candidate_directory("originals", candidate)
                .read_dir()
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.file_stem().is_some_and(|name| name == "original"))
                .collect::<Vec<_>>(),
            ImageSize::Thumbnail(requested) => {
                let mut entries = self
                    .candidate_directory("ready", candidate)
                    .read_dir()
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|entry| {
                        let path = entry.path();
                        let size = thumbnail_size(path.file_name()?.to_str()?)?;
                        (size >= requested).then_some((size, path))
                    })
                    .collect::<Vec<_>>();
                entries.sort_unstable_by(|left, right| {
                    left.0.cmp(&right.0).then(left.1.cmp(&right.1))
                });
                entries.into_iter().map(|(_, path)| path).collect()
            }
        };
        paths.into_iter().filter_map(|path| {
            let metadata = fs::metadata(&path).ok()?;
            if metadata.is_file() && metadata.len() > 0 {
                return Some(CacheEntry { path });
            }
            self.remove_file_tracked(&path);
            None
        })
    }

    pub(crate) fn write_ready(
        &self,
        candidate: &Candidate,
        size: ImageSize,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "artwork response was empty",
            ));
        }
        let path = match size {
            ImageSize::Original => {
                let extension = crate::decode::original_extension(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                self.candidate_directory("originals", candidate)
                    .join(format!("original.{extension}"))
            }
            ImageSize::Thumbnail(size) => self
                .candidate_directory("ready", candidate)
                .join(format!("{size}.png")),
        };
        if same_file_bytes(&path, bytes)? {
            return Ok(path);
        }
        if candidate.is_external() || size == ImageSize::Original {
            self.write_tracked(&path, bytes)?;
            self.remove_file_tracked(&self.missing_path(candidate, size));
        } else {
            atomic_write(&path, bytes)?;
            remove_file_if_present(&self.missing_path(candidate, size))?;
        }
        Ok(path)
    }

    pub(crate) fn remove_ready(&self, path: &Path) {
        self.remove_file_tracked(path);
    }

    pub(crate) fn is_missing(&self, candidate: &Candidate, size: ImageSize) -> bool {
        match size {
            ImageSize::Original => self.missing_path(candidate, size).is_file(),
            ImageSize::Thumbnail(size) => reusable_sizes(size).into_iter().any(|size| {
                self.missing_path(candidate, ImageSize::Thumbnail(size))
                    .is_file()
            }),
        }
    }

    pub(crate) fn mark_missing(&self, candidate: &Candidate, size: ImageSize) -> io::Result<()> {
        let path = self.missing_path(candidate, size);
        if candidate.is_external() {
            self.write_tracked(&path, &b"missing\n"[..])
        } else {
            atomic_write(&path, &b"missing\n"[..])
        }
    }

    pub(crate) fn retry_external(&self) -> io::Result<()> {
        self.remove_dir_budgeted(&self.root.join("missing/external"))
    }

    pub(crate) fn invalidate_source(&self, source_id: &SourceId) -> io::Result<()> {
        let source = digest(source_id.as_str());
        self.remove_dir_notified(&self.root.join("ready/native").join(&source))?;
        remove_dir_if_present(&self.root.join("missing/native").join(&source))?;
        self.remove_dir_budgeted(&self.root.join("originals/native").join(source))
    }

    pub(crate) fn invalidate_image(&self, candidate: &Candidate) -> io::Result<()> {
        self.remove_dir_notified(&self.candidate_directory("ready", candidate))?;
        remove_dir_if_present(&self.candidate_directory("missing", candidate))?;
        self.remove_dir_budgeted(&self.candidate_directory("originals", candidate))
    }

    fn missing_path(&self, candidate: &Candidate, size: ImageSize) -> PathBuf {
        let name = match size {
            ImageSize::Original => "original".to_string(),
            ImageSize::Thumbnail(size) => size.to_string(),
        };
        self.candidate_directory("missing", candidate)
            .join(format!("{name}.missing"))
    }

    fn candidate_directory(&self, state: &str, candidate: &Candidate) -> PathBuf {
        self.root
            .join(state)
            .join(Self::candidate_family(candidate))
    }

    pub(crate) fn candidate_family(candidate: &Candidate) -> String {
        candidate.key.clone()
    }

    fn initialize_usage(&self) -> io::Result<()> {
        let mut state = lock(&self.maintenance.state)?;
        let usage = prune_cache(
            &self.root,
            self.maintenance.limits,
            self.maintenance.limits,
            None,
        )?;
        *state = Some(usage);
        Ok(())
    }

    fn write_tracked(&self, path: &Path, bytes: impl Read) -> io::Result<()> {
        let mut state = lock(&self.maintenance.state)?;
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
            if usage.exceeds(self.maintenance.limits) {
                prune_cache(
                    &self.root,
                    self.maintenance.limits,
                    self.maintenance.limits.prune_target(),
                    Some(path),
                )?
            } else {
                usage
            }
        } else {
            prune_cache(
                &self.root,
                self.maintenance.limits,
                self.maintenance.limits,
                Some(path),
            )?
        };
        *state = Some(usage);
        Ok(())
    }

    fn remove_file_tracked(&self, path: &Path) {
        if is_budgeted_path(&self.root, path) {
            self.remove_file_budgeted(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }

    fn remove_file_budgeted(&self, path: &Path) {
        let Ok(mut state) = lock(&self.maintenance.state) else {
            return;
        };
        let previous = file_usage(path);
        if fs::remove_file(path).is_ok() {
            if let Some(usage) = state.as_mut() {
                usage.bytes = usage.bytes.saturating_sub(previous.bytes);
                usage.files = usage.files.saturating_sub(previous.files);
            }
        }
    }

    fn remove_dir_budgeted(&self, path: &Path) -> io::Result<()> {
        let mut state = lock(&self.maintenance.state)?;
        let previous = path_usage(path)?;
        self.remove_dir_notified(path)?;
        if let Some(usage) = state.as_mut() {
            usage.bytes = usage.bytes.saturating_sub(previous.bytes);
            usage.files = usage.files.saturating_sub(previous.files);
        }
        Ok(())
    }

    fn remove_dir_notified(&self, path: &Path) -> io::Result<()> {
        let directory = match fs::read_dir(path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in (ArtworkCacheEntries {
            root: self.root.clone(),
            directories: vec![directory],
        }) {
            let entry = entry?;
            match fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        remove_dir_if_present(path)
    }
}

struct CacheFile {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

fn prune_cache(
    root: &Path,
    trigger: CacheLimits,
    target: CacheLimits,
    preserve: Option<&Path>,
) -> io::Result<CacheUsage> {
    let mut files = Vec::new();
    collect_discretionary_files(root, &mut files)?;
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

fn collect_discretionary_files(root: &Path, files: &mut Vec<CacheFile>) -> io::Result<()> {
    for path in [
        root.join("ready/external"),
        root.join("missing/external"),
        root.join("originals"),
    ] {
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

fn is_budgeted_path(root: &Path, path: &Path) -> bool {
    path.starts_with(root.join("ready/external"))
        || path.starts_with(root.join("missing/external"))
        || path.starts_with(root.join("originals"))
}

fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("artwork cache maintenance lock was poisoned"))
}

pub(crate) fn thumbnail_size(name: &str) -> Option<u32> {
    let (size, extension) = name.split_once('.')?;
    let parsed = size.parse::<u32>().ok()?;
    (parsed > 0 && parsed.to_string() == size && matches!(extension, "png" | "img"))
        .then_some(parsed)
}

pub(crate) fn valid_cache_key(key: &str) -> bool {
    let parts = key.split('/').collect::<Vec<_>>();
    let (state, identity, name) = match parts.as_slice() {
        [state, "native", source, identity, name] if is_digest(source) => {
            (*state, *identity, *name)
        }
        [state, "external" | "playlists", identity, name] => (*state, *identity, *name),
        _ => return false,
    };
    is_digest(identity)
        && match state {
            "ready" => thumbnail_size(name).is_some(),
            "originals" => name.strip_prefix("original.").is_some_and(|extension| {
                matches!(
                    extension,
                    "jpg" | "png" | "gif" | "webp" | "tiff" | "bmp" | "jxl"
                )
            }),
            _ => false,
        }
}

fn is_digest(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn cache_key(root: &Path, path: &Path) -> Option<String> {
    let key = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    valid_cache_key(&key).then_some(key)
}

fn same_file_bytes(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() != bytes.len() as u64 {
        return Ok(false);
    }
    let mut buffer = [0; 16 * 1024];
    for expected in bytes.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..expected.len()])?;
        if &buffer[..expected.len()] != expected {
            return Ok(false);
        }
    }
    Ok(true)
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
    format!("{:x}", Md5::digest(value.as_bytes()))
}

fn atomic_write(path: &Path, mut bytes: impl Read) -> io::Result<()> {
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
    io::copy(&mut bytes, &mut fs::File::create(&temporary)?)?;
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
    use library::ArtworkImage;

    fn native() -> Candidate {
        Candidate::new(
            ArtworkImage::Native {
                source_id: SourceId::new("source"),
                item_id: "album".into(),
                tag: Some("tag".into()),
            },
            None,
        )
    }

    #[test]
    fn a_ready_larger_cover_is_reused_for_a_smaller_preview() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        cache
            .write_ready(&native(), ImageSize::Thumbnail(256), b"normalized")
            .expect("cache cover");

        assert!(
            cache
                .ready_entry(&native(), ImageSize::Thumbnail(96))
                .is_some()
        );
        assert!(
            cache
                .ready_entry(&native(), ImageSize::Thumbnail(512))
                .is_none()
        );
        let external = Candidate::new(
            ArtworkImage::Artist {
                name: "Artist".into(),
                musicbrainz_id: None,
            },
            None,
        );
        cache
            .write_ready(&external, ImageSize::Thumbnail(256), b"normalized")
            .unwrap();
        assert!(
            cache
                .ready_entry(&external, ImageSize::Thumbnail(512))
                .is_none()
        );
        assert!(
            cache
                .ready_entry(&external, ImageSize::Thumbnail(96))
                .is_some()
        );
        let request = crate::CacheImageRequest {
            family: external.key.clone(),
            size: ImageSize::Thumbnail(700),
            fallback: false,
        };
        let keys = [
            format!("ready/{}/512.png", external.key),
            format!("ready/{}/800.png", external.key),
            format!("ready/{}/1024.png", external.key),
        ];
        assert_eq!(
            request.select_key(keys.iter().map(String::as_str)),
            Some(keys[1].as_str())
        );
        let original = crate::CacheImageRequest {
            size: ImageSize::Original,
            ..request.clone()
        };
        assert!(
            original
                .select_key(keys.iter().map(String::as_str))
                .is_none()
        );
        assert_eq!(
            crate::CacheImageRequest {
                fallback: true,
                ..original
            }
            .select_key(keys.iter().map(String::as_str)),
            Some(keys[2].as_str())
        );
        cache
            .write_ready(&external, ImageSize::Thumbnail(4096), b"large")
            .unwrap();
        assert!(
            cache
                .ready_entry(&external, ImageSize::Thumbnail(96))
                .unwrap()
                .path
                .ends_with("256.png")
        );
        assert!(
            cache
                .ready_entries(&external, ImageSize::Thumbnail(1))
                .last()
                .unwrap()
                .path
                .ends_with("4096.png")
        );
    }

    #[test]
    fn source_invalidation_removes_only_that_native_binding_family() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        let source = SourceId::new("source");
        let other = Candidate::new(
            ArtworkImage::Native {
                source_id: SourceId::new("other"),
                item_id: "album".into(),
                tag: Some("tag".into()),
            },
            None,
        );
        cache
            .write_ready(&native(), ImageSize::Thumbnail(256), b"source")
            .expect("cache source cover");
        cache
            .write_ready(&other, ImageSize::Thumbnail(256), b"other")
            .expect("cache other cover");

        cache.invalidate_source(&source).expect("invalidate source");
        assert!(
            cache
                .ready_entry(&native(), ImageSize::Thumbnail(256))
                .is_none()
        );
        assert!(
            cache
                .ready_entry(&other, ImageSize::Thumbnail(256))
                .is_some()
        );
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
