//! Resident records hold 4-byte string ids into one append-only arena.
//! Written bytes never move. Replaced strings stay as garbage until the next full load.

use std::collections::HashMap;

const CHUNK: usize = 1 << 20;
const OFFSET_BITS: u32 = 20;
const LEN_BITS: u32 = 24;
const MAX_LEN: usize = (1 << LEN_BITS) - 1;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct StrId(u32);

impl StrId {
    pub(crate) const EMPTY: Self = Self(0);
    pub(crate) const NONE: Self = Self(u32::MAX);

    pub(crate) fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub(crate) fn raw(self) -> u32 {
        self.0
    }
}

pub(crate) struct Strings {
    pub(super) chunks: Vec<Vec<u8>>,
    // chunk << 44 | offset << 24 | length
    pub(super) entries: Vec<u64>,
    pub(super) interned: HashMap<u64, StrId>,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            entries: vec![0],
            interned: HashMap::new(),
        }
    }
}

impl Strings {
    /// Stores a string that rarely repeats.
    pub(crate) fn push(&mut self, text: &str) -> StrId {
        if text.is_empty() {
            return StrId::EMPTY;
        }
        let mut end = text.len().min(MAX_LEN);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let bytes = &text.as_bytes()[..end];
        if self
            .chunks
            .last()
            .is_none_or(|chunk| chunk.capacity() - chunk.len() < bytes.len())
        {
            self.chunks.push(Vec::with_capacity(CHUNK.max(bytes.len())));
        }
        let chunk = self.chunks.len() - 1;
        let offset = self.chunks[chunk].len();
        self.chunks[chunk].extend_from_slice(bytes);
        // A string longer than one chunk owns its chunk and starts at zero.
        self.entries.push(
            (chunk as u64) << (OFFSET_BITS + LEN_BITS)
                | (offset as u64) << LEN_BITS
                | bytes.len() as u64,
        );
        StrId(self.entries.len() as u32 - 1)
    }

    /// Stores a string that repeats across records, such as names.
    pub(crate) fn intern(&mut self, text: &str) -> StrId {
        if text.is_empty() {
            return StrId::EMPTY;
        }
        let hash = hash(text.as_bytes());
        if let Some(&id) = self.interned.get(&hash) {
            if self.get(id) == text {
                return id;
            }
            return self.push(text);
        }
        let id = self.push(text);
        self.interned.insert(hash, id);
        id
    }

    /// Keeps the previous id when its text is unchanged, so rewrites leave no garbage.
    pub(crate) fn text(&mut self, previous: StrId, text: &str, intern: bool) -> StrId {
        if previous != StrId::NONE && self.get(previous) == text {
            previous
        } else if intern {
            self.intern(text)
        } else {
            self.push(text)
        }
    }

    pub(crate) fn optional(&mut self, previous: StrId, text: Option<&str>, intern: bool) -> StrId {
        match text {
            None => StrId::NONE,
            Some(text) => self.text(previous, text, intern),
        }
    }

    /// Copies another arena's strings here and returns the id each of its ids has here. An
    /// interned string this arena holds already is not copied again.
    pub(crate) fn append(&mut self, other: &Strings) -> Vec<StrId> {
        let interned = other
            .interned
            .iter()
            .map(|(hash, id)| (id.0, *hash))
            .collect::<HashMap<_, _>>();
        let mut ids = Vec::with_capacity(other.entries.len());
        ids.push(StrId::EMPTY);
        for raw in 1..other.entries.len() as u32 {
            let text = other.get(StrId(raw));
            let id = match interned.get(&raw) {
                Some(hash) => match self.interned.get(hash) {
                    Some(&id) if self.get(id) == text => id,
                    Some(_) => self.push(text),
                    None => {
                        let id = self.push(text);
                        self.interned.insert(*hash, id);
                        id
                    }
                },
                None => self.push(text),
            };
            ids.push(id);
        }
        ids
    }

    pub(crate) fn get(&self, id: StrId) -> &str {
        if id == StrId::EMPTY || id == StrId::NONE {
            return "";
        }
        let entry = self.entries[id.0 as usize];
        let chunk = (entry >> (OFFSET_BITS + LEN_BITS)) as usize;
        let offset = (entry >> LEN_BITS & ((1 << OFFSET_BITS) - 1)) as usize;
        let len = (entry & ((1 << LEN_BITS) - 1)) as usize;
        std::str::from_utf8(&self.chunks[chunk][offset..offset + len]).unwrap_or_default()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.chunks.iter().map(Vec::capacity).sum::<usize>()
            + self.entries.capacity() * 8
            + self.interned.capacity() * 13
    }

    /// Gives back the unused tail of each chunk and of the entry list.
    pub(crate) fn shrink(&mut self) {
        for chunk in &mut self.chunks {
            chunk.shrink_to_fit();
        }
        self.entries.shrink_to_fit();
        self.interned.shrink_to_fit();
    }
}

/// A fixed-key SipHash of interned text. It is compared in memory only, and the arena checks
/// the text behind a match.
pub(crate) fn hash(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}
