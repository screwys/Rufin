//! The Catalog image: the resident Catalog written to a file at idle and read at start in place
//! of the load. It carries the Store's identity and commit counter; an image whose stamp differs
//! from the Store's is not read. The first Write after an image is written raises the counter.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::mem::size_of;
use std::path::{Path, PathBuf};

use bytemuck::Pod;
use sqlx::SqliteConnection;

use super::records::Source;
use super::snapshot::*;
use super::strings::{StrId, Strings};
use crate::{LibraryResult, SourceKey};

const MAGIC: &[u8; 8] = b"RUFINCAT";
/// Raised when the meaning of a record's fields or flags changes, or its fields are reordered.
const REVISION: u8 = 7;
/// Records are written as they are in memory, so their sizes are part of the format.
const FORMAT: [u8; 8] = [
    REVISION,
    size_of::<TrackRec>() as u8,
    size_of::<AlbumRec>() as u8,
    size_of::<ArtistRec>() as u8,
    size_of::<TagRec>() as u8,
    size_of::<FolderRec>() as u8,
    size_of::<Stats>() as u8,
    0,
];

/// The Store's identity and commit counter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Stamp {
    store: Vec<u8>,
    commits: i64,
}

pub(crate) async fn stamp(connection: &mut SqliteConnection) -> LibraryResult<Option<Stamp>> {
    let row: Option<(Vec<u8>, i64)> =
        sqlx::query_as("SELECT store_id,commits FROM store_commits WHERE singleton=1")
            .fetch_optional(connection)
            .await?;
    Ok(row.map(|(store, commits)| Stamp { store, commits }))
}

/// Raises the commit counter in a fully synced commit and returns the new stamp. Every earlier
/// commit is on disk once it returns, so an image with this stamp holds no commit the Store
/// could lose in a power cut.
pub(crate) async fn seal(connection: &mut SqliteConnection) -> LibraryResult<Stamp> {
    sqlx::query("PRAGMA synchronous=FULL")
        .execute(&mut *connection)
        .await?;
    let counted = count_commit(connection).await;
    sqlx::query("PRAGMA synchronous=NORMAL")
        .execute(&mut *connection)
        .await?;
    counted?;
    stamp(connection)
        .await?
        .ok_or_else(|| crate::LibraryError::InvalidStore("the Store has no commit counter".into()))
}

/// Raises the commit counter, so an image written before this commit no longer matches.
pub(crate) async fn count_commit(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::query("UPDATE store_commits SET commits=commits+1 WHERE singleton=1")
        .execute(connection)
        .await?;
    Ok(())
}

pub(crate) fn path(store: &Path) -> PathBuf {
    store.with_extension("image")
}

/// Writes the image as it encodes, summing what it wrote. The first failure is kept.
struct Out<W> {
    out: W,
    sum: blake3::Hasher,
    failed: std::io::Result<()>,
}

impl<W: std::io::Write> Out<W> {
    fn bytes(&mut self, bytes: &[u8]) {
        self.sum.update(bytes);
        if self.failed.is_ok() {
            self.failed = self.out.write_all(bytes);
        }
    }

    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.bytes(&value.to_le_bytes());
    }

    fn len(&mut self, len: usize) {
        self.i64(len as i64);
    }

    /// Plain records as they are in memory. Every target Rufin runs on is little-endian.
    fn slice<T: Pod>(&mut self, items: &[T]) {
        self.len(items.len());
        self.bytes(bytemuck::cast_slice(items));
    }

    fn text(&mut self, text: &str) {
        self.slice(text.as_bytes());
    }

    fn optional_text(&mut self, text: Option<&[u8]>) {
        self.u8(u8::from(text.is_some()));
        self.slice(text.unwrap_or_default());
    }

    fn optional_i64(&mut self, value: Option<i64>) {
        self.u8(u8::from(value.is_some()));
        self.i64(value.unwrap_or_default());
    }

    fn family<R: Pod>(&mut self, family: &Family<R>) {
        self.slice(&family.records);
        self.keys(family);
    }

    fn keys<R>(&mut self, family: &Family<R>) {
        self.slice(&family.keys);
        self.i64(family.base);
        self.len(family.live);
    }
}

/// A truncated or foreign image reads as no image.
struct In<'a>(&'a [u8]);

impl In<'_> {
    fn take(&mut self, len: usize) -> Option<&[u8]> {
        let (head, tail) = self.0.split_at_checked(len)?;
        self.0 = tail;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.array()?))
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.i64()?).ok()
    }

    fn slice<T: Pod>(&mut self) -> Option<Vec<T>> {
        let len = self.len()?.checked_mul(size_of::<T>())?;
        Some(bytemuck::pod_collect_to_vec(self.take(len)?))
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.slice()?).ok()
    }

    fn optional_text(&mut self) -> Option<Option<Vec<u8>>> {
        let known = self.u8()? != 0;
        let bytes = self.slice()?;
        Some(known.then_some(bytes))
    }

    fn optional_i64(&mut self) -> Option<Option<i64>> {
        let known = self.u8()? != 0;
        let value = self.i64()?;
        Some(known.then_some(value))
    }

    fn family<R: Pod>(&mut self) -> Option<Family<R>> {
        let records = self.slice()?;
        self.keys(records)
    }

    fn keys<R>(&mut self, records: Vec<R>) -> Option<Family<R>> {
        let keys: Vec<u32> = self.slice()?;
        if keys
            .iter()
            .any(|slot| *slot != NONE && *slot as usize >= records.len())
        {
            return None;
        }
        Some(Family {
            records,
            keys,
            base: self.i64()?,
            live: self.len()?,
        })
    }
}

/// What an image of the Store with this stamp starts with.
fn header(stamp: &Stamp) -> Vec<u8> {
    let mut out = Out {
        out: Vec::new(),
        sum: blake3::Hasher::new(),
        failed: Ok(()),
    };
    out.bytes(MAGIC);
    out.bytes(&FORMAT);
    out.slice(&stamp.store);
    out.i64(stamp.commits);
    out.out
}

/// The image after its header, followed by the sum of the header and everything after it.
fn encode<W: std::io::Write>(snapshot: &Snapshot, stamp: &Stamp, out: W) -> std::io::Result<W> {
    let mut out = Out {
        out,
        sum: blake3::Hasher::new(),
        failed: Ok(()),
    };
    out.sum.update(&header(stamp));
    out.i64(snapshot.revision as i64);
    let strings = &snapshot.strings;
    out.len(strings.chunks.len());
    for chunk in &strings.chunks {
        out.slice(chunk);
    }
    out.slice(&strings.entries);
    out.slice(&strings.interned.values().copied().collect::<Vec<StrId>>());
    out.len(strings.garbage);
    out.slice(&snapshot.edges);
    out.len(snapshot.sources.len());
    for source in snapshot.sources.values() {
        out.i64(source.key.raw());
        out.text(&source.object);
        out.optional_text(source.kind.as_deref().map(str::as_bytes));
        out.text(&source.name);
        out.u8(u8::from(source.complete));
        out.optional_text(source.freshness.as_deref());
        out.optional_i64(source.synced_at);
    }
    out.slice(
        &snapshot
            .source_slots
            .iter()
            .map(|key| key.raw())
            .collect::<Vec<_>>(),
    );
    out.family(&snapshot.tracks);
    out.family(&snapshot.albums);
    out.family(&snapshot.artists);
    out.family(&snapshot.tags);
    out.family(&snapshot.folders);
    out.slice(&snapshot.stats.keys().copied().collect::<Vec<_>>());
    out.slice(&snapshot.stats.values().copied().collect::<Vec<_>>());
    let playlists = &snapshot.playlists;
    out.len(playlists.records.len());
    for playlist in &playlists.records {
        out.i64(playlist.key);
        out.u32(playlist.name.raw());
        out.u32(playlist.object.raw());
        out.i64(playlist.folder);
        out.u32(playlist.created);
        out.u16(playlist.source);
        out.u16(playlist.flags);
        out.len(playlist.entries.len());
        for entry in &playlist.entries {
            out.i64(entry.key);
            out.slice(&entry.rank);
            out.u32(entry.track);
        }
    }
    out.keys(playlists);
    out.len(snapshot.smart_playlists.len());
    for smart in snapshot.smart_playlists.values() {
        out.i64(smart.key);
        out.text(&smart.object);
        out.text(&smart.name);
        out.optional_i64(smart.folder);
        out.i64(smart.position);
        out.text(&serde_json::to_string(&smart.definition).unwrap_or_default());
    }
    out.len(snapshot.playlist_folders.len());
    for folder in snapshot.playlist_folders.values() {
        out.i64(folder.key);
        out.text(&folder.object);
        out.text(&folder.name);
        out.u8(u8::from(folder.smart));
    }
    let sum = *out.sum.finalize().as_bytes();
    out.failed?;
    out.out.write_all(&sum)?;
    Ok(out.out)
}

/// Decodes what follows the header.
fn decode(bytes: &[u8]) -> Option<Snapshot> {
    let mut input = In(bytes);
    let mut snapshot = Snapshot {
        revision: input.i64()? as u64,
        ..Snapshot::default()
    };
    let chunks = input.len()?;
    let mut strings = Strings {
        chunks: Vec::with_capacity(chunks.min(1 << 16)),
        entries: Vec::new(),
        interned: HashMap::new(),
        garbage: 0,
    };
    for _ in 0..chunks {
        strings.chunks.push(input.slice()?);
    }
    strings.entries = input.slice()?;
    let interned: Vec<StrId> = input.slice()?;
    strings.interned.reserve(interned.len());
    for id in interned {
        if id.raw() as usize >= strings.entries.len() {
            return None;
        }
        let hash = super::strings::hash(strings.get(id).as_bytes());
        strings.interned.insert(hash, id);
    }
    strings.garbage = input.len()?;
    snapshot.strings = strings;
    snapshot.edges = input.slice()?;
    let sources = input.len()?;
    for _ in 0..sources {
        let key = SourceKey::from_raw(input.i64()?);
        snapshot.sources.insert(
            key,
            Source {
                key,
                object: input.text()?,
                kind: input
                    .optional_text()?
                    .map(String::from_utf8)
                    .transpose()
                    .ok()?,
                name: input.text()?,
                complete: input.u8()? != 0,
                freshness: input.optional_text()?,
                synced_at: input.optional_i64()?,
            },
        );
    }
    snapshot.source_slots = input
        .slice::<i64>()?
        .into_iter()
        .map(SourceKey::from_raw)
        .collect();
    snapshot.tracks = input.family()?;
    snapshot.download_keys = snapshot
        .tracks
        .records
        .iter()
        .filter(|record| record.flags & DOWNLOADED != 0)
        .map(|record| record.key)
        .collect();
    snapshot.albums = input.family()?;
    snapshot.artists = input.family()?;
    snapshot.tags = input.family()?;
    snapshot.folders = input.family()?;
    let slots: Vec<u32> = input.slice()?;
    let stats: Vec<Stats> = input.slice()?;
    if slots.len() != stats.len() {
        return None;
    }
    snapshot.stats = slots.into_iter().zip(stats).collect();
    let playlists = input.len()?;
    let mut records = Vec::with_capacity(playlists.min(1 << 20));
    for _ in 0..playlists {
        let mut playlist = PlaylistRec {
            key: input.i64()?,
            name: StrId::from_raw(input.u32()?),
            object: StrId::from_raw(input.u32()?),
            folder: input.i64()?,
            created: input.u32()?,
            source: input.u16()?,
            flags: input.u16()?,
            entries: Vec::new(),
        };
        let entries = input.len()?;
        playlist.entries.reserve(entries.min(1 << 20));
        for _ in 0..entries {
            playlist.entries.push(Entry {
                key: input.i64()?,
                rank: input.slice()?.into(),
                track: input.u32()?,
            });
        }
        records.push(playlist);
    }
    snapshot.playlists = input.keys(records)?;
    let smart = input.len()?;
    for _ in 0..smart {
        let key = input.i64()?;
        snapshot.smart_playlists.insert(
            key,
            SmartPlaylist {
                key,
                object: input.text()?,
                name: input.text()?,
                folder: input.optional_i64()?,
                position: input.i64()?,
                definition: serde_json::from_str(&input.text()?).ok()?,
            },
        );
    }
    let folders = input.len()?;
    for _ in 0..folders {
        let key = input.i64()?;
        snapshot.playlist_folders.insert(
            key,
            PlaylistFolder {
                key,
                object: input.text()?,
                name: input.text()?,
                smart: input.u8()? != 0,
            },
        );
    }
    input.0.is_empty().then_some(snapshot)
}

/// Writes the image beside the Store, replacing the previous one only once it is complete.
pub(crate) fn write(store: &Path, snapshot: &Snapshot, stamp: &Stamp) -> std::io::Result<()> {
    let target = path(store);
    let partial = target.with_extension("image-partial");
    let mut file = std::io::BufWriter::new(std::fs::File::create(&partial)?);
    file.write_all(&header(stamp))?;
    let file = encode(snapshot, stamp, file)?.into_inner()?;
    file.sync_data()?;
    drop(file);
    std::fs::rename(partial, target)
}

/// The Catalog the image holds, when it matches the Store's stamp.
pub(crate) fn read(store: &Path, stamp: &Stamp) -> Option<Snapshot> {
    let mut file = std::fs::File::open(path(store)).ok()?;
    let header = header(stamp);
    let mut start = vec![0; header.len()];
    let mut bytes = Vec::new();
    // An image of another state is rejected before the rest is read.
    let read = file.read_exact(&mut start).is_ok()
        && start == header
        && file.read_to_end(&mut bytes).is_ok();
    // Decode trusts what it reads, so a damaged image is not decoded.
    let (bytes, sum) = bytes.split_at_checked(bytes.len().checked_sub(32)?)?;
    let summed = || {
        blake3::Hasher::new()
            .update(&header)
            .update(bytes)
            .finalize()
    };
    let snapshot = (read && summed().as_bytes() == sum)
        .then(|| decode(bytes))
        .flatten();
    if snapshot.is_none() {
        tracing::debug!("the Catalog image does not match the Store");
    }
    snapshot
}

pub(crate) fn remove(store: &Path) {
    let _ = std::fs::remove_file(path(store));
}

#[cfg(test)]
pub(crate) fn round_trip(snapshot: &Snapshot) -> Option<Snapshot> {
    let stamp = Stamp {
        store: vec![1; 16],
        commits: 7,
    };
    let bytes = encode(snapshot, &stamp, Vec::new()).ok()?;
    decode(&bytes[..bytes.len() - 32])
}
