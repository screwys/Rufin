//! Each resident table is declared once: its columns, which of them a read takes from the
//! Catalog, and the bound statements a Write reads and persists rows with. A Write reads by key
//! only the columns the Catalog does not hold, compares typed cells, and writes the cells a
//! staged row changed, one statement per table, page and set of changed columns.

use serde_json::Value;
use sqlx::Row as _;
use sqlx::sqlite::{SqliteArguments, SqliteRow};

use super::merge::Touched;
use super::records::*;
use super::snapshot::{
    CACHED_LOCAL, INDEXED, INPUT, NATIVE, Packed, SORT_LOWER, SORT_TITLE, Snapshot, UserItem,
};
use super::strings::StrId;
use super::write::{Family, Overlay, Write};
use crate::schema::{LINK_ALBUM_ARTIST, LINK_ARTIST, LINK_FOLDER, LINK_GENRE, LINK_MOOD};
use crate::tags::TagKind;
use crate::{
    AlbumKey, AlbumMetadata, ArtistKey, ArtistMetadata, FolderKey, FolderMetadata, LibraryError,
    LibraryResult, LocalFileKey, SourceKey, TagMetadata, TrackKey, TrackMetadata,
};

/// One column value, bound to a statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Cell {
    Null,
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    /// The record has it without the SELECT: from the Catalog, or derived from other columns.
    Held,
    /// Read by key with the row.
    Read,
    /// Written when a row sets it, never read back.
    Written,
    /// Written as JSON text and stored as JSONB.
    Json,
}

pub(crate) struct Column {
    pub(crate) name: &'static str,
    pub(crate) kind: Kind,
}

/// Columns by kind and name, as `Held source_key, Read object_id`.
macro_rules! columns {
    ($($kind:ident $name:ident),* $(,)?) => {
        &[$(Column { name: stringify!($name), kind: Kind::$kind }),*]
    };
}

/// An ordered list of link targets of one role, replaced as a whole when it changes.
pub(crate) struct Run {
    pub(crate) table: &'static str,
    pub(crate) owner: &'static str,
    pub(crate) role: i64,
}

const fn run(table: &'static str, owner: &'static str, role: i64) -> Run {
    Run { table, owner, role }
}

pub(crate) struct Table {
    pub(crate) name: &'static str,
    pub(crate) key: &'static str,
    /// The column a new row's RETURNING maps back to its staged row.
    pub(crate) natural: &'static str,
    /// Every column a row has, in cell order.
    pub(crate) columns: &'static [Column],
    /// The table a read joins; its columns follow the row's own in cell order.
    pub(crate) details: Option<&'static Table>,
    /// Whether an object id is unique only within a kind.
    pub(crate) tagged: bool,
    pub(crate) runs: &'static [Run],
}

pub(crate) static SOURCES: Table = Table {
    name: "sources",
    key: "source_key",
    natural: "object_id",
    columns: columns![
        Held object_id, Held kind, Held display_name, Held catalog_complete, Held freshness,
    ],
    details: None,
    tagged: false,
    runs: &[],
};

pub(crate) static TRACK_DETAILS: Table = Table {
    name: "track_details",
    key: "track_key",
    natural: "track_key",
    columns: columns![
        Read native_artwork, Read source_path, Read cue_path, Read cue_start_millis,
        Read cue_end_millis, Json metadata, Written source_loudness_analysis_key,
        Written loudness_analysis_key,
    ],
    details: None,
    tagged: false,
    runs: &[],
};

pub(crate) static TRACKS: Table = Table {
    name: "tracks",
    key: "track_key",
    natural: "media_uri",
    columns: columns![
        Held source_key, Read object_id, Read media_uri, Held album_key, Read local_file_key,
        Held indexed, Read hidden_since, Held native_metadata, Held title, Held display_artist,
        Held display_album, Held sort_text, Read duration_millis, Read disc_number,
        Read track_number, Read year, Read release_date, Read date_added, Held released,
        Held added_at, Read first_seen_at, Read bpm, Held comment, Held source_format, Held flags, Read native_favorite,
        Read native_rating, Read native_play_count, Read native_last_played,
    ],
    details: Some(&TRACK_DETAILS),
    tagged: false,
    runs: &[
        run("track_links", "track_key", LINK_ARTIST),
        run("track_links", "track_key", LINK_ALBUM_ARTIST),
        run("track_links", "track_key", LINK_GENRE),
        run("track_links", "track_key", LINK_MOOD),
        run("track_links", "track_key", LINK_FOLDER),
    ],
};

pub(crate) static ALBUMS: Table = Table {
    name: "albums",
    key: "album_key",
    natural: "media_uri",
    columns: columns![
        Held source_key, Read object_id, Read media_uri, Held indexed, Held native_metadata,
        Held cached_local, Held title, Held display_artist, Held sort_text, Read year,
        Read release_date, Read date_added, Held released, Held added_at, Read is_compilation,
        Read first_seen_at, Read release_identity, Read release_lookup_identity,
        Read native_favorite, Read native_rating, Read native_play_count,
        Read native_last_played, Read native_artwork, Held flags, Json input_metadata, Json metadata, Written accepted_audio,
    ],
    details: None,
    tagged: false,
    runs: &[
        run("album_links", "album_key", LINK_ARTIST),
        run("album_links", "album_key", LINK_GENRE),
    ],
};

pub(crate) static ARTISTS: Table = Table {
    name: "artists",
    key: "artist_key",
    natural: "media_uri",
    columns: columns![
        Held source_key, Read object_id, Read media_uri, Held indexed, Held native_metadata,
        Held name, Held sort_text, Read musicbrainz_artist_id, Read native_favorite,
        Read native_rating, Read native_play_count, Read native_last_played,
        Read native_artwork, Held flags,
    ],
    details: None,
    tagged: false,
    runs: &[],
};

pub(crate) static TAGS: Table = Table {
    name: "tags",
    key: "tag_key",
    natural: "media_uri",
    columns: columns![
        Held source_key, Held kind, Read object_id, Read media_uri, Held indexed,
        Held native_metadata, Held name, Held sort_text, Read native_artwork, Held flags,
    ],
    details: None,
    tagged: true,
    runs: &[],
};

pub(crate) static FOLDERS: Table = Table {
    name: "folders",
    key: "folder_key",
    natural: "media_uri",
    columns: columns![
        Held source_key, Read object_id, Read media_uri, Held indexed, Held native_metadata,
        Held name, Held sort_text, Read native_artwork, Held flags,
    ],
    details: None,
    tagged: false,
    runs: &[],
};

pub(crate) fn tag_kind(kind: i64) -> TagKind {
    if kind == 1 {
        TagKind::Mood
    } else {
        TagKind::Genre
    }
}

/// Numbered placeholders from `first`, as `?2,?3`.
pub(crate) fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// `rows` rows of VALUES: a leading key when `key` is set, then the columns.
fn values(rows: usize, key: bool, columns: &[&Column]) -> String {
    let row = key
        .then_some("?")
        .into_iter()
        .chain(columns.iter().map(|column| match column.kind {
            Kind::Json => "jsonb(?)",
            _ => "?",
        }))
        .collect::<Vec<_>>()
        .join(",");
    vec![format!("({row})"); rows].join(",")
}

impl Table {
    /// Reads the key and the Read columns, joined with the details' Read columns.
    pub(crate) fn select(&self, condition: &str) -> String {
        let mut columns = vec![format!("{}.{}", self.name, self.key)];
        let mut from = self.name.to_owned();
        for table in std::iter::once(self).chain(self.details) {
            columns.extend(
                table
                    .columns
                    .iter()
                    .filter(|column| column.kind == Kind::Read)
                    .map(|column| format!("{}.{}", table.name, column.name)),
            );
        }
        if let Some(details) = self.details {
            from = format!(
                "{} LEFT JOIN {} USING({})",
                self.name, details.name, self.key
            );
        }
        format!("SELECT {} FROM {from} WHERE {condition}", columns.join(","))
    }

    /// The condition that finds rows of a source by object id, with `count` ids from ?2.
    pub(crate) fn objects(&self, count: usize) -> String {
        let kind = if self.tagged {
            format!(" AND {}.kind=?{}", self.name, count + 2)
        } else {
            String::new()
        };
        format!(
            "{0}.source_key=?1 AND {0}.object_id IN({1}){kind}",
            self.name,
            placeholders(2, count)
        )
    }

    /// New rows with these columns; RETURNING maps each to its key.
    pub(crate) fn insert(&self, columns: &[&Column], rows: usize) -> String {
        format!(
            "INSERT INTO {}({}) VALUES {} RETURNING {},{}",
            self.name,
            names(columns),
            values(rows, false, columns),
            self.key,
            self.natural
        )
    }

    /// Sets these columns of rows by key. SQLite rewrites the index entries of every column the
    /// SET list names, even when the value is unchanged, so the list names no other column.
    pub(crate) fn update(&self, columns: &[&Column], rows: usize) -> String {
        format!(
            "UPDATE {0} SET {1} FROM (VALUES {2}) staged WHERE {0}.{3}=staged.column1",
            self.name,
            columns
                .iter()
                .enumerate()
                .map(|(index, column)| format!("{}=staged.column{}", column.name, index + 2))
                .collect::<Vec<_>>()
                .join(","),
            values(rows, true, columns),
            self.key
        )
    }

    /// Details rows take their owner's key; a missing one is created with the defaults.
    pub(crate) fn upsert(&self, columns: &[&Column], rows: usize) -> String {
        format!(
            "INSERT INTO {0}({1},{2}) VALUES {3} ON CONFLICT({1}) DO UPDATE SET {4}",
            self.name,
            self.key,
            names(columns),
            values(rows, true, columns),
            columns
                .iter()
                .map(|column| format!("{0}=excluded.{0}", column.name))
                .collect::<Vec<_>>()
                .join(",")
        )
    }

    pub(crate) fn delete(&self, rows: usize) -> String {
        format!(
            "DELETE FROM {} WHERE {} IN({})",
            self.name,
            self.key,
            placeholders(1, rows)
        )
    }
}

fn names(columns: &[&Column]) -> String {
    columns
        .iter()
        .map(|column| column.name)
        .collect::<Vec<_>>()
        .join(",")
}

impl Run {
    /// Deletes the runs of `rows` (owner, role) pairs.
    pub(crate) fn delete(&self, rows: usize) -> String {
        format!(
            "DELETE FROM {} WHERE ({},role) IN(VALUES {})",
            self.table,
            self.owner,
            vec!["(?,?)"; rows].join(",")
        )
    }

    /// Inserts `rows` (owner, role, position, target) rows.
    pub(crate) fn insert(&self, rows: usize) -> String {
        format!(
            "INSERT INTO {}({},role,position,target) VALUES {}",
            self.table,
            self.owner,
            vec!["(?,?,?,?)"; rows].join(",")
        )
    }
}

pub(crate) type Query<'q> = sqlx::query::Query<'q, sqlx::Sqlite, SqliteArguments>;

pub(crate) fn bind<'q>(
    mut query: Query<'q>,
    cells: impl IntoIterator<Item = &'q Cell>,
) -> Query<'q> {
    for cell in cells {
        query = match cell {
            Cell::Null => query.bind(None::<i64>),
            Cell::Int(value) => query.bind(*value),
            Cell::Text(value) => query.bind(value.as_str()),
            Cell::Blob(value) => query.bind(value.as_slice()),
        };
    }
    query
}

fn int(value: Option<i64>) -> Option<Cell> {
    Some(value.map_or(Cell::Null, Cell::Int))
}

fn num(value: i64) -> Option<Cell> {
    Some(Cell::Int(value))
}

fn flag(value: bool) -> Option<Cell> {
    num(i64::from(value))
}

fn text(value: Option<&str>) -> Option<Cell> {
    Some(value.map_or(Cell::Null, |value| Cell::Text(value.to_owned())))
}

fn string(value: &str) -> Option<Cell> {
    Some(Cell::Text(value.to_owned()))
}

fn blob(value: Option<&[u8]>) -> Option<Cell> {
    Some(value.map_or(Cell::Null, |value| Cell::Blob(value.to_vec())))
}

/// A JSON cell a row writes when it sets one.
fn json_cell(value: Option<Option<&Value>>) -> Option<Cell> {
    value.map(|value| value.map_or(Cell::Null, |value| Cell::Text(value.to_string())))
}

fn state_cells(state: &NativeState) -> [Option<Cell>; 4] {
    [
        int(state.favorite.map(i64::from)),
        int(state.rating),
        int(state.play_count),
        int(state.last_played),
    ]
}

fn native_state(row: &SqliteRow) -> LibraryResult<NativeState> {
    Ok(NativeState {
        favorite: row
            .try_get::<Option<i64>, _>("native_favorite")?
            .map(|value| value != 0),
        rating: row.try_get("native_rating")?,
        play_count: row.try_get("native_play_count")?,
        last_played: row.try_get("native_last_played")?,
    })
}

fn missing(table: &str, key: i64) -> LibraryError {
    LibraryError::InvalidStore(format!("{table} row {key} is not in the Catalog"))
}

impl Snapshot {
    fn held_text(&self, id: StrId) -> String {
        self.text(id).to_owned()
    }

    fn held_optional(&self, id: StrId) -> Option<String> {
        (id != StrId::NONE).then(|| self.held_text(id))
    }
}

/// A resident family a Write reads, stages and persists.
pub(crate) trait Row: Clone + PartialEq + Sized {
    const TABLE: &'static Table;
    const FAMILY: Family;
    fn key(&self) -> i64;
    fn set_key(&mut self, key: i64);
    fn natural(&self) -> &str;
    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)>;
    fn source(&self) -> Option<SourceKey>;
    /// The cells of the row's columns, then its details columns. A Written column the row does
    /// not set is None.
    fn cells(&self) -> Vec<Option<Cell>>;
    /// The link targets of each of the table's runs.
    fn runs(&self) -> Vec<Vec<i64>> {
        Vec::new()
    }
    /// Decodes a row the table's SELECT read; the Held columns and the links come from the
    /// resident record.
    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self>;
    fn overlay<'b>(write: &'b Write<'_>) -> &'b Overlay<Self>;
    fn overlay_mut<'b>(write: &'b mut Write<'_>) -> &'b mut Overlay<Self>;
    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>);
    /// The source of a resident record, for a removal of a row the Write never read.
    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey>;
    /// Columns the Write keeps consistent: when a track stops being listed, and whether an
    /// album has a header.
    fn normalize(&mut self, _before: Option<&Self>) {}
    /// The favorite, rating and plays the row's source reports, for rows that carry them.
    fn state(&mut self) -> Option<&mut NativeState> {
        None
    }
    /// The row as the user favorites and rates it, with its URI.
    fn user_item(&self) -> Option<(UserItem, &str)> {
        None
    }
}

/// The source-owned families: what completion, removal and the header merge read and change.
pub(crate) trait Owned: Row {
    type Metadata: serde::Serialize + serde::de::DeserializeOwned + Default + Clone + PartialEq;
    /// Whether the stored picture is rebuilt from the header, as artist pictures are.
    const BUILT_PICTURE: bool = false;
    fn indexed(&self) -> bool;
    fn set_indexed(&mut self, indexed: bool);
    fn native(&self) -> bool;
    fn set_native(&mut self, native: bool);
    fn binding(&self) -> Option<&[u8]>;
    fn set_binding(&mut self, binding: Option<Vec<u8>>);
    fn create(source: SourceKey, tag: Option<TagKind>, object: &str, uri: String) -> Self;
    /// The stored header, when the row holds it; album headers are read by key.
    fn header(&self) -> Option<Value>;
    fn set_header(&mut self, metadata: &Self::Metadata, json: Value);
    /// Folder observations without a name keep the stored header.
    fn keeps_header(_metadata: &Self::Metadata) -> bool {
        false
    }
    /// The picture to store from an observed one. Artist pictures carry the artist itself.
    fn picture(&self, observed: Option<&[u8]>, _native: bool) -> LibraryResult<Option<Vec<u8>>> {
        Ok(observed.map(<[u8]>::to_vec))
    }
    /// Notes a staged change for album resolution.
    fn touch(&self, _touched: &mut Touched) {}
}

/// What every family declares alike: its table, its overlay in a Write, its key and natural key.
macro_rules! family {
    ($table:ident, $family:ident, $overlay:ident, $natural:ident, $raw:expr, $from:expr) => {
        const TABLE: &'static Table = &$table;
        const FAMILY: Family = Family::$family;

        fn overlay<'b>(write: &'b Write<'_>) -> &'b Overlay<Self> {
            &write.$overlay
        }

        fn overlay_mut<'b>(write: &'b mut Write<'_>) -> &'b mut Overlay<Self> {
            &mut write.$overlay
        }

        fn key(&self) -> i64 {
            $raw(self.key)
        }

        fn set_key(&mut self, key: i64) {
            self.key = $from(key);
        }

        fn natural(&self) -> &str {
            &self.$natural
        }
    };
}

macro_rules! owned {
    () => {
        fn indexed(&self) -> bool {
            self.indexed
        }

        fn set_indexed(&mut self, indexed: bool) {
            self.indexed = indexed;
        }

        fn native(&self) -> bool {
            self.native
        }

        fn set_native(&mut self, native: bool) {
            self.native = native;
        }

        fn binding(&self) -> Option<&[u8]> {
            self.binding.as_deref()
        }

        fn set_binding(&mut self, binding: Option<Vec<u8>>) {
            self.binding = binding;
        }
    };
}

fn source_of<R: Packed>(
    snapshot: &Snapshot,
    family: &super::snapshot::Family<R>,
    key: i64,
) -> Option<SourceKey> {
    family
        .get(key)
        .and_then(|record| snapshot.source_key(record.source()))
}

impl Row for Source {
    family!(
        SOURCES,
        Source,
        sources,
        object,
        SourceKey::raw,
        SourceKey::from_raw
    );

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        None
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.key)
    }

    fn cells(&self) -> Vec<Option<Cell>> {
        vec![
            string(&self.object),
            text(self.kind.as_deref()),
            string(&self.name),
            flag(self.complete),
            blob(self.freshness.as_deref()),
        ]
    }

    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self> {
        let key = SourceKey::from_raw(row.try_get(0)?);
        snapshot
            .source(key)
            .cloned()
            .ok_or_else(|| missing("sources", key.raw()))
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_source(record.clone()),
            None => snapshot.remove_source(SourceKey::from_raw(key)),
        }
    }

    fn resident_source(_snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        Some(SourceKey::from_raw(key))
    }
}

impl Row for Track {
    family!(
        TRACKS,
        Track,
        tracks,
        uri,
        TrackKey::raw,
        TrackKey::from_raw
    );

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source?, None, self.object.as_deref()?))
    }

    fn source(&self) -> Option<SourceKey> {
        self.source
    }

    fn cells(&self) -> Vec<Option<Cell>> {
        let facts = &self.facts;
        let mut cells = vec![
            int(self.source.map(SourceKey::raw)),
            text(self.object.as_deref()),
            string(&self.uri),
            int(self.album.map(AlbumKey::raw)),
            int(self.local_file.map(LocalFileKey::raw)),
            flag(self.indexed),
            int(self.hidden_since),
            flag(self.native),
            string(&facts.title),
            string(&facts.artist),
            string(&facts.album),
            string(&facts.sort),
            num(facts.duration_ms),
            num(facts.disc),
            num(facts.number),
            int(facts.year),
            text(facts.release_date.as_deref()),
            text(facts.date_added.as_deref()),
            num(i64::from(release(facts.release_date.as_deref()))),
            int(added(facts.date_added.as_deref(), self.first_seen)),
            int(self.first_seen),
            int(facts.bpm),
            text(facts.comment.as_deref()),
            text(facts.format.as_deref()),
            num(self.flags()),
        ];
        cells.extend(state_cells(&self.state));
        cells.extend([
            blob(self.binding.as_deref()),
            text(self.source_path.as_deref()),
            text(self.cue_path.as_deref()),
            int(self.cue_start),
            int(self.cue_end),
            json_cell(self.writes.metadata.as_ref().map(Some)),
            self.writes
                .source_loudness
                .as_ref()
                .map(|key| Cell::Blob(key.clone())),
            self.writes
                .loudness
                .as_ref()
                .map(|key| Cell::Blob(key.clone())),
        ]);
        cells
    }

    fn runs(&self) -> Vec<Vec<i64>> {
        let tags = |kind| {
            self.tags
                .iter()
                .filter(|(tag, _)| *tag == kind)
                .map(|(_, key)| *key)
                .collect()
        };
        vec![
            self.artists.iter().map(|key| key.raw()).collect(),
            self.album_artists.iter().map(|key| key.raw()).collect(),
            tags(TagKind::Genre),
            tags(TagKind::Mood),
            self.folders.iter().map(|key| key.raw()).collect(),
        ]
    }

    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self> {
        let key: i64 = row.try_get(0)?;
        let record = snapshot
            .tracks
            .get(key)
            .ok_or_else(|| missing("tracks", key))?;
        let title = snapshot.held_text(record.title);
        let sort = if record.flags & SORT_TITLE != 0 {
            title.clone()
        } else if record.flags & SORT_LOWER != 0 {
            title.to_lowercase()
        } else {
            snapshot.held_text(record.sort)
        };
        let links = snapshot.track_links(record);
        Ok(Self {
            key: TrackKey::from_raw(key),
            source: snapshot.source_key(record.source),
            object: row.try_get("object_id")?,
            uri: row.try_get("media_uri")?,
            album: snapshot.albums.key_of(record.album).map(AlbumKey::from_raw),
            local_file: row
                .try_get::<Option<i64>, _>("local_file_key")?
                .map(LocalFileKey::from_raw),
            indexed: record.flags & INDEXED != 0,
            hidden_since: row.try_get("hidden_since")?,
            native: record.flags & NATIVE != 0,
            facts: TrackFacts {
                title,
                artist: snapshot.held_text(record.artist),
                album: snapshot.held_text(record.album_title),
                sort,
                duration_ms: row.try_get("duration_millis")?,
                disc: row.try_get("disc_number")?,
                number: row.try_get("track_number")?,
                year: row.try_get("year")?,
                release_date: row.try_get("release_date")?,
                date_added: row.try_get("date_added")?,
                bpm: row.try_get("bpm")?,
                comment: snapshot.held_optional(record.comment),
                format: snapshot.held_optional(record.format),
            },
            first_seen: row.try_get("first_seen_at")?,
            state: native_state(row)?,
            binding: row.try_get("native_artwork")?,
            source_path: row.try_get("source_path")?,
            cue_path: row.try_get("cue_path")?,
            cue_start: row.try_get("cue_start_millis")?,
            cue_end: row.try_get("cue_end_millis")?,
            artists: links.artists,
            album_artists: links.album_artists,
            tags: links.tags,
            folders: links.folders,
            writes: TrackWrites::default(),
        })
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_track(record),
            None => snapshot.remove_track(TrackKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        source_of(snapshot, &snapshot.tracks, key)
    }

    /// A track is hidden from when it stops being listed, or from its insertion when nothing
    /// lists it, so the expiry can forget it.
    fn normalize(&mut self, before: Option<&Self>) {
        self.hidden_since = if self.indexed {
            None
        } else {
            match before {
                Some(before) if !before.indexed => before.hidden_since,
                Some(_) => None,
                None => self.hidden_since,
            }
            .or_else(|| Some(chrono::Utc::now().timestamp()))
        };
    }

    fn state(&mut self) -> Option<&mut NativeState> {
        Some(&mut self.state)
    }

    fn user_item(&self) -> Option<(UserItem, &str)> {
        Some((UserItem::Track(self.key.raw()), &self.uri))
    }
}

impl Owned for Track {
    type Metadata = TrackMetadata;
    owned!();

    fn create(source: SourceKey, _tag: Option<TagKind>, object: &str, uri: String) -> Self {
        Track::new(Some(source), Some(object.to_owned()), uri)
    }

    fn header(&self) -> Option<Value> {
        None
    }

    fn set_header(&mut self, metadata: &TrackMetadata, json: Value) {
        self.facts = TrackFacts::of(metadata);
        self.writes.metadata = Some(json);
    }
}

impl Row for Album {
    family!(
        ALBUMS,
        Album,
        albums,
        uri,
        AlbumKey::raw,
        AlbumKey::from_raw
    );

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, None, &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn cells(&self) -> Vec<Option<Cell>> {
        let facts = &self.facts;
        let mut cells = vec![
            num(self.source.raw()),
            string(&self.object),
            string(&self.uri),
            flag(self.indexed),
            flag(self.native),
            flag(self.cached_local),
            string(&facts.title),
            string(&facts.artist),
            string(&facts.sort),
            int(facts.year),
            text(facts.release_date.as_deref()),
            text(facts.date_added.as_deref()),
            num(i64::from(release(facts.release_date.as_deref()))),
            int(added(facts.date_added.as_deref(), self.first_seen)),
            int(facts.compilation.map(i64::from)),
            int(self.first_seen),
            text(self.release_identity.as_deref()),
            text(self.release_lookup_identity.as_deref()),
        ];
        cells.extend(state_cells(&self.state));
        cells.extend([
            blob(self.binding.as_deref()),
            num(picture_flag(self.binding.as_deref())),
            json_cell(self.writes.header.as_ref().map(Option::as_ref)),
            json_cell(self.writes.metadata.as_ref().map(Some)),
            self.writes.clear_accepted_audio.then_some(Cell::Null),
        ]);
        cells
    }

    fn runs(&self) -> Vec<Vec<i64>> {
        vec![
            self.artists.iter().map(|key| key.raw()).collect(),
            self.genres.clone(),
        ]
    }

    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self> {
        let key: i64 = row.try_get(0)?;
        let record = snapshot
            .albums
            .get(key)
            .ok_or_else(|| missing("albums", key))?;
        let (artists, genres) = snapshot.album_links(record);
        Ok(Self {
            key: AlbumKey::from_raw(key),
            source: snapshot
                .source_key(record.source)
                .ok_or_else(|| missing("albums", key))?,
            object: row.try_get("object_id")?,
            uri: row.try_get("media_uri")?,
            indexed: record.flags & INDEXED != 0,
            native: record.flags & NATIVE != 0,
            cached_local: record.flags & CACHED_LOCAL != 0,
            facts: AlbumFacts {
                title: snapshot.held_text(record.title),
                artist: snapshot.held_text(record.artist),
                sort: snapshot.held_text(record.sort),
                year: row.try_get("year")?,
                release_date: row.try_get("release_date")?,
                date_added: row.try_get("date_added")?,
                compilation: row
                    .try_get::<Option<i64>, _>("is_compilation")?
                    .map(|value| value != 0),
            },
            first_seen: row.try_get("first_seen_at")?,
            release_identity: row.try_get("release_identity")?,
            release_lookup_identity: row.try_get("release_lookup_identity")?,
            state: native_state(row)?,
            binding: row.try_get("native_artwork")?,
            input: record.flags & INPUT != 0,
            artists,
            genres,
            writes: AlbumWrites::default(),
        })
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_album(record),
            None => snapshot.remove_album(AlbumKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        source_of(snapshot, &snapshot.albums, key)
    }

    fn normalize(&mut self, _before: Option<&Self>) {
        if let Some(header) = &self.writes.header {
            self.input = header.is_some();
        }
    }

    fn state(&mut self) -> Option<&mut NativeState> {
        Some(&mut self.state)
    }

    fn user_item(&self) -> Option<(UserItem, &str)> {
        Some((UserItem::Album(self.key.raw()), &self.uri))
    }
}

impl Owned for Album {
    type Metadata = AlbumMetadata;
    owned!();

    fn create(source: SourceKey, _tag: Option<TagKind>, object: &str, uri: String) -> Self {
        Album::new(source, object, uri)
    }

    fn header(&self) -> Option<Value> {
        None
    }

    fn set_header(&mut self, _metadata: &AlbumMetadata, json: Value) {
        self.writes.header = Some(Some(json));
    }

    fn touch(&self, touched: &mut Touched) {
        touched.albums.insert(self.key.raw());
    }
}

impl Row for Artist {
    family!(
        ARTISTS,
        Artist,
        artists,
        uri,
        ArtistKey::raw,
        ArtistKey::from_raw
    );

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, None, &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn cells(&self) -> Vec<Option<Cell>> {
        let mut cells = vec![
            num(self.source.raw()),
            string(&self.object),
            string(&self.uri),
            flag(self.indexed),
            flag(self.native),
            string(&self.metadata.name),
            text(self.metadata.sort_text.as_deref()),
            text(self.metadata.musicbrainz_artist_id.as_deref()),
        ];
        cells.extend(state_cells(&self.state));
        cells.extend([
            blob(self.binding.as_deref()),
            num(picture_flag(self.binding.as_deref())),
        ]);
        cells
    }

    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self> {
        let key: i64 = row.try_get(0)?;
        let record = snapshot
            .artists
            .get(key)
            .ok_or_else(|| missing("artists", key))?;
        Ok(Self {
            key: ArtistKey::from_raw(key),
            source: snapshot
                .source_key(record.source)
                .ok_or_else(|| missing("artists", key))?,
            object: row.try_get("object_id")?,
            uri: row.try_get("media_uri")?,
            indexed: record.flags & INDEXED != 0,
            native: record.flags & NATIVE != 0,
            metadata: ArtistMetadata {
                name: snapshot.held_text(record.name),
                sort_text: snapshot.held_optional(record.sort),
                musicbrainz_artist_id: row.try_get("musicbrainz_artist_id")?,
            },
            state: native_state(row)?,
            binding: row.try_get("native_artwork")?,
        })
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_artist(record),
            None => snapshot.remove_artist(ArtistKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        source_of(snapshot, &snapshot.artists, key)
    }

    fn state(&mut self) -> Option<&mut NativeState> {
        Some(&mut self.state)
    }

    fn user_item(&self) -> Option<(UserItem, &str)> {
        Some((UserItem::Artist(self.key.raw()), &self.uri))
    }
}

impl Owned for Artist {
    type Metadata = ArtistMetadata;
    const BUILT_PICTURE: bool = true;
    owned!();

    fn create(source: SourceKey, _tag: Option<TagKind>, object: &str, uri: String) -> Self {
        Artist::new(source, object, uri)
    }

    fn header(&self) -> Option<Value> {
        serde_json::to_value(&self.metadata).ok()
    }

    fn set_header(&mut self, metadata: &ArtistMetadata, _json: Value) {
        self.metadata = metadata.clone();
    }

    fn picture(&self, observed: Option<&[u8]>, native: bool) -> LibraryResult<Option<Vec<u8>>> {
        let pictures = if native {
            observed
        } else {
            self.binding.as_deref().or(observed)
        };
        Ok(Some(super::artwork::artist_binding(
            &self.metadata.name,
            self.metadata.musicbrainz_artist_id.as_deref(),
            pictures,
        )?))
    }

    fn touch(&self, touched: &mut Touched) {
        touched.artists.insert(self.key.raw());
    }
}

impl Row for Tag {
    family!(
        TAGS,
        Tag,
        tags,
        uri,
        std::convert::identity,
        std::convert::identity
    );

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, Some(self.kind), &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn cells(&self) -> Vec<Option<Cell>> {
        vec![
            num(self.source.raw()),
            num(self.kind as i64),
            string(&self.object),
            string(&self.uri),
            flag(self.indexed),
            flag(self.native),
            string(&self.metadata.name),
            text(self.metadata.sort_text.as_deref()),
            blob(self.binding.as_deref()),
            num(picture_flag(self.binding.as_deref())),
        ]
    }

    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self> {
        let key: i64 = row.try_get(0)?;
        let record = snapshot.tags.get(key).ok_or_else(|| missing("tags", key))?;
        Ok(Self {
            key,
            source: snapshot
                .source_key(record.source)
                .ok_or_else(|| missing("tags", key))?,
            kind: record.kind(),
            object: row.try_get("object_id")?,
            uri: row.try_get("media_uri")?,
            indexed: record.flags & INDEXED != 0,
            native: record.flags & NATIVE != 0,
            metadata: TagMetadata {
                name: snapshot.held_text(record.name),
                sort_text: snapshot.held_optional(record.sort),
            },
            binding: row.try_get("native_artwork")?,
        })
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_tag(record),
            None => snapshot.remove_tag(key),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        source_of(snapshot, &snapshot.tags, key)
    }
}

impl Owned for Tag {
    type Metadata = TagMetadata;
    owned!();

    fn create(source: SourceKey, tag: Option<TagKind>, object: &str, uri: String) -> Self {
        Tag::new(tag.unwrap_or(TagKind::Genre), source, object, uri)
    }

    fn header(&self) -> Option<Value> {
        serde_json::to_value(&self.metadata).ok()
    }

    fn set_header(&mut self, metadata: &TagMetadata, _json: Value) {
        self.metadata = metadata.clone();
    }
}

impl Row for Folder {
    family!(
        FOLDERS,
        Folder,
        folders,
        uri,
        FolderKey::raw,
        FolderKey::from_raw
    );

    fn object(&self) -> Option<(SourceKey, Option<TagKind>, &str)> {
        Some((self.source, None, &self.object))
    }

    fn source(&self) -> Option<SourceKey> {
        Some(self.source)
    }

    fn cells(&self) -> Vec<Option<Cell>> {
        vec![
            num(self.source.raw()),
            string(&self.object),
            string(&self.uri),
            flag(self.indexed),
            flag(self.native),
            text(self.metadata.name.as_deref()),
            text(self.metadata.sort_text.as_deref()),
            blob(self.binding.as_deref()),
            num(picture_flag(self.binding.as_deref())),
        ]
    }

    fn read(row: &SqliteRow, snapshot: &Snapshot) -> LibraryResult<Self> {
        let key: i64 = row.try_get(0)?;
        let record = snapshot
            .folders
            .get(key)
            .ok_or_else(|| missing("folders", key))?;
        Ok(Self {
            key: FolderKey::from_raw(key),
            source: snapshot
                .source_key(record.source)
                .ok_or_else(|| missing("folders", key))?,
            object: row.try_get("object_id")?,
            uri: row.try_get("media_uri")?,
            indexed: record.flags & INDEXED != 0,
            native: record.flags & NATIVE != 0,
            metadata: FolderMetadata {
                name: snapshot.held_optional(record.name),
                sort_text: snapshot.held_optional(record.sort),
            },
            binding: row.try_get("native_artwork")?,
        })
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_folder(record),
            None => snapshot.remove_folder(FolderKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        source_of(snapshot, &snapshot.folders, key)
    }
}

impl Owned for Folder {
    type Metadata = FolderMetadata;
    owned!();

    fn create(source: SourceKey, _tag: Option<TagKind>, object: &str, uri: String) -> Self {
        Folder::new(source, object, uri)
    }

    fn header(&self) -> Option<Value> {
        serde_json::to_value(&self.metadata).ok()
    }

    fn set_header(&mut self, metadata: &FolderMetadata, _json: Value) {
        self.metadata = metadata.clone();
    }

    fn keeps_header(metadata: &FolderMetadata) -> bool {
        metadata.name.is_none()
    }
}
