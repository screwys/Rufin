//! Each resident table is declared once. A Write reads rows by key, URI or object id with the
//! SELECT generated here, decodes them in column order, and persists only the columns a staged
//! row changed with the statements generated from the same declaration.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Map, Value, json};
use sqlx::Row as _;
use sqlx::sqlite::SqliteRow;

use super::records::*;
use crate::schema::{LINK_ALBUM_ARTIST, LINK_ARTIST, LINK_FOLDER, LINK_GENRE, LINK_MOOD};
use crate::tags::TagKind;
use crate::{
    AlbumKey, ArtistKey, ArtistMetadata, FolderKey, FolderMetadata, LibraryResult, LocalFileKey,
    SourceKey, TagMetadata, TrackKey,
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Conv {
    Value,
    /// Hex text in the staged row.
    Blob,
    /// JSON in the staged row, stored as JSONB.
    Jsonb,
}

pub(crate) struct Column {
    pub(crate) name: &'static str,
    pub(crate) conv: Conv,
    /// The value of a new row when the staged row leaves the column out.
    pub(crate) default: &'static str,
}

const fn value(name: &'static str) -> Column {
    Column {
        name,
        conv: Conv::Value,
        default: "NULL",
    }
}

const fn text(name: &'static str, default: &'static str) -> Column {
    Column {
        name,
        conv: Conv::Value,
        default,
    }
}

const fn blob(name: &'static str, default: &'static str) -> Column {
    Column {
        name,
        conv: Conv::Blob,
        default,
    }
}

const fn jsonb(name: &'static str, default: &'static str) -> Column {
    Column {
        name,
        conv: Conv::Jsonb,
        default,
    }
}

pub(crate) struct Table {
    pub(crate) name: &'static str,
    pub(crate) key: &'static str,
    /// The column a new row's RETURNING maps back to its staged row.
    pub(crate) natural: &'static str,
    /// The columns a read decodes, in order after the key. They are the row's own fields.
    pub(crate) read: usize,
    /// Every column a staged row may write: the read ones first, then extras that are read by
    /// key only.
    pub(crate) columns: &'static [Column],
    /// The table a read joins and the row's details are written to.
    pub(crate) details: Option<&'static Table>,
    /// The condition that finds a row by source and object id.
    pub(crate) object: &'static str,
}

/// An ordered list of link targets of one role, replaced as a whole when it changes.
pub(crate) struct Run {
    pub(crate) table: &'static str,
    pub(crate) owner: &'static str,
    pub(crate) role: i64,
}

pub(crate) static SOURCES: Table = Table {
    name: "sources",
    key: "source_key",
    natural: "object_id",
    read: 5,
    columns: &[
        value("object_id"),
        value("kind"),
        text("display_name", "''"),
        text("catalog_complete", "0"),
        blob("freshness", "NULL"),
    ],
    details: None,
    object: "false",
};

pub(crate) static TRACK_DETAILS: Table = Table {
    name: "track_details",
    key: "track_key",
    natural: "track_key",
    read: 5,
    columns: &[
        blob("native_artwork", "NULL"),
        value("source_path"),
        value("cue_path"),
        value("cue_start_millis"),
        value("cue_end_millis"),
        jsonb("metadata", "jsonb('{}')"),
        blob("source_loudness_analysis_key", "zeroblob(32)"),
        blob("loudness_analysis_key", "zeroblob(32)"),
    ],
    details: None,
    object: "false",
};

pub(crate) static TRACKS: Table = Table {
    name: "tracks",
    key: "track_key",
    natural: "media_uri",
    read: 27,
    columns: &[
        value("source_key"),
        value("object_id"),
        value("media_uri"),
        value("album_key"),
        value("local_file_key"),
        text("indexed", "0"),
        value("hidden_since"),
        text("native_metadata", "0"),
        text("title", "''"),
        text("display_artist", "''"),
        text("display_album", "''"),
        text("sort_text", "''"),
        text("duration_millis", "0"),
        text("disc_number", "0"),
        text("track_number", "0"),
        value("year"),
        value("release_date"),
        value("date_added"),
        value("first_seen_at"),
        value("bpm"),
        value("comment"),
        value("source_format"),
        text("flags", "0"),
        value("native_favorite"),
        value("native_rating"),
        value("native_play_count"),
        value("native_last_played"),
    ],
    details: Some(&TRACK_DETAILS),
    object: "source_key=?1 AND object_id=?2",
};

pub(crate) static ALBUMS: Table = Table {
    name: "albums",
    key: "album_key",
    natural: "media_uri",
    read: 22,
    columns: &[
        value("source_key"),
        value("object_id"),
        value("media_uri"),
        text("indexed", "0"),
        text("native_metadata", "0"),
        text("cached_local", "0"),
        text("title", "''"),
        text("display_artist", "''"),
        text("sort_text", "''"),
        value("year"),
        value("release_date"),
        value("date_added"),
        value("is_compilation"),
        value("first_seen_at"),
        value("release_identity"),
        value("release_lookup_identity"),
        value("native_favorite"),
        value("native_rating"),
        value("native_play_count"),
        value("native_last_played"),
        blob("native_artwork", "NULL"),
        // Read as whether a header exists; written as the header itself.
        jsonb("input_metadata", "NULL"),
        jsonb("metadata", "jsonb('{}')"),
        blob("source_loudness_analysis_key", "zeroblob(32)"),
        blob("loudness_analysis_key", "zeroblob(32)"),
        blob("accepted_audio", "NULL"),
    ],
    details: None,
    object: "source_key=?1 AND object_id=?2",
};

pub(crate) static ARTISTS: Table = Table {
    name: "artists",
    key: "artist_key",
    natural: "media_uri",
    read: 13,
    columns: &[
        value("source_key"),
        value("object_id"),
        value("media_uri"),
        text("indexed", "0"),
        text("native_metadata", "0"),
        text("name", "''"),
        value("sort_text"),
        value("musicbrainz_artist_id"),
        value("native_favorite"),
        value("native_rating"),
        value("native_play_count"),
        value("native_last_played"),
        blob("native_artwork", "NULL"),
    ],
    details: None,
    object: "source_key=?1 AND object_id=?2",
};

pub(crate) static TAGS: Table = Table {
    name: "tags",
    key: "tag_key",
    natural: "media_uri",
    read: 9,
    columns: &[
        value("source_key"),
        value("kind"),
        value("object_id"),
        value("media_uri"),
        text("indexed", "0"),
        text("native_metadata", "0"),
        text("name", "''"),
        value("sort_text"),
        blob("native_artwork", "NULL"),
    ],
    details: None,
    object: "source_key=?1 AND object_id=?2 AND kind=?3",
};

pub(crate) static FOLDERS: Table = Table {
    name: "folders",
    key: "folder_key",
    natural: "media_uri",
    read: 8,
    columns: &[
        value("source_key"),
        value("object_id"),
        value("media_uri"),
        text("indexed", "0"),
        text("native_metadata", "0"),
        value("name"),
        value("sort_text"),
        blob("native_artwork", "NULL"),
    ],
    details: None,
    object: "source_key=?1 AND object_id=?2",
};

pub(crate) static TRACK_RUNS: [Run; 5] = [
    Run {
        table: "track_links",
        owner: "track_key",
        role: LINK_ARTIST,
    },
    Run {
        table: "track_links",
        owner: "track_key",
        role: LINK_ALBUM_ARTIST,
    },
    Run {
        table: "track_links",
        owner: "track_key",
        role: LINK_GENRE,
    },
    Run {
        table: "track_links",
        owner: "track_key",
        role: LINK_MOOD,
    },
    Run {
        table: "track_links",
        owner: "track_key",
        role: LINK_FOLDER,
    },
];

pub(crate) static ALBUM_RUNS: [Run; 2] = [
    Run {
        table: "album_links",
        owner: "album_key",
        role: LINK_ARTIST,
    },
    Run {
        table: "album_links",
        owner: "album_key",
        role: LINK_GENRE,
    },
];

pub(crate) fn tag_kind(kind: i64) -> TagKind {
    if kind == 1 {
        TagKind::Mood
    } else {
        TagKind::Genre
    }
}

impl Table {
    /// Reads rows by a condition over the table's own columns.
    pub(crate) fn select(&self, condition: &str) -> String {
        let mut columns = vec![format!("{}.{}", self.name, self.key)];
        columns.extend(self.columns[..self.read].iter().map(|column| {
            if column.name == "input_metadata" {
                "input_metadata IS NOT NULL".to_owned()
            } else {
                format!("{}.{}", self.name, column.name)
            }
        }));
        let mut from = self.name.to_owned();
        if let Some(details) = self.details {
            columns.extend(
                details.columns[..details.read]
                    .iter()
                    .map(|column| format!("{}.{}", details.name, column.name)),
            );
            from = format!(
                "{} LEFT JOIN {} USING({})",
                self.name, details.name, self.key
            );
        }
        format!("SELECT {} FROM {from} WHERE {condition}", columns.join(","))
    }
}

fn staged(column: &Column, fallback: &str) -> String {
    let name = column.name;
    let set = match column.conv {
        Conv::Value => format!("j.value->>'$.{name}'"),
        Conv::Blob => format!("unhex(j.value->>'$.{name}')"),
        Conv::Jsonb => format!(
            "CASE json_type(j.value,'$.{name}') WHEN 'null' THEN NULL ELSE jsonb(j.value->'$.{name}') END"
        ),
    };
    format!("CASE WHEN json_type(j.value,'$.{name}') IS NULL THEN {fallback} ELSE {set} END")
}

/// The statements for one table, generated once.
pub(crate) struct TableStatements {
    pub(crate) insert: String,
    pub(crate) delete: String,
}

/// An UPDATE of the columns any staged row holds; a row without one keeps its value. SQLite
/// rewrites the index entries of every column the SET list names, even when the value is
/// unchanged, so the list names no other column.
pub(crate) fn update(table: &Table, columns: &[&Column]) -> String {
    format!(
        "UPDATE {} SET {} FROM json_each(?1) j WHERE {}.{}=j.value->>'$.{}'",
        table.name,
        columns
            .iter()
            .map(|column| format!(
                "{}={}",
                column.name,
                staged(column, &format!("{}.{}", table.name, column.name))
            ))
            .collect::<Vec<_>>()
            .join(","),
        table.name,
        table.key,
        table.key
    )
}

fn build_table_statements(table: &Table) -> TableStatements {
    let columns = table.columns.iter().map(|column| column.name);
    let values = table
        .columns
        .iter()
        .map(|column| staged(column, column.default));
    let insert = if table.details.is_some() || table.natural != table.key {
        format!(
            "INSERT INTO {}({}) SELECT {} FROM json_each(?1) j RETURNING {},{}",
            table.name,
            columns.collect::<Vec<_>>().join(","),
            values.collect::<Vec<_>>().join(","),
            table.key,
            table.natural
        )
    } else {
        // Details rows take their owner's key; an existing row keeps its columns until the
        // update that follows.
        format!(
            "INSERT OR IGNORE INTO {}({}) SELECT j.value->>'$.{}' FROM json_each(?1) j",
            table.name, table.key, table.key
        )
    };
    let delete = format!(
        "DELETE FROM {} WHERE {} IN (SELECT value FROM json_each(?1))",
        table.name, table.key
    );
    TableStatements { insert, delete }
}

static TABLE_STATEMENTS: LazyLock<HashMap<&'static str, TableStatements>> = LazyLock::new(|| {
    [
        &SOURCES,
        &TRACKS,
        &TRACK_DETAILS,
        &ALBUMS,
        &ARTISTS,
        &TAGS,
        &FOLDERS,
    ]
    .into_iter()
    .map(|table| (table.name, build_table_statements(table)))
    .collect()
});

pub(crate) fn statements(table: &Table) -> &'static TableStatements {
    &TABLE_STATEMENTS[table.name]
}

/// Deletes the runs of (owner, role) pairs, then inserts (owner, role, position, target) rows.
pub(crate) fn run_delete(table: &str, owner: &str) -> String {
    format!(
        "DELETE FROM {table} WHERE ({owner},role) IN (SELECT j.value->>0,j.value->>1 FROM json_each(?1) j)"
    )
}

pub(crate) fn run_insert(table: &str, owner: &str) -> String {
    format!(
        "INSERT INTO {table}({owner},role,position,target) SELECT j.value->>0,j.value->>1,j.value->>2,j.value->>3 FROM json_each(?1) j"
    )
}

pub(crate) fn hex(bytes: &[u8]) -> Value {
    Value::String(bytes.iter().map(|byte| format!("{byte:02X}")).collect())
}

pub(crate) fn hex_or_null(bytes: Option<&[u8]>) -> Value {
    bytes.map_or(Value::Null, hex)
}

/// A staged row: its table, key, identity, columns and link runs.
pub(crate) trait Row: Clone + PartialEq + Sized {
    const TABLE: &'static Table;
    fn key(&self) -> i64;
    fn set_key(&mut self, key: i64);
    fn natural(&self) -> &str;
    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)>;
    fn source(&self) -> Option<SourceKey>;
    fn columns(&self) -> Map<String, Value>;
    /// The columns of the details row, for rows that have one.
    fn details(&self) -> Option<Map<String, Value>> {
        None
    }
    fn runs(&self) -> Vec<(&'static Run, Vec<i64>)> {
        Vec::new()
    }
    /// Decodes the key, then the table's read columns, then the details columns.
    fn read(row: &SqliteRow) -> LibraryResult<Self>;
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(columns) => columns,
        _ => unreachable!("rows are JSON objects"),
    }
}

fn flag(row: &SqliteRow, index: usize) -> LibraryResult<bool> {
    Ok(row.try_get::<i64, _>(index)? != 0)
}

fn native_state(row: &SqliteRow, start: usize) -> LibraryResult<NativeState> {
    Ok(NativeState {
        favorite: row
            .try_get::<Option<i64>, _>(start)?
            .map(|value| value != 0),
        rating: row.try_get(start + 1)?,
        play_count: row.try_get(start + 2)?,
        last_played: row.try_get(start + 3)?,
    })
}

fn state_columns(columns: &mut Map<String, Value>, state: &NativeState) {
    columns.insert(
        "native_favorite".into(),
        state.favorite.map(i64::from).into(),
    );
    columns.insert("native_rating".into(), state.rating.into());
    columns.insert("native_play_count".into(), state.play_count.into());
    columns.insert("native_last_played".into(), state.last_played.into());
}

impl Row for Source {
    const TABLE: &'static Table = &SOURCES;

    fn key(&self) -> i64 {
        self.key.raw()
    }

    fn set_key(&mut self, key: i64) {
        self.key = SourceKey::from_raw(key);
    }

    fn natural(&self) -> &str {
        &self.object
    }

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        None
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.key)
    }

    fn columns(&self) -> Map<String, Value> {
        object(json!({
            "object_id": self.object,
            "kind": self.kind,
            "display_name": self.name,
            "catalog_complete": i64::from(self.complete),
            "freshness": hex_or_null(self.freshness.as_deref()),
        }))
    }

    fn read(row: &SqliteRow) -> LibraryResult<Self> {
        Ok(Self {
            key: SourceKey::from_raw(row.try_get(0)?),
            object: row.try_get(1)?,
            kind: row.try_get(2)?,
            name: row.try_get(3)?,
            complete: flag(row, 4)?,
            freshness: row.try_get(5)?,
        })
    }
}

impl Row for Track {
    const TABLE: &'static Table = &TRACKS;

    fn key(&self) -> i64 {
        self.key.raw()
    }

    fn set_key(&mut self, key: i64) {
        self.key = TrackKey::from_raw(key);
    }

    fn natural(&self) -> &str {
        &self.uri
    }

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source?, None, self.object.as_deref()?))
    }

    fn source(&self) -> Option<SourceKey> {
        self.source
    }

    fn columns(&self) -> Map<String, Value> {
        let facts = &self.facts;
        let mut columns = object(json!({
            "source_key": self.source,
            "object_id": self.object,
            "media_uri": self.uri,
            "album_key": self.album,
            "local_file_key": self.local_file,
            "indexed": i64::from(self.indexed),
            "hidden_since": self.hidden_since,
            "native_metadata": i64::from(self.native),
            "title": facts.title,
            "display_artist": facts.artist,
            "display_album": facts.album,
            "sort_text": facts.sort,
            "duration_millis": facts.duration_ms,
            "disc_number": facts.disc,
            "track_number": facts.number,
            "year": facts.year,
            "release_date": facts.release_date,
            "date_added": facts.date_added,
            "first_seen_at": self.first_seen,
            "bpm": facts.bpm,
            "comment": facts.comment,
            "source_format": facts.format,
            "flags": self.flags(),
        }));
        state_columns(&mut columns, &self.state);
        columns
    }

    fn details(&self) -> Option<Map<String, Value>> {
        Some(object(json!({
            "native_artwork": hex_or_null(self.binding.as_deref()),
            "source_path": self.source_path,
            "cue_path": self.cue_path,
            "cue_start_millis": self.cue_start,
            "cue_end_millis": self.cue_end,
        })))
    }

    fn runs(&self) -> Vec<(&'static Run, Vec<i64>)> {
        let tags = |kind| {
            self.tags
                .iter()
                .filter(|(tag, _)| *tag == kind)
                .map(|(_, key)| *key)
                .collect()
        };
        vec![
            (
                &TRACK_RUNS[0],
                self.artists.iter().map(|key| key.raw()).collect(),
            ),
            (
                &TRACK_RUNS[1],
                self.album_artists.iter().map(|key| key.raw()).collect(),
            ),
            (&TRACK_RUNS[2], tags(TagKind::Genre)),
            (&TRACK_RUNS[3], tags(TagKind::Mood)),
            (
                &TRACK_RUNS[4],
                self.folders.iter().map(|key| key.raw()).collect(),
            ),
        ]
    }

    fn read(row: &SqliteRow) -> LibraryResult<Self> {
        Ok(Self {
            key: TrackKey::from_raw(row.try_get(0)?),
            source: row.try_get::<Option<i64>, _>(1)?.map(SourceKey::from_raw),
            object: row.try_get(2)?,
            uri: row.try_get(3)?,
            album: row.try_get::<Option<i64>, _>(4)?.map(AlbumKey::from_raw),
            local_file: row
                .try_get::<Option<i64>, _>(5)?
                .map(LocalFileKey::from_raw),
            indexed: flag(row, 6)?,
            hidden_since: row.try_get(7)?,
            native: flag(row, 8)?,
            facts: TrackFacts {
                title: row.try_get(9)?,
                artist: row.try_get(10)?,
                album: row.try_get(11)?,
                sort: row.try_get(12)?,
                duration_ms: row.try_get(13)?,
                disc: row.try_get(14)?,
                number: row.try_get(15)?,
                year: row.try_get(16)?,
                release_date: row.try_get(17)?,
                date_added: row.try_get(18)?,
                bpm: row.try_get(20)?,
                comment: row.try_get(21)?,
                format: row.try_get(22)?,
            },
            first_seen: row.try_get(19)?,
            // 23 is flags, derived from the details columns.
            state: native_state(row, 24)?,
            binding: row.try_get(28)?,
            source_path: row.try_get(29)?,
            cue_path: row.try_get(30)?,
            cue_start: row.try_get(31)?,
            cue_end: row.try_get(32)?,
            artists: Vec::new(),
            album_artists: Vec::new(),
            tags: Vec::new(),
            folders: Vec::new(),
        })
    }
}

impl Row for Album {
    const TABLE: &'static Table = &ALBUMS;

    fn key(&self) -> i64 {
        self.key.raw()
    }

    fn set_key(&mut self, key: i64) {
        self.key = AlbumKey::from_raw(key);
    }

    fn natural(&self) -> &str {
        &self.uri
    }

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, None, &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn columns(&self) -> Map<String, Value> {
        let facts = &self.facts;
        let mut columns = object(json!({
            "source_key": self.source,
            "object_id": self.object,
            "media_uri": self.uri,
            "indexed": i64::from(self.indexed),
            "native_metadata": i64::from(self.native),
            "cached_local": i64::from(self.cached_local),
            "title": facts.title,
            "display_artist": facts.artist,
            "sort_text": facts.sort,
            "year": facts.year,
            "release_date": facts.release_date,
            "date_added": facts.date_added,
            "is_compilation": facts.compilation.map(i64::from),
            "first_seen_at": self.first_seen,
            "release_identity": self.release_identity,
            "release_lookup_identity": self.release_lookup_identity,
            "native_artwork": hex_or_null(self.binding.as_deref()),
        }));
        state_columns(&mut columns, &self.state);
        columns
    }

    fn runs(&self) -> Vec<(&'static Run, Vec<i64>)> {
        vec![
            (
                &ALBUM_RUNS[0],
                self.artists.iter().map(|key| key.raw()).collect(),
            ),
            (&ALBUM_RUNS[1], self.genres.clone()),
        ]
    }

    fn read(row: &SqliteRow) -> LibraryResult<Self> {
        Ok(Self {
            key: AlbumKey::from_raw(row.try_get(0)?),
            source: SourceKey::from_raw(row.try_get(1)?),
            object: row.try_get(2)?,
            uri: row.try_get(3)?,
            indexed: flag(row, 4)?,
            native: flag(row, 5)?,
            cached_local: flag(row, 6)?,
            facts: AlbumFacts {
                title: row.try_get(7)?,
                artist: row.try_get(8)?,
                sort: row.try_get(9)?,
                year: row.try_get(10)?,
                release_date: row.try_get(11)?,
                date_added: row.try_get(12)?,
                compilation: row.try_get::<Option<i64>, _>(13)?.map(|value| value != 0),
            },
            first_seen: row.try_get(14)?,
            release_identity: row.try_get(15)?,
            release_lookup_identity: row.try_get(16)?,
            state: native_state(row, 17)?,
            binding: row.try_get(21)?,
            input: flag(row, 22)?,
            artists: Vec::new(),
            genres: Vec::new(),
        })
    }
}

impl Row for Artist {
    const TABLE: &'static Table = &ARTISTS;

    fn key(&self) -> i64 {
        self.key.raw()
    }

    fn set_key(&mut self, key: i64) {
        self.key = ArtistKey::from_raw(key);
    }

    fn natural(&self) -> &str {
        &self.uri
    }

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, None, &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn columns(&self) -> Map<String, Value> {
        let mut columns = object(json!({
            "source_key": self.source,
            "object_id": self.object,
            "media_uri": self.uri,
            "indexed": i64::from(self.indexed),
            "native_metadata": i64::from(self.native),
            "name": self.metadata.name,
            "sort_text": self.metadata.sort_text,
            "musicbrainz_artist_id": self.metadata.musicbrainz_artist_id,
            "native_artwork": hex_or_null(self.binding.as_deref()),
        }));
        state_columns(&mut columns, &self.state);
        columns
    }

    fn read(row: &SqliteRow) -> LibraryResult<Self> {
        Ok(Self {
            key: ArtistKey::from_raw(row.try_get(0)?),
            source: SourceKey::from_raw(row.try_get(1)?),
            object: row.try_get(2)?,
            uri: row.try_get(3)?,
            indexed: flag(row, 4)?,
            native: flag(row, 5)?,
            metadata: ArtistMetadata {
                name: row.try_get(6)?,
                sort_text: row.try_get(7)?,
                musicbrainz_artist_id: row.try_get(8)?,
            },
            state: native_state(row, 9)?,
            binding: row.try_get(13)?,
        })
    }
}

impl Row for Tag {
    const TABLE: &'static Table = &TAGS;

    fn key(&self) -> i64 {
        self.key
    }

    fn set_key(&mut self, key: i64) {
        self.key = key;
    }

    fn natural(&self) -> &str {
        &self.uri
    }

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, Some(self.kind), &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn columns(&self) -> Map<String, Value> {
        object(json!({
            "source_key": self.source,
            "kind": self.kind as i64,
            "object_id": self.object,
            "media_uri": self.uri,
            "indexed": i64::from(self.indexed),
            "native_metadata": i64::from(self.native),
            "name": self.metadata.name,
            "sort_text": self.metadata.sort_text,
            "native_artwork": hex_or_null(self.binding.as_deref()),
        }))
    }

    fn read(row: &SqliteRow) -> LibraryResult<Self> {
        Ok(Self {
            key: row.try_get(0)?,
            source: SourceKey::from_raw(row.try_get(1)?),
            kind: tag_kind(row.try_get(2)?),
            object: row.try_get(3)?,
            uri: row.try_get(4)?,
            indexed: flag(row, 5)?,
            native: flag(row, 6)?,
            metadata: TagMetadata {
                name: row.try_get(7)?,
                sort_text: row.try_get(8)?,
            },
            binding: row.try_get(9)?,
        })
    }
}

impl Row for Folder {
    const TABLE: &'static Table = &FOLDERS;

    fn key(&self) -> i64 {
        self.key.raw()
    }

    fn set_key(&mut self, key: i64) {
        self.key = FolderKey::from_raw(key);
    }

    fn natural(&self) -> &str {
        &self.uri
    }

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, None, &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn columns(&self) -> Map<String, Value> {
        object(json!({
            "source_key": self.source,
            "object_id": self.object,
            "media_uri": self.uri,
            "indexed": i64::from(self.indexed),
            "native_metadata": i64::from(self.native),
            "name": self.metadata.name,
            "sort_text": self.metadata.sort_text,
            "native_artwork": hex_or_null(self.binding.as_deref()),
        }))
    }

    fn read(row: &SqliteRow) -> LibraryResult<Self> {
        Ok(Self {
            key: FolderKey::from_raw(row.try_get(0)?),
            source: SourceKey::from_raw(row.try_get(1)?),
            object: row.try_get(2)?,
            uri: row.try_get(3)?,
            indexed: flag(row, 4)?,
            native: flag(row, 5)?,
            metadata: FolderMetadata {
                name: row.try_get(6)?,
                sort_text: row.try_get(7)?,
            },
            binding: row.try_get(8)?,
        })
    }
}
