//! The Catalog image: the resident Catalog written to a file at idle and read at start in place
//! of the load. It carries the Store's identity and commit counter; an image whose stamp differs
//! from the Store's is not read. The first Write after an image is written raises the counter.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sqlx::SqliteConnection;

use super::records::Source;
use super::snapshot::*;
use super::strings::{StrId, Strings};
use crate::tags::TagKind;
use crate::{LibraryResult, SourceKey};

const MAGIC: &[u8; 8] = b"RUFINCAT";
/// Raised whenever the layout or the flags of a record change.
const FORMAT: u32 = 3;

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

struct Out(Vec<u8>);

impl Out {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend_from_slice(&value.to_le_bytes());
    }

    fn len(&mut self, len: usize) {
        self.u64(len as u64);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.len(bytes.len());
        self.0.extend_from_slice(bytes);
    }

    fn text(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    fn optional_bytes(&mut self, bytes: Option<&[u8]>) {
        self.u8(u8::from(bytes.is_some()));
        if let Some(bytes) = bytes {
            self.bytes(bytes);
        }
    }

    fn optional_i64(&mut self, value: Option<i64>) {
        self.u8(u8::from(value.is_some()));
        self.i64(value.unwrap_or_default());
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

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.u64()?).ok()
    }

    fn bytes(&mut self) -> Option<Vec<u8>> {
        let len = self.len()?;
        Some(self.take(len)?.to_vec())
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?).ok()
    }

    fn optional_bytes(&mut self) -> Option<Option<Vec<u8>>> {
        Some(match self.u8()? {
            0 => None,
            _ => Some(self.bytes()?),
        })
    }

    fn optional_i64(&mut self) -> Option<Option<i64>> {
        let known = self.u8()? != 0;
        let value = self.i64()?;
        Some(known.then_some(value))
    }
}

/// A packed record's fields, written and read in declaration order.
trait Fields: Sized {
    fn put(&self, out: &mut Out);
    fn take(input: &mut In<'_>) -> Option<Self>;
}

macro_rules! fields {
    ($record:ident { $($field:ident: $kind:ident),* $(,)? }) => {
        impl Fields for $record {
            fn put(&self, out: &mut Out) {
                $(field::$kind::put(&self.$field, out);)*
            }

            fn take(input: &mut In<'_>) -> Option<Self> {
                Some(Self { $($field: field::$kind::take(input)?,)* })
            }
        }
    };
}

/// Writers and readers of one field type each.
mod field {
    use super::*;

    macro_rules! scalar {
        ($name:ident, $type:ty, $put:ident, $take:ident, $from:expr, $to:expr) => {
            pub(super) mod $name {
                use super::*;

                pub(crate) fn put(value: &$type, out: &mut Out) {
                    out.$put($from(*value));
                }

                pub(crate) fn take(input: &mut In<'_>) -> Option<$type> {
                    Some($to(input.$take()?))
                }
            }
        };
    }

    scalar!(byte, u8, u8, u8, |value| value, |value| value);
    scalar!(short, u16, u16, u16, |value| value, |value| value);
    scalar!(word, u32, u32, u32, |value| value, |value| value);
    scalar!(long, i64, i64, i64, |value| value, |value| value);
    scalar!(
        signed,
        i16,
        u16,
        u16,
        |value: i16| value as u16,
        |value: u16| value as i16
    );
    scalar!(text, StrId, u32, u32, StrId::raw, StrId::from_raw);
    scalar!(
        kind,
        TagKind,
        u8,
        u8,
        |value: TagKind| value as u8,
        |value| if value == 1 {
            TagKind::Mood
        } else {
            TagKind::Genre
        }
    );

    pub(super) mod lens4 {
        use super::*;

        pub(crate) fn put(value: &[u16; 4], out: &mut Out) {
            value.iter().for_each(|len| out.u16(*len));
        }

        pub(crate) fn take(input: &mut In<'_>) -> Option<[u16; 4]> {
            Some([input.u16()?, input.u16()?, input.u16()?, input.u16()?])
        }
    }

    pub(super) mod lens2 {
        use super::*;

        pub(crate) fn put(value: &[u16; 2], out: &mut Out) {
            value.iter().for_each(|len| out.u16(*len));
        }

        pub(crate) fn take(input: &mut In<'_>) -> Option<[u16; 2]> {
            Some([input.u16()?, input.u16()?])
        }
    }

    pub(super) mod entries {
        use super::*;
        use crate::catalog::snapshot::Entry;

        pub(crate) fn put(value: &[Entry<u32>], out: &mut Out) {
            out.len(value.len());
            for entry in value {
                out.i64(entry.key);
                out.bytes(&entry.rank);
                out.u32(entry.track);
            }
        }

        pub(crate) fn take(input: &mut In<'_>) -> Option<Vec<Entry<u32>>> {
            let len = input.len()?;
            let mut entries = Vec::with_capacity(len.min(1 << 20));
            for _ in 0..len {
                entries.push(Entry {
                    key: input.i64()?,
                    rank: input.bytes()?.into(),
                    track: input.u32()?,
                });
            }
            Some(entries)
        }
    }
}

fields!(TrackRec {
    key: long,
    title: text,
    artist: text,
    album_title: text,
    sort: text,
    comment: text,
    format: text,
    album: word,
    duration_ms: word,
    added: word,
    release: word,
    edges: word,
    lens: lens4,
    year: signed,
    disc: short,
    number: short,
    bpm: short,
    source: short,
    flags: short,
    native_rating: byte,
    rating: byte,
});

fields!(AlbumRec {
    key: long,
    title: text,
    artist: text,
    sort: text,
    added: word,
    release: word,
    edges: word,
    lens: lens2,
    year: signed,
    source: short,
    flags: short,
    native_rating: byte,
    rating: byte,
});

fields!(ArtistRec {
    key: long,
    name: text,
    sort: text,
    source: short,
    flags: short,
    native_rating: byte,
    rating: byte,
});

fields!(TagRec {
    key: long,
    name: text,
    sort: text,
    source: short,
    flags: short,
    kind: kind,
});

fields!(FolderRec {
    key: long,
    name: text,
    sort: text,
    source: short,
    flags: short,
});

fields!(PlaylistRec {
    key: long,
    name: text,
    folder: long,
    created: word,
    source: short,
    flags: short,
    entries: entries,
});

fn put_family<R: Fields>(out: &mut Out, family: &Family<R>) {
    out.len(family.records.len());
    for record in &family.records {
        record.put(out);
    }
}

fn take_family<R: Fields + Packed>(input: &mut In<'_>) -> Option<Family<R>> {
    let len = input.len()?;
    let mut family = Family {
        records: Vec::with_capacity(len.min(1 << 24)),
        keys: HashMap::with_capacity(len.min(1 << 24)),
    };
    for slot in 0..len {
        let record = R::take(input)?;
        if !record.deleted() {
            family.keys.insert(record.key(), slot as u32);
        }
        family.records.push(record);
    }
    Some(family)
}

pub(crate) fn encode(snapshot: &Snapshot, stamp: &Stamp) -> Vec<u8> {
    let mut out = Out(Vec::with_capacity(snapshot.bytes() + 4_096));
    out.0.extend_from_slice(MAGIC);
    out.u32(FORMAT);
    out.bytes(&stamp.store);
    out.i64(stamp.commits);
    out.u64(snapshot.revision);
    let strings = &snapshot.strings;
    out.len(strings.chunks.len());
    for chunk in &strings.chunks {
        out.bytes(chunk);
    }
    out.len(strings.entries.len());
    for entry in &strings.entries {
        out.u64(*entry);
    }
    out.len(strings.interned.len());
    for id in strings.interned.values() {
        out.u32(id.raw());
    }
    out.len(snapshot.edges.len());
    for edge in &snapshot.edges {
        out.u32(*edge);
    }
    out.len(snapshot.sources.len());
    for source in snapshot.sources.values() {
        out.i64(source.key.raw());
        out.text(&source.object);
        out.optional_bytes(source.kind.as_deref().map(str::as_bytes));
        out.text(&source.name);
        out.u8(u8::from(source.complete));
        out.optional_bytes(source.freshness.as_deref());
    }
    out.len(snapshot.source_slots.len());
    for key in &snapshot.source_slots {
        out.i64(key.raw());
    }
    put_family(&mut out, &snapshot.tracks);
    put_family(&mut out, &snapshot.albums);
    put_family(&mut out, &snapshot.artists);
    put_family(&mut out, &snapshot.tags);
    put_family(&mut out, &snapshot.folders);
    out.len(snapshot.stats.len());
    for (slot, stats) in &snapshot.stats {
        out.u32(*slot);
        out.u32(stats.plays);
        out.u32(stats.skips);
        out.u32(stats.last_played);
        out.u32(stats.native_plays);
        out.u32(stats.native_last_played);
    }
    put_family(&mut out, &snapshot.playlists);
    out.len(snapshot.smart_playlists.len());
    for smart in snapshot.smart_playlists.values() {
        out.i64(smart.key);
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
    out.0
}

fn decode(bytes: &[u8], stamp: &Stamp) -> Option<Snapshot> {
    let mut input = In(bytes);
    if input.take(MAGIC.len())? != MAGIC || input.u32()? != FORMAT {
        return None;
    }
    if input.bytes()? != stamp.store || input.i64()? != stamp.commits {
        return None;
    }
    let mut snapshot = Snapshot {
        revision: input.u64()?,
        ..Snapshot::default()
    };
    let chunks = input.len()?;
    let mut strings = Strings {
        chunks: Vec::with_capacity(chunks.min(1 << 16)),
        entries: Vec::new(),
        interned: HashMap::new(),
    };
    for _ in 0..chunks {
        strings.chunks.push(input.bytes()?);
    }
    let entries = input.len()?;
    strings.entries.reserve_exact(entries.min(1 << 26));
    for _ in 0..entries {
        strings.entries.push(input.u64()?);
    }
    let interned = input.len()?;
    strings.interned.reserve(interned.min(1 << 24));
    for _ in 0..interned {
        let id = StrId::from_raw(input.u32()?);
        if id.raw() as usize >= strings.entries.len() {
            return None;
        }
        let hash = super::strings::hash(strings.get(id).as_bytes());
        strings.interned.insert(hash, id);
    }
    snapshot.strings = strings;
    let edges = input.len()?;
    snapshot.edges.reserve_exact(edges.min(1 << 26));
    for _ in 0..edges {
        snapshot.edges.push(input.u32()?);
    }
    let sources = input.len()?;
    for _ in 0..sources {
        let key = SourceKey::from_raw(input.i64()?);
        snapshot.sources.insert(
            key,
            Source {
                key,
                object: input.text()?,
                kind: input
                    .optional_bytes()?
                    .map(String::from_utf8)
                    .transpose()
                    .ok()?,
                name: input.text()?,
                complete: input.u8()? != 0,
                freshness: input.optional_bytes()?,
            },
        );
    }
    let slots = input.len()?;
    for _ in 0..slots {
        snapshot
            .source_slots
            .push(SourceKey::from_raw(input.i64()?));
    }
    snapshot.tracks = take_family(&mut input)?;
    snapshot.albums = take_family(&mut input)?;
    snapshot.artists = take_family(&mut input)?;
    snapshot.tags = take_family(&mut input)?;
    snapshot.folders = take_family(&mut input)?;
    let stats = input.len()?;
    snapshot.stats.reserve(stats.min(1 << 24));
    for _ in 0..stats {
        let slot = input.u32()?;
        snapshot.stats.insert(
            slot,
            Stats {
                plays: input.u32()?,
                skips: input.u32()?,
                last_played: input.u32()?,
                native_plays: input.u32()?,
                native_last_played: input.u32()?,
            },
        );
    }
    snapshot.playlists = take_family(&mut input)?;
    let smart = input.len()?;
    let mut smart_playlists = BTreeMap::new();
    for _ in 0..smart {
        let key = input.i64()?;
        smart_playlists.insert(
            key,
            SmartPlaylist {
                key,
                name: input.text()?,
                folder: input.optional_i64()?,
                position: input.i64()?,
                definition: serde_json::from_str(&input.text()?).ok()?,
            },
        );
    }
    snapshot.smart_playlists = smart_playlists;
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
pub(crate) fn write(store: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = path(store);
    let partial = target.with_extension("image-partial");
    let mut file = std::fs::File::create(&partial)?;
    file.write_all(bytes)?;
    file.sync_data()?;
    drop(file);
    std::fs::rename(partial, target)
}

/// The Catalog the image holds, when it matches the Store's stamp.
pub(crate) fn read(store: &Path, stamp: &Stamp) -> Option<Snapshot> {
    let bytes = std::fs::read(path(store)).ok()?;
    let snapshot = decode(&bytes, stamp);
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
    decode(&encode(snapshot, &stamp), &stamp)
}
