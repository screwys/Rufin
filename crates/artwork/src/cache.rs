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
                        part.as_os_str() == "originals" || part.as_os_str() == "displays"
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
    let current = root.join(CACHE_LAYOUT);
    fs::create_dir_all(&current)?;
    Ok(current)
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
        self.path_for_key(&format!("originals/{}/original.png", request.family))?;
        if let ImageSize::Thumbnail(size) = request.size
            && let Some(entry) = self.display_file(&request.family, size)?
        {
            return Ok(Some(entry));
        }
        let directory = match fs::read_dir(self.root.join("originals").join(&request.family)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        for entry in directory {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file()
                && metadata.len() > 0
                && let Some(key) = cache_key(&self.root, &entry.path())
            {
                return Ok(Some(ArtworkCacheEntry {
                    key,
                    path: entry.path(),
                }));
            }
        }
        Ok(None)
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

    pub(crate) fn ready_entry(&self, candidate: &Candidate) -> Option<CacheEntry> {
        self.candidate_directory("originals", candidate)
            .read_dir()
            .ok()?
            .filter_map(Result::ok)
            .find_map(|entry| {
                let path = entry.path();
                cache_key(&self.root, &path)?;
                let metadata = entry.metadata().ok()?;
                if metadata.is_file() && metadata.len() > 0 {
                    Some(CacheEntry { path })
                } else {
                    self.remove_file_tracked(&path);
                    None
                }
            })
    }

    pub(crate) fn write_ready(
        &self,
        candidate: &Candidate,
        size: ImageSize,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        let path = match size {
            ImageSize::Original => {
                let extension = crate::decode::original_extension(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                self.candidate_directory("originals", candidate)
                    .join(format!("original.{extension}"))
            }
            ImageSize::Thumbnail(size) => self
                .candidate_directory("displays", candidate)
                .join(format!("{size}.image")),
        };
        if is_budgeted_path(&self.root, &path) {
            self.write_tracked(&path, bytes)?;
        } else {
            atomic_write(&path, bytes)?;
        }
        self.remove_file_tracked(&self.missing_path(candidate));
        Ok(path)
    }

    pub(crate) fn display_path(&self, candidate: &Candidate, size: u32) -> Option<PathBuf> {
        self.display_file(&candidate.key, size)
            .ok()?
            .map(|entry| entry.path)
    }

    fn display_file(&self, family: &str, size: u32) -> io::Result<Option<ArtworkCacheEntry>> {
        let key = format!("displays/{family}/{size}.image");
        if let Some(path) = self.file(&key)? {
            return Ok(Some(ArtworkCacheEntry { key, path }));
        }
        let directory = match fs::read_dir(self.root.join("displays").join(family)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(directory
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let available = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".image")?
                    .parse::<u32>()
                    .ok()?;
                if available < size || file_usage(&path).bytes == 0 {
                    return None;
                }
                let key = cache_key(&self.root, &path)?;
                Some((available, ArtworkCacheEntry { key, path }))
            })
            .min_by_key(|(size, _)| *size)
            .map(|(_, entry)| entry))
    }

    pub(crate) fn best_display(&self, candidate: &Candidate) -> Option<PathBuf> {
        self.candidate_directory("displays", candidate)
            .read_dir()
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let size = entry
                    .file_name()
                    .to_str()?
                    .strip_suffix(".image")?
                    .parse::<u32>()
                    .ok()?;
                let path = entry.path();
                (size > 0 && file_usage(&path).bytes > 0).then_some((size, path))
            })
            .max_by_key(|(size, _)| *size)
            .map(|(_, path)| path)
    }

    pub(crate) fn remove_ready(&self, path: &Path) {
        self.remove_file_tracked(path);
    }

    pub(crate) fn is_missing(&self, candidate: &Candidate) -> bool {
        self.missing_path(candidate).is_file()
    }

    pub(crate) fn mark_missing(&self, candidate: &Candidate) -> io::Result<()> {
        let path = self.missing_path(candidate);
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
        remove_dir_if_present(&self.root.join("missing/native").join(&source))?;
        remove_dir_if_present(&self.root.join("originals/native").join(&source))?;
        self.remove_dir_budgeted(&self.root.join("displays/native").join(source))
    }

    pub(crate) fn invalidate_image(&self, candidate: &Candidate) -> io::Result<()> {
        for state in ["missing", "originals", "displays"] {
            let path = self.candidate_directory(state, candidate);
            if is_budgeted_path(&self.root, &path) {
                self.remove_dir_budgeted(&path)?;
            } else {
                remove_dir_if_present(&path)?;
            }
        }
        Ok(())
    }

    fn missing_path(&self, candidate: &Candidate) -> PathBuf {
        self.candidate_directory("missing", candidate)
            .join("original.missing")
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
        remove_dir_if_present(&self.root.join("ready"))?;
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
        root.join("missing/external"),
        root.join("originals/external"),
        root.join("displays"),
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
    path.starts_with(root.join("missing/external"))
        || path.starts_with(root.join("originals/external"))
        || path.starts_with(root.join("displays"))
}

fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("artwork cache maintenance lock was poisoned"))
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
            "displays" => name
                .strip_suffix(".image")
                .is_some_and(|size| size.parse::<u32>().is_ok_and(|size| size > 0)),
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

#[derive(Clone, Debug)]
pub(crate) struct CacheEntry {
    pub(crate) path: PathBuf,
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
    fn cached_original_is_returned_for_its_binding() {
        let directory = tempfile::tempdir().expect("temporary artwork cache");
        let cache = FilesystemCache::new(directory.path().to_path_buf()).expect("open cache");
        let bytes = b"\x89PNG\r\n\x1a\n";
        cache.mark_missing(&native()).unwrap();
        let path = cache
            .write_ready(&native(), ImageSize::Original, bytes)
            .expect("cache original");
        assert_eq!(cache.ready_entry(&native()).unwrap().path, path);
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(!cache.is_missing(&native()));
        let request = crate::CacheImageRequest {
            family: native().key,
            size: ImageSize::Original,
        };
        let display = cache
            .write_ready(&native(), ImageSize::Thumbnail(540), bytes)
            .unwrap();
        let smaller = cache
            .write_ready(&native(), ImageSize::Thumbnail(256), bytes)
            .unwrap();
        assert_eq!(cache.display_path(&native(), 540), Some(display.clone()));
        assert_eq!(cache.display_path(&native(), 512), Some(display.clone()));
        assert_eq!(cache.display_path(&native(), 96), Some(smaller.clone()));
        assert!(cache.display_path(&native(), 768).is_none());
        assert_eq!(
            cache.family_file(&request).unwrap().unwrap().path,
            cache.ready_entry(&native()).unwrap().path
        );
        let display_request = crate::CacheImageRequest {
            size: ImageSize::Thumbnail(540),
            ..request.clone()
        };
        assert_eq!(
            cache.family_file(&display_request).unwrap().unwrap().path,
            display
        );
        let keys = [
            format!("originals/{}/original.png", request.family),
            format!("displays/{}/540.image", request.family),
            format!("displays/{}/256.image", request.family),
        ];
        assert_eq!(
            request.select_key(keys.iter().map(String::as_str)),
            Some(keys[0].as_str())
        );
        assert_eq!(
            display_request.select_key(keys.iter().map(String::as_str)),
            Some(keys[1].as_str())
        );
        assert_eq!(request.select_key([keys[1].as_str()]), None);
        let other_size = crate::CacheImageRequest {
            size: ImageSize::Thumbnail(96),
            ..request.clone()
        };
        assert_eq!(
            other_size.select_key(keys.iter().map(String::as_str)),
            Some(keys[2].as_str())
        );
        assert_eq!(
            cache.family_file(&other_size).unwrap().unwrap().path,
            smaller
        );
        let larger = crate::CacheImageRequest {
            size: ImageSize::Thumbnail(768),
            ..request.clone()
        };
        assert_eq!(larger.select_key([keys[1].as_str()]), None);
        assert_eq!(
            cache.family_file(&larger).unwrap().unwrap().path,
            cache.ready_entry(&native()).unwrap().path
        );
        let external = Candidate::new(
            ArtworkImage::Artist {
                name: "Artist".into(),
                musicbrainz_id: None,
            },
            None,
        );
        cache
            .write_ready(&external, ImageSize::Original, bytes)
            .unwrap();
        cache
            .write_ready(&external, ImageSize::Thumbnail(512), bytes)
            .unwrap();
        let empty = CacheLimits { bytes: 0, files: 0 };
        let usage = prune_cache(directory.path(), empty, empty, None).unwrap();
        assert_eq!((usage.bytes, usage.files), (0, 0));
        assert!(cache.ready_entry(&external).is_none());
        assert!(cache.ready_entry(&native()).is_some());
        assert!(cache.display_path(&external, 512).is_none());
        assert!(cache.best_display(&native()).is_none());
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
            .write_ready(&native(), ImageSize::Original, b"\x89PNG\r\n\x1a\n")
            .expect("cache source cover");
        cache
            .write_ready(&other, ImageSize::Original, b"\x89PNG\r\n\x1a\n")
            .expect("cache other cover");
        cache
            .write_ready(&native(), ImageSize::Thumbnail(512), b"\x89PNG\r\n\x1a\n")
            .unwrap();
        cache
            .write_ready(&other, ImageSize::Thumbnail(512), b"\x89PNG\r\n\x1a\n")
            .unwrap();

        cache.invalidate_source(&source).expect("invalidate source");
        assert!(cache.ready_entry(&native()).is_none());
        assert!(cache.ready_entry(&other).is_some());
        assert!(cache.display_path(&native(), 512).is_none());
        assert!(cache.display_path(&other, 512).is_some());
        cache.invalidate_image(&other).expect("invalidate image");
        assert!(cache.ready_entry(&other).is_none());
        assert!(cache.display_path(&other, 512).is_none());
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
