//! Every change to the Store runs in one Write. A Write takes the writer and begins a
//! transaction. Resident rows are staged and compared with what the Catalog and the Store hold;
//! only the cells that differ are written. User data an operation writes with plain SQL is
//! staged with the values its statement returned. When nothing differs the transaction rolls
//! back and nothing is published. Otherwise the Write commits, applies the same rows to the
//! Catalog and publishes one Change before it releases the writer.
//! The lock order is always the writer, then the Catalog.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use sqlx::Row as _;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteTransactionManager;
use sqlx_core::transaction::TransactionManager;

use super::families::{self, Cell, Column, Kind, Row, Run, Table};
use super::records::*;
use super::snapshot::{
    Entry, Playlist, PlaylistFolder, SmartPlaylist, Snapshot, Stats, UserItem, UserState,
};
use crate::db::{Mark, WriterGuard};
use crate::tags::TagKind;
use crate::{
    Catalog, LibraryError, LibraryResult, PlaylistKey, SmartPlaylistKey, SourceKey, TrackKey,
};

/// The favorite and rating a row shows.
type Shown = (Option<bool>, Option<u8>);

const PERSIST_ROWS: usize = 256;
const READ_KEYS: usize = 256;
const APPLY_RECORDS: usize = 8_192;
/// Bound parameters per statement, below SQLite's limit.
const PARAMETERS: usize = 32_000;

/// What a Source scope reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePhase {
    Added,
    Changed,
    Removed,
    /// A provider listing of the source completed.
    Completed,
}

/// One part of the Store a committed Write changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    /// The Catalog was loaded again; everything may have changed.
    Reload,
    /// Sort text or the track cover choice changed, nothing stored did; rows read again show it.
    Display,
    Tracks {
        source: Option<SourceKey>,
        keys: Vec<TrackKey>,
        /// The keys of tracks the Store did not hold before.
        added: Vec<TrackKey>,
    },
    /// Albums, artists, tags or folders of a source.
    Collections {
        source: SourceKey,
    },
    Source {
        key: SourceKey,
        phase: SourcePhase,
    },
    /// Favorites and ratings rows show that changed.
    State {
        items: Vec<StateChange>,
    },
    /// The favorites and ratings Rufin holds for these URIs changed, or whether one waits for
    /// its server. A URI no track, album or artist row has counts too.
    Held {
        uris: Vec<String>,
    },
    Listened {
        track: TrackKey,
    },
    Playlist {
        key: PlaylistKey,
        entries: bool,
    },
    /// A playlist was linked to a playlist file or unlinked, and nothing it shows changed.
    PlaylistLink {
        key: PlaylistKey,
    },
    /// Playlists were added, removed or moved. A rename is a `Playlist` scope.
    Playlists,
    Smart {
        key: SmartPlaylistKey,
    },
    /// Playlist folders.
    Folders,
    /// The saved queue or its playback state.
    Queue,
    /// Tracks whose downloaded copy was added or removed.
    Downloads {
        tracks: Vec<DownloadChange>,
    },
    /// The lyrics a person picked for this track changed.
    Lyrics {
        uri: String,
    },
    Home {
        source: SourceKey,
    },
    Rekey {
        old: TrackKey,
        new: TrackKey,
    },
    /// An analysis result or an audio hash was written.
    Analysis {
        hash: [u8; 32],
    },
}

/// A track whose downloaded copy was added or removed.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DownloadChange {
    pub track: crate::TrackKey,
    pub uri: String,
    pub downloaded: bool,
}

impl DownloadChange {
    /// Shows the download in a track row of the same track.
    pub fn show_in(&self, row: &mut crate::TrackRow) {
        if row.track_key == self.track {
            row.is_downloaded = self.downloaded;
        }
    }
}

/// A track, album or artist whose shown favorite or rating changed.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StateChange {
    pub target: crate::FavoriteTarget,
    /// The target's row key: a TrackKey, AlbumKey or ArtistKey by its kind.
    pub key: i64,
    /// The favorite it shows now.
    pub favorite: bool,
    /// The rating it shows now.
    pub rating: Option<u8>,
    /// Whether the rating it shows changed.
    pub rating_changed: bool,
}

impl StateChange {
    /// The track it names.
    pub fn track(&self) -> Option<crate::TrackKey> {
        matches!(self.target, crate::FavoriteTarget::Track(_))
            .then(|| crate::TrackKey::from_raw(self.key))
    }

    /// The album it names.
    pub fn album(&self) -> Option<crate::AlbumKey> {
        matches!(self.target, crate::FavoriteTarget::Album(_))
            .then(|| crate::AlbumKey::from_raw(self.key))
    }

    /// The artist it names.
    pub fn artist(&self) -> Option<crate::ArtistKey> {
        matches!(self.target, crate::FavoriteTarget::Artist(_))
            .then(|| crate::ArtistKey::from_raw(self.key))
    }

    /// Whether a row's `state` already shows this favorite and rating.
    pub fn shown_in(&self, state: &crate::TrackState) -> bool {
        state.favorite == self.favorite && state.rating == crate::read::shown_rating(self.rating)
    }

    /// Shows this favorite and rating in a row's `state`.
    pub fn show_in(&self, state: &mut crate::TrackState) {
        state.favorite = self.favorite;
        state.rating = crate::read::shown_rating(self.rating);
    }
}

/// A row that shows a track's, album's or artist's favorite and rating.
pub trait ShowsState {
    /// The state this row shows of the item `change` names, if it shows that item.
    fn state(&self, _change: &crate::StateChange) -> Option<&crate::TrackState> {
        None
    }
    fn state_mut(&mut self, _change: &crate::StateChange) -> Option<&mut crate::TrackState> {
        None
    }
}

impl ShowsState for crate::TrackRow {
    fn state(&self, change: &crate::StateChange) -> Option<&crate::TrackState> {
        (change.track() == Some(self.track_key)).then_some(&self.state)
    }
    fn state_mut(&mut self, change: &crate::StateChange) -> Option<&mut crate::TrackState> {
        (change.track() == Some(self.track_key)).then_some(&mut self.state)
    }
}

impl ShowsState for crate::AlbumRow {
    fn state(&self, change: &crate::StateChange) -> Option<&crate::TrackState> {
        (change.album() == Some(self.album_key)).then_some(&self.state)
    }
    fn state_mut(&mut self, change: &crate::StateChange) -> Option<&mut crate::TrackState> {
        (change.album() == Some(self.album_key)).then_some(&mut self.state)
    }
}

impl ShowsState for crate::ArtistRow {
    fn state(&self, change: &crate::StateChange) -> Option<&crate::TrackState> {
        (change.artist() == Some(self.artist_key)).then_some(&self.state)
    }
    fn state_mut(&mut self, change: &crate::StateChange) -> Option<&mut crate::TrackState> {
        (change.artist() == Some(self.artist_key)).then_some(&mut self.state)
    }
}

impl ShowsState for crate::Row {
    fn state(&self, change: &crate::StateChange) -> Option<&crate::TrackState> {
        match self {
            Self::Track(track) | Self::AlbumTrack { track, .. } => track.state(change),
            Self::Album(album) | Self::AlbumHeading(album) => album.state(change),
            Self::Artist(artist) => artist.state(change),
            Self::Tag(_) | Self::Playlist(_) | Self::Folder(_) => None,
        }
    }
    fn state_mut(&mut self, change: &crate::StateChange) -> Option<&mut crate::TrackState> {
        match self {
            Self::Track(track) | Self::AlbumTrack { track, .. } => track.state_mut(change),
            Self::Album(album) | Self::AlbumHeading(album) => album.state_mut(change),
            Self::Artist(artist) => artist.state_mut(change),
            Self::Tag(_) | Self::Playlist(_) | Self::Folder(_) => None,
        }
    }
}

impl ShowsState for crate::TagRow {}
impl ShowsState for crate::PlaylistListRow {}

/// One committed Write, as clients and features see it.
#[derive(Clone, Debug)]
pub struct Change {
    pub revision: u64,
    pub scopes: Vec<Scope>,
}

impl Change {
    /// The sources whose catalog rows changed.
    pub fn sources(&self) -> BTreeSet<SourceKey> {
        self.scopes
            .iter()
            .filter_map(|scope| match scope {
                Scope::Tracks {
                    source: Some(source),
                    ..
                }
                | Scope::Collections { source }
                | Scope::Source { key: source, .. } => Some(*source),
                _ => None,
            })
            .collect()
    }
}

/// The record families and their ids in the observation seen map.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum Family {
    Source,
    Track,
    Album,
    Artist,
    Tag,
    Folder,
    Playlist,
}

/// What the Store holds for a staged row: its cells, None where it was never read, and its
/// link runs.
#[derive(Clone)]
struct Stored {
    cells: Vec<Option<Cell>>,
    runs: Vec<Vec<i64>>,
}

impl Stored {
    fn of<R: Row>(record: &R) -> Self {
        Self {
            cells: record.cells(),
            runs: record.runs(),
        }
    }
}

struct Staged<R> {
    /// The row when this Write first staged it; None for a row this Write inserted.
    before: Option<R>,
    current: Option<R>,
    /// What the Store holds now. None once a removal was written.
    stored: Option<Stored>,
    /// The Write removed a row it never read.
    unread: bool,
    /// Staged since the last flush.
    dirty: bool,
}

impl<R: PartialEq> Staged<R> {
    fn changed(&self) -> bool {
        self.unread || self.before != self.current
    }
}

impl<R: Row> Staged<R> {
    /// When the row changed only in the favorite, rating or plays its source reports: whether
    /// the plays changed. Clients show plays as activity and favorites and ratings as state, so
    /// such a row changes no catalog page.
    fn native_only(&self) -> Option<bool> {
        let (Some(before), Some(current)) = (&self.before, &self.current) else {
            return None;
        };
        let (mut before, mut current) = (before.clone(), current.clone());
        let (Some(prior), Some(state)) = (before.state(), current.state()) else {
            return None;
        };
        let played = (prior.play_count, prior.last_played) != (state.play_count, state.last_played);
        *state = prior.clone();
        (current == before).then_some(played)
    }

    /// Whether the favorite or rating the row's source reports changed.
    fn rated(&self) -> bool {
        let (Some(mut before), Some(mut current)) = (self.before.clone(), self.current.clone())
        else {
            return false;
        };
        before
            .state()
            .zip(current.state())
            .is_some_and(|(prior, state)| {
                (prior.favorite, prior.rating) != (state.favorite, state.rating)
            })
    }
}

/// The rows of one family this Write read or staged.
pub(crate) struct Overlay<R> {
    order: Vec<i64>,
    rows: HashMap<i64, Staged<R>>,
    /// Rows read from the Store and not staged.
    fetched: HashMap<i64, R>,
    naturals: HashMap<String, i64>,
    objects: HashMap<(SourceKey, Option<TagKind>, String), i64>,
    /// Keys, natural keys and object ids a batch read found no row for.
    missing_keys: HashSet<i64>,
    missing_naturals: HashSet<String>,
    missing_objects: HashSet<(SourceKey, Option<TagKind>, String)>,
}

impl<R> Default for Overlay<R> {
    fn default() -> Self {
        Self {
            order: Vec::new(),
            rows: HashMap::new(),
            fetched: HashMap::new(),
            naturals: HashMap::new(),
            objects: HashMap::new(),
            missing_keys: HashSet::new(),
            missing_naturals: HashSet::new(),
            missing_objects: HashSet::new(),
        }
    }
}

impl<R: Row> Overlay<R> {
    /// The row this Write knows under a key: staged, or read and unchanged.
    fn known(&self, key: i64) -> Option<Option<&R>> {
        match self.rows.get(&key) {
            Some(staged) => Some(staged.current.as_ref()),
            None => self.fetched.get(&key).map(Some),
        }
    }

    fn remember(&mut self, record: &R) {
        self.naturals
            .insert(record.natural().to_owned(), record.key());
        if let Some((source, tag, object)) = record.object() {
            self.objects
                .insert((source, tag, object.to_owned()), record.key());
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Open,
    /// COMMIT was sent; whether it landed is unknown until it returns.
    Committing,
    Committed,
    Done,
}

/// RETURNING of a user_state statement: the URI, the favorite and rating it holds now, and the
/// track, album and artist with that URI.
pub(crate) const STATE_RETURNING: &str = " RETURNING media_uri,favorite,rating,(SELECT track_key FROM tracks WHERE tracks.media_uri=user_state.media_uri),(SELECT album_key FROM albums WHERE albums.media_uri=user_state.media_uri),(SELECT artist_key FROM artists WHERE artists.media_uri=user_state.media_uri)";
/// The same for a DELETE: the URI holds no state any more.
pub(crate) const STATE_DELETED: &str = " RETURNING media_uri,NULL,NULL,(SELECT track_key FROM tracks WHERE tracks.media_uri=user_state.media_uri),(SELECT album_key FROM albums WHERE albums.media_uri=user_state.media_uri),(SELECT artist_key FROM artists WHERE artists.media_uri=user_state.media_uri)";
pub(crate) type StateRow = (
    String,
    Option<bool>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// A playlist's header and entries as operations wrote them.
#[derive(Default)]
struct PlaylistEdit {
    /// The header the Store holds now; None inside when it was removed.
    header: Option<Option<Playlist>>,
    linked: Option<bool>,
    removed: Vec<i64>,
    put: Vec<Entry<TrackKey>>,
}

/// User data operations wrote, as the Store holds it now.
#[derive(Default)]
struct UserEdits {
    states: BTreeMap<UserItem, (UserState, String)>,
    listened: BTreeMap<TrackKey, Stats>,
    downloads: BTreeMap<TrackKey, (bool, String)>,
    playlists: BTreeMap<i64, PlaylistEdit>,
    smart: BTreeMap<i64, Option<SmartPlaylist>>,
    folders: BTreeMap<i64, Option<PlaylistFolder>>,
}

/// The resident user data a commit applies: what differs from the Catalog.
#[derive(Default)]
struct UserChanges {
    states: Vec<(UserItem, UserState, String)>,
    listened: Vec<(TrackKey, Stats)>,
    downloads: Vec<(TrackKey, bool, String)>,
    /// Playlists, and whether more than the link changed, and whether their entries did.
    playlists: Vec<(i64, Option<Playlist>, bool, bool)>,
    smart: Vec<(i64, Option<SmartPlaylist>)>,
    folders: Option<BTreeMap<i64, PlaylistFolder>>,
}

impl UserChanges {
    fn is_empty(&self) -> bool {
        self.states.is_empty()
            && self.listened.is_empty()
            && self.downloads.is_empty()
            && self.playlists.is_empty()
            && self.smart.is_empty()
            && self.folders.is_none()
    }
}

pub(crate) struct Write<'a> {
    catalog: &'a Catalog,
    writer: WriterGuard<'a>,
    // Boxed: the futures of syncs and scans hold a Write, and they nest deeply.
    pub(super) sources: Box<Overlay<Source>>,
    pub(super) folders: Box<Overlay<Folder>>,
    pub(super) artists: Box<Overlay<Artist>>,
    pub(super) tags: Box<Overlay<Tag>>,
    pub(super) albums: Box<Overlay<Album>>,
    pub(super) tracks: Box<Overlay<Track>>,
    edits: UserEdits,
    /// The walk this Write belongs to, when an observation started one.
    pub(super) walk: Option<u64>,
    /// Rows an observation saw, by family and key, with the row's source.
    seen: Vec<(Family, i64, SourceKey)>,
    /// Sources this Write forgets, with the tracks they keep as hidden rows without a source.
    forgotten: Vec<(SourceKey, Vec<TrackKey>)>,
    scopes: Vec<Scope>,
    rekeys: Vec<(TrackKey, TrackKey)>,
    /// Plain SQL changed rows no client shows.
    kept: bool,
    /// Plain SQL replaced whole tables, so the Catalog loads again after the commit.
    reload: bool,
    /// Something was staged since the last flush.
    dirty: bool,
    stage: Stage,
}

impl Catalog {
    /// Opens the one write context. It waits for the Catalog load and holds the writer until
    /// it commits or drops.
    pub(crate) async fn write(&self) -> LibraryResult<Write<'_>> {
        self.loaded().await;
        let mut writer = self.writer().await?;
        if self.reload_pending() {
            let revision = self.reload_under(&mut writer).await?;
            self.publish(Arc::new(Change {
                revision,
                scopes: vec![Scope::Reload],
            }));
        }
        SqliteTransactionManager::begin(&mut writer, None).await?;
        Ok(Write {
            catalog: self,
            writer,
            sources: Box::default(),
            folders: Box::default(),
            artists: Box::default(),
            tags: Box::default(),
            albums: Box::default(),
            tracks: Box::default(),
            edits: UserEdits::default(),
            walk: None,
            seen: Vec::new(),
            forgotten: Vec::new(),
            scopes: Vec::new(),
            rekeys: Vec::new(),
            kept: false,
            reload: false,
            dirty: false,
            stage: Stage::Open,
        })
    }
}

/// Rows per statement for rows of `width` parameters.
fn page_rows(width: usize) -> usize {
    (PARAMETERS / width.max(1)).clamp(1, PERSIST_ROWS)
}

/// Runs a statement per page of rows; `sql` builds it for a page's length.
async fn paged(
    connection: &mut SqliteConnection,
    rows: &[Vec<Cell>],
    sql: impl Fn(usize) -> String,
) -> LibraryResult<()> {
    let width = rows.first().map_or(1, Vec::len);
    for page in rows.chunks(page_rows(width)) {
        let statement = sql(page.len());
        families::bind(
            sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).persistent(false),
            page.iter().flatten(),
        )
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

impl<'a> Write<'a> {
    /// A row by key: staged, read before, or read from the Store now.
    pub(crate) async fn get<R: Row>(&mut self, key: i64) -> LibraryResult<Option<R>> {
        let overlay = R::overlay(self);
        if let Some(known) = overlay.known(key) {
            return Ok(known.cloned());
        }
        if overlay.missing_keys.contains(&key) {
            return Ok(None);
        }
        self.read_keys::<R>(&[key]).await?;
        Ok(R::overlay(self).known(key).flatten().cloned())
    }

    pub(crate) async fn by_natural<R: Row>(&mut self, natural: &str) -> LibraryResult<Option<R>> {
        if let Some(record) = self.natural_known::<R>(natural) {
            return Ok(Some(record));
        }
        if R::overlay(self).missing_naturals.contains(natural) {
            return Ok(None);
        }
        self.read_naturals::<R>(&[natural]).await?;
        Ok(self.natural_known::<R>(natural))
    }

    fn natural_known<R: Row>(&self, natural: &str) -> Option<R> {
        let overlay = R::overlay(self);
        let key = overlay.naturals.get(natural)?;
        overlay
            .known(*key)
            .flatten()
            .filter(|record| record.natural() == natural)
            .cloned()
    }

    pub(crate) async fn by_object<R: Row>(
        &mut self,
        source: SourceKey,
        tag: Option<TagKind>,
        object: &str,
    ) -> LibraryResult<Option<R>> {
        if let Some(record) = self.object_known::<R>(source, tag, object) {
            return Ok(Some(record));
        }
        let identity = (source, tag, object.to_owned());
        if R::overlay(self).missing_objects.contains(&identity) {
            return Ok(None);
        }
        self.read_objects::<R>(source, tag, &[object]).await?;
        Ok(self.object_known::<R>(source, tag, object))
    }

    fn object_known<R: Row>(
        &self,
        source: SourceKey,
        tag: Option<TagKind>,
        object: &str,
    ) -> Option<R> {
        let overlay = R::overlay(self);
        let key = overlay.objects.get(&(source, tag, object.to_owned()))?;
        overlay
            .known(*key)
            .flatten()
            .filter(|record| record.object() == Some((source, tag, object)))
            .cloned()
    }

    /// Reads the rows of these keys in one statement per page, so the per-item reads that
    /// follow answer from memory, also for keys the Store holds no row for.
    pub(crate) async fn read_keys<R: Row>(&mut self, keys: &[i64]) -> LibraryResult<()> {
        let table = R::TABLE;
        for page in keys.chunks(READ_KEYS) {
            let condition = format!(
                "{}.{} IN({})",
                table.name,
                table.key,
                families::placeholders(1, page.len())
            );
            let select = table.select(&condition);
            let mut query = sqlx::query(sqlx::AssertSqlSafe(select.as_str()));
            for key in page {
                query = query.bind(*key);
            }
            let found = self.fetch::<R>(query).await?;
            let overlay = R::overlay_mut(self);
            for key in page {
                if !found.contains(key) {
                    overlay.missing_keys.insert(*key);
                }
            }
        }
        Ok(())
    }

    /// Reads the rows of these natural keys, as `read_keys` does.
    pub(crate) async fn read_naturals<R: Row>(&mut self, naturals: &[&str]) -> LibraryResult<()> {
        let table = R::TABLE;
        for page in naturals.chunks(READ_KEYS) {
            let condition = format!(
                "{}.{} IN({})",
                table.name,
                table.natural,
                families::placeholders(1, page.len())
            );
            let select = table.select(&condition);
            let mut query = sqlx::query(sqlx::AssertSqlSafe(select.as_str()));
            for natural in page {
                query = query.bind(*natural);
            }
            self.fetch::<R>(query).await?;
            let overlay = R::overlay_mut(self);
            for natural in page {
                if !overlay.naturals.contains_key(*natural) {
                    overlay.missing_naturals.insert((*natural).to_owned());
                }
            }
        }
        Ok(())
    }

    /// Reads the rows of these object ids of a source, as `read_keys` does.
    pub(crate) async fn read_objects<R: Row>(
        &mut self,
        source: SourceKey,
        tag: Option<TagKind>,
        objects: &[&str],
    ) -> LibraryResult<()> {
        let table = R::TABLE;
        for page in objects.chunks(READ_KEYS) {
            let select = table.select(&table.objects(page.len()));
            let mut query = sqlx::query(sqlx::AssertSqlSafe(select.as_str())).bind(source);
            for object in page {
                query = query.bind(*object);
            }
            if table.tagged {
                query = query.bind(tag.map(|kind| kind as i64));
            }
            self.fetch::<R>(query).await?;
            let overlay = R::overlay_mut(self);
            for object in page {
                let identity = (source, tag, (*object).to_owned());
                if !overlay.objects.contains_key(&identity) {
                    overlay.missing_objects.insert(identity);
                }
            }
        }
        Ok(())
    }

    /// Runs a read and remembers each row it finds that this Write does not know yet. A row
    /// this Write staged answers from its staged version, so a row that moved away from the
    /// identity it was found by is not returned for it. Returns the keys found.
    async fn fetch<R: Row>(&mut self, query: families::Query<'_>) -> LibraryResult<HashSet<i64>> {
        let rows = query.fetch_all(&mut *self.writer).await?;
        let catalog = self.catalog;
        let snapshot = catalog.snapshot();
        let mut found = HashSet::new();
        for row in rows {
            let key: i64 = row.try_get(0)?;
            found.insert(key);
            if R::overlay(self).known(key).is_some() {
                continue;
            }
            let record = R::read(&row, &snapshot)?;
            let overlay = R::overlay_mut(self);
            overlay.remember(&record);
            overlay.fetched.insert(key, record);
        }
        Ok(found)
    }

    /// Whether this Write staged a row of the family.
    pub(crate) fn staged<R: Row>(&self) -> bool {
        !R::overlay(self).order.is_empty()
    }

    /// Inserts new rows at once, so they get SQLite's keys through RETURNING. Their details and
    /// links are written with the next flush.
    pub(crate) async fn insert<R: Row>(&mut self, mut rows: Vec<R>) -> LibraryResult<Vec<R>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        self.flush().await?;
        let table = R::TABLE;
        let own = table.columns.len();
        for record in &mut rows {
            record.normalize(None);
        }
        let cells = rows.iter().map(R::cells).collect::<Vec<_>>();
        let mut groups = Vec::<(u64, Vec<usize>)>::new();
        for (index, row) in cells.iter().enumerate() {
            let mask = mask(&row[..own]);
            match groups.iter_mut().find(|(known, _)| *known == mask) {
                Some((_, indices)) => indices.push(index),
                None => groups.push((mask, vec![index])),
            }
        }
        let mut keys = HashMap::new();
        for (mask, indices) in groups {
            let columns = masked(table.columns, mask);
            for page in indices.chunks(page_rows(columns.len())) {
                let statement = table.insert(&columns, page.len());
                let inserted = families::bind(
                    sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).persistent(false),
                    page.iter()
                        .flat_map(|index| cells[*index][..own].iter().flatten()),
                )
                .fetch_all(&mut *self.writer)
                .await?;
                for row in inserted {
                    keys.insert(row.try_get::<String, _>(1)?, row.try_get::<i64, _>(0)?);
                }
            }
        }
        let mut records = Vec::with_capacity(rows.len());
        for (mut record, mut cells) in rows.into_iter().zip(cells) {
            let key = *keys.get(record.natural()).ok_or_else(|| {
                LibraryError::InvalidStore(format!("{} insert returned no key", table.name))
            })?;
            record.set_key(key);
            // The details row and the links do not exist yet.
            if let Some(details) = table.details {
                for (cell, column) in cells[own..].iter_mut().zip(details.columns) {
                    *cell = (column.kind == Kind::Read).then_some(Cell::Null);
                }
            }
            let stored = Stored {
                cells,
                runs: vec![Vec::new(); table.runs.len()],
            };
            self.stage(record.clone(), None, Some(stored));
            records.push(record);
        }
        Ok(records)
    }

    /// Stages a changed row. A row equal to the known one stages nothing.
    pub(crate) fn put<R: Row>(&mut self, mut record: R) {
        let key = record.key();
        let overlay = R::overlay(self);
        let previous = match overlay.rows.get(&key) {
            Some(staged) => staged.current.clone(),
            None => overlay.fetched.get(&key).cloned(),
        };
        record.normalize(previous.as_ref());
        if previous.as_ref() == Some(&record) {
            return;
        }
        let overlay = R::overlay_mut(self);
        let (before, stored) = if overlay.rows.contains_key(&key) {
            (None, None)
        } else {
            let before = overlay.fetched.remove(&key);
            let stored = before.as_ref().map(Stored::of);
            (before, stored)
        };
        self.stage(record, before, stored);
    }

    pub(crate) fn remove<R: Row>(&mut self, key: i64) {
        self.dirty = true;
        let overlay = R::overlay_mut(self);
        if let Some(staged) = overlay.rows.get_mut(&key) {
            staged.current = None;
            staged.dirty = true;
            return;
        }
        let before = overlay.fetched.remove(&key);
        overlay.order.push(key);
        overlay.rows.insert(
            key,
            Staged {
                stored: Some(before.as_ref().map_or(
                    Stored {
                        cells: Vec::new(),
                        runs: Vec::new(),
                    },
                    Stored::of,
                )),
                unread: before.is_none(),
                before,
                current: None,
                dirty: true,
            },
        );
    }

    fn stage<R: Row>(&mut self, record: R, before: Option<R>, stored: Option<Stored>) {
        self.dirty = true;
        let overlay = R::overlay_mut(self);
        let key = record.key();
        overlay.remember(&record);
        overlay.fetched.remove(&key);
        match overlay.rows.get_mut(&key) {
            Some(staged) => {
                staged.current = Some(record);
                staged.dirty = true;
            }
            None => {
                overlay.order.push(key);
                overlay.rows.insert(
                    key,
                    Staged {
                        before,
                        current: Some(record),
                        stored,
                        unread: false,
                        dirty: true,
                    },
                );
            }
        }
    }

    /// Records that an observation saw a row. A walk keeps it as seen; a write outside a walk
    /// keeps every open walk of the row's source from changing it.
    pub(crate) fn see<R: Row>(&mut self, record: &R) {
        if let Some(source) = record.source() {
            self.seen.push((R::FAMILY, record.key(), source));
        }
    }

    pub(crate) fn see_playlist(&mut self, key: i64, source: SourceKey) {
        self.seen.push((Family::Playlist, key, source));
    }

    /// Whether a write outside this Write's walk changed the row while the walk ran.
    pub(crate) fn newer(&self, family: Family, key: i64) -> bool {
        self.walk
            .is_some_and(|walk| self.catalog.mark(walk, family, key) == Some(Mark::Live))
    }

    /// Whether this Write or its walk saw the row.
    pub(crate) fn saw(&self, family: Family, key: i64) -> bool {
        self.seen
            .iter()
            .any(|(seen, row, _)| *seen == family && *row == key)
            || self
                .walk
                .is_some_and(|walk| self.catalog.mark(walk, family, key).is_some())
    }

    /// Drops every row a source owns from memory after the commit, except the tracks it kept.
    /// The caller removed them from the Store with plain SQL.
    pub(crate) fn forget_source(&mut self, source: SourceKey, kept: Vec<TrackKey>) {
        self.forgotten.push((source, kept));
    }

    pub(crate) async fn source_by_object(&mut self, object: &str) -> LibraryResult<Option<Source>> {
        self.by_natural(object).await
    }

    /// The source with this object id, inserted empty when it is new.
    pub(crate) async fn source_identity(&mut self, object: &str) -> LibraryResult<SourceKey> {
        if object.is_empty() {
            return Err(LibraryError::InvalidRequest(
                "source identity cannot be empty".into(),
            ));
        }
        if let Some(source) = self.source_by_object(object).await? {
            return Ok(source.key);
        }
        let source = Source {
            key: SourceKey::from_raw(0),
            object: object.to_owned(),
            kind: None,
            name: String::new(),
            complete: false,
            freshness: None,
            synced_at: None,
        };
        Ok(self.insert(vec![source]).await?.remove(0).key)
    }

    pub(crate) fn rekey(&mut self, old: TrackKey, new: TrackKey) {
        self.rekeys.push((old, new));
    }

    /// Reports a change to a table the Catalog does not hold. Callers report only rows that
    /// changed.
    pub(crate) fn changed(&mut self, scope: Scope) {
        if !self.scopes.contains(&scope) {
            self.scopes.push(scope);
        }
    }

    /// Keeps what plain SQL changed in tables no client shows, such as freshness markers and
    /// delivery queues. The commit publishes nothing for it.
    pub(crate) fn keep(&mut self) {
        self.kept = true;
    }

    /// Loads the Catalog again after the commit instead of applying what was staged. A restore
    /// that replaces whole tables of user data uses it.
    pub(crate) fn reload(&mut self) {
        self.reload = true;
    }

    /// Stages user_state rows a statement changed and returned with STATE_RETURNING or
    /// STATE_DELETED: what rows show, and the values Rufin holds.
    pub(crate) fn states_changed(&mut self, rows: Vec<StateRow>) {
        self.held(rows.iter().map(|row| row.0.clone()).collect());
        self.set_states(rows);
    }

    /// Stages the Held scope of these URIs.
    pub(crate) fn held(&mut self, uris: Vec<String>) {
        if !uris.is_empty() {
            self.changed(Scope::Held { uris });
        }
    }

    /// Stages what user_state rows read with STATE_RETURNING show.
    pub(crate) fn set_states(&mut self, rows: Vec<StateRow>) {
        for (uri, favorite, rating, track, album, artist) in rows {
            let state = user_state(favorite, rating);
            for item in [
                track.map(UserItem::Track),
                album.map(UserItem::Album),
                artist.map(UserItem::Artist),
            ]
            .into_iter()
            .flatten()
            {
                self.edits.states.insert(item, (state, uri.clone()));
            }
        }
    }

    /// The plays, skips and last play of a track, with what this Write staged.
    pub(crate) fn stats(&self, key: TrackKey) -> Stats {
        self.edits
            .listened
            .get(&key)
            .copied()
            .unwrap_or_else(|| self.catalog.snapshot().listened(key))
    }

    /// Stages a track's plays, skips and last play.
    pub(crate) fn listened(&mut self, key: TrackKey, stats: Stats) {
        self.edits.listened.insert(key, stats);
    }

    /// Counts the plays of the tracks with these URIs again from their listens and carried-over
    /// counts. Imports and identity moves change those with plain SQL.
    pub(crate) async fn recount(&mut self, uris: &[&str]) -> LibraryResult<()> {
        for page in uris.chunks(READ_KEYS) {
            let sql = listened_sql(&format!(
                "track.media_uri IN({})",
                families::placeholders(1, page.len())
            ));
            let mut query = sqlx::query_as::<_, (i64, i64, i64, Option<i64>)>(sqlx::AssertSqlSafe(
                sql.as_str(),
            ));
            for uri in page {
                query = query.bind(*uri);
            }
            for (key, plays, skips, last) in query.fetch_all(self.connection().await?).await? {
                self.listened(TrackKey::from_raw(key), Stats::listened(plays, skips, last));
            }
        }
        Ok(())
    }

    /// Reads whether the tracks with these URIs have a downloaded copy after plain SQL changed
    /// their download rows.
    pub(crate) async fn read_downloads(&mut self, uris: &[&str]) -> LibraryResult<()> {
        for page in uris.chunks(READ_KEYS) {
            let sql = format!(
                "SELECT track.track_key,EXISTS(SELECT 1 FROM downloads download WHERE download.media_uri=track.media_uri),track.media_uri FROM tracks track WHERE track.media_uri IN({})",
                families::placeholders(1, page.len())
            );
            let mut query =
                sqlx::query_as::<_, (i64, bool, String)>(sqlx::AssertSqlSafe(sql.as_str()));
            for uri in page {
                query = query.bind(*uri);
            }
            for (key, downloaded, uri) in query.fetch_all(self.connection().await?).await? {
                self.edits
                    .downloads
                    .insert(TrackKey::from_raw(key), (downloaded, uri));
            }
        }
        Ok(())
    }

    /// Stages a playlist header a statement returned with PLAYLIST_COLUMNS.
    pub(crate) fn put_playlist(&mut self, row: PlaylistRow) {
        let header = playlist(row, Vec::new());
        let edit = self.edits.playlists.entry(header.key).or_default();
        edit.linked = None;
        edit.header = Some(Some(header));
    }

    pub(crate) fn remove_playlist(&mut self, key: i64) {
        self.edits.playlists.entry(key).or_default().header = Some(None);
    }

    /// A playlist was linked to a file or unlinked.
    pub(crate) fn link_playlist(&mut self, key: i64, linked: bool) {
        self.edits.playlists.entry(key).or_default().linked = Some(linked);
    }

    /// Entries plain SQL inserted into a playlist or gave another rank or track.
    pub(crate) fn put_entries(&mut self, playlist: i64, entries: Vec<Entry<TrackKey>>) {
        if !entries.is_empty() {
            let edit = self.edits.playlists.entry(playlist).or_default();
            edit.removed
                .retain(|key| !entries.iter().any(|entry| entry.key == *key));
            edit.put
                .retain(|put| !entries.iter().any(|entry| entry.key == put.key));
            edit.put.extend(entries);
        }
    }

    /// Entries plain SQL removed from a playlist.
    pub(crate) fn remove_entries(&mut self, playlist: i64, keys: Vec<i64>) {
        if !keys.is_empty() {
            let edit = self.edits.playlists.entry(playlist).or_default();
            edit.put.retain(|entry| !keys.contains(&entry.key));
            edit.removed.extend(keys);
        }
    }

    /// Stages a smart playlist a statement returned with SMART_COLUMNS.
    pub(crate) fn put_smart(&mut self, row: SmartRow) {
        let smart = smart_playlist(row);
        self.edits.smart.insert(smart.key, Some(smart));
    }

    pub(crate) fn remove_smart(&mut self, key: i64) {
        self.edits.smart.insert(key, None);
    }

    /// Stages a playlist folder a statement returned with FOLDER_COLUMNS.
    pub(crate) fn put_folder(&mut self, (key, object, name, smart): FolderRow) {
        self.edits.folders.insert(
            key,
            Some(PlaylistFolder {
                key,
                object,
                name,
                smart,
            }),
        );
    }

    pub(crate) fn remove_folder(&mut self, key: i64) {
        self.edits.folders.insert(key, None);
    }

    /// The open transaction for plain SQL and Store reads. Staged rows are written first, so
    /// the SQL sees them. Plain SQL never writes a resident column of a resident table; it
    /// stages what it changed of user data instead.
    pub(crate) async fn connection(&mut self) -> LibraryResult<&mut SqliteConnection> {
        self.flush().await?;
        Ok(&mut self.writer)
    }

    /// Writes every staged difference into the open transaction.
    async fn flush(&mut self) -> LibraryResult<()> {
        if !std::mem::take(&mut self.dirty) {
            return Ok(());
        }
        let mut writes = Writes::default();
        persist(&mut self.sources, &mut writes);
        persist(&mut self.folders, &mut writes);
        persist(&mut self.artists, &mut writes);
        persist(&mut self.tags, &mut writes);
        persist(&mut self.albums, &mut writes);
        persist(&mut self.tracks, &mut writes);
        let connection = &mut *self.writer;
        for group in &writes.updates {
            let ((table, details), (_, mask)) = (group.statement, group.key);
            let columns = masked(table.columns, mask);
            let sql = |count| {
                if details {
                    table.upsert(&columns, count)
                } else {
                    table.update(&columns, count)
                }
            };
            paged(connection, &group.rows, sql).await?;
        }
        for group in &writes.run_deletes {
            paged(connection, &group.rows, |count| {
                group.statement.delete(count)
            })
            .await?;
        }
        for group in &writes.run_inserts {
            paged(connection, &group.rows, |count| {
                group.statement.insert(count)
            })
            .await?;
        }
        for group in writes.deletes.iter().rev() {
            let (table, keys) = (group.statement, &group.rows);
            // Links name their targets without a foreign key, so they go with them.
            let roles = match table.name {
                "artists" => Some("0,1"),
                "tags" => Some("2,3"),
                "folders" => Some("4"),
                _ => None,
            };
            if let Some(roles) = roles {
                for links in ["track_links", "album_links"] {
                    paged(connection, keys, |count| {
                        format!(
                            "DELETE FROM {links} WHERE role IN({roles}) AND target IN({})",
                            families::placeholders(1, count)
                        )
                    })
                    .await?;
                }
            }
            paged(connection, keys, |count| table.delete(count)).await?;
        }
        Ok(())
    }

    /// Commits, applies the staged rows in memory and publishes them. A Write that changed
    /// nothing rolls back and publishes nothing.
    pub(crate) async fn commit(mut self) -> LibraryResult<Option<Arc<Change>>> {
        self.flush().await?;
        let user = if self.reload {
            UserChanges::default()
        } else {
            self.attached().await?;
            self.user_changes()
        };
        let mut scopes = self.scopes(&user);
        if self.reload && !scopes.contains(&Scope::Reload) {
            scopes.insert(0, Scope::Reload);
        }
        let shown = if self.reload {
            Vec::new()
        } else {
            self.shown_before(&user)
        };
        // A commit of rows memory does not hold leaves the Catalog and the image as they are.
        let resident =
            self.reload || self.changed_rows() || !self.forgotten.is_empty() || !user.is_empty();
        if scopes.is_empty() && !self.kept && !resident {
            SqliteTransactionManager::rollback(&mut self.writer).await?;
            self.stage = Stage::Done;
            self.catalog
                .record_seen(self.walk, std::mem::take(&mut self.seen));
            return Ok(None);
        }
        if resident {
            self.catalog.count_commit(&mut self.writer).await?;
        }
        self.stage = Stage::Committing;
        SqliteTransactionManager::commit(&mut self.writer).await?;
        self.stage = Stage::Committed;
        let revision = if self.reload {
            self.catalog.require_reload();
            Some(self.catalog.reload_under(&mut self.writer).await?)
        } else if resident {
            Some(self.apply(user))
        } else {
            None
        };
        self.stage = Stage::Done;
        if resident {
            self.catalog.committed();
        }
        if let Some(scope) = self.shown_after(shown) {
            scopes.push(scope);
        }
        self.catalog
            .record_seen(self.walk, std::mem::take(&mut self.seen));
        if scopes.is_empty() {
            return Ok(None);
        }
        let revision = revision.unwrap_or_else(|| self.catalog.next_revision());
        let change = Arc::new(Change { revision, scopes });
        self.catalog.publish(Arc::clone(&change));
        Ok(Some(change))
    }

    /// The tracks, albums and artists whose shown favorite or rating this Write may change,
    /// with their URIs and what they show before it applies.
    fn shown_before(&self, user: &UserChanges) -> Vec<(UserItem, String, Shown)> {
        fn rated<R: Row>(overlay: &Overlay<R>, items: &mut BTreeMap<UserItem, String>) {
            for key in &overlay.order {
                let staged = &overlay.rows[key];
                if staged.rated()
                    && let Some((item, uri)) = staged.current.as_ref().and_then(R::user_item)
                {
                    items.insert(item, uri.to_owned());
                }
            }
        }
        let mut items = user
            .states
            .iter()
            .map(|(item, _, uri)| (*item, uri.clone()))
            .collect::<BTreeMap<_, _>>();
        rated(&self.tracks, &mut items);
        rated(&self.albums, &mut items);
        rated(&self.artists, &mut items);
        let snapshot = self.catalog.snapshot();
        items
            .into_iter()
            .map(|(item, uri)| {
                let shown = snapshot.shown_state(item).unwrap_or_default();
                (item, uri, shown)
            })
            .collect()
    }

    /// The State scope for what the applied Write changed of what these rows show.
    fn shown_after(&self, before: Vec<(UserItem, String, Shown)>) -> Option<Scope> {
        if before.is_empty() {
            return None;
        }
        let snapshot = self.catalog.snapshot();
        let items = before
            .into_iter()
            .filter_map(|(item, uri, before)| {
                let after = snapshot.shown_state(item)?;
                (after != before).then(|| StateChange {
                    target: item.target(uri),
                    key: item.key(),
                    favorite: after.0.unwrap_or(false),
                    rating: after.1,
                    rating_changed: after.1 != before.1,
                })
            })
            .collect::<Vec<_>>();
        (!items.is_empty()).then_some(Scope::State { items })
    }

    /// Rows this Write inserted or moved to another URI or identity take the user data the
    /// Store holds for them: favorites and ratings by URI, plays by URI and identity, and
    /// downloads by URI.
    async fn attached(&mut self) -> LibraryResult<()> {
        let mut states = BTreeSet::new();
        let mut tracks = BTreeSet::new();
        for key in &self.tracks.order {
            let staged = &self.tracks.rows[key];
            if let Some(current) = &staged.current
                && staged.before.as_ref().is_none_or(|before| {
                    before.uri != current.uri
                        || before.source != current.source
                        || before.object != current.object
                })
            {
                states.insert(current.uri.clone());
                tracks.insert(current.uri.clone());
            }
        }
        for uri in self
            .albums
            .order
            .iter()
            .filter_map(|key| moved(&self.albums.rows[key], |album| &album.uri))
            .chain(
                self.artists
                    .order
                    .iter()
                    .filter_map(|key| moved(&self.artists.rows[key], |artist| &artist.uri)),
            )
        {
            states.insert(uri.clone());
        }
        if !states.is_empty() {
            let rows = read_states(&mut self.writer, &states).await?;
            self.set_states(rows);
        }
        if !tracks.is_empty() {
            let uris = tracks.iter().map(String::as_str).collect::<Vec<_>>();
            self.recount(&uris).await?;
            self.read_downloads(&uris).await?;
        }
        Ok(())
    }

    /// What the staged user data changes of the Catalog.
    fn user_changes(&mut self) -> UserChanges {
        let edits = std::mem::take(&mut self.edits);
        let snapshot = self.catalog.snapshot();
        let mut changes = UserChanges {
            states: edits
                .states
                .into_iter()
                .filter(|(item, (state, _))| {
                    snapshot.user_state(*item).unwrap_or_default() != *state
                })
                .map(|(item, (state, uri))| (item, state, uri))
                .collect(),
            listened: edits
                .listened
                .into_iter()
                .filter(|(key, stats)| snapshot.listened(*key) != *stats)
                .collect(),
            downloads: edits
                .downloads
                .into_iter()
                .filter(|(key, (downloaded, _))| {
                    snapshot.downloaded(*key).unwrap_or(false) != *downloaded
                })
                .map(|(key, (downloaded, uri))| (key, downloaded, uri))
                .collect(),
            ..UserChanges::default()
        };
        for (key, edit) in edits.playlists {
            let resident = snapshot.playlist(key);
            let mut playlist = match edit.header {
                Some(None) => None,
                Some(Some(mut header)) => {
                    header.entries = resident
                        .as_ref()
                        .map(|resident| resident.entries.clone())
                        .unwrap_or_default();
                    Some(header)
                }
                None => resident.clone(),
            };
            if let Some(playlist) = &mut playlist {
                if let Some(linked) = edit.linked {
                    playlist.linked = linked;
                }
                edit_entries(&mut playlist.entries, &edit.removed, edit.put);
            }
            if playlist != resident {
                let entries = playlist.as_ref().map(|playlist| &playlist.entries)
                    != resident.as_ref().map(|playlist| &playlist.entries);
                let shown = match (&playlist, &resident) {
                    (Some(next), Some(before)) => {
                        Playlist {
                            linked: before.linked,
                            ..next.clone()
                        } != *before
                    }
                    _ => true,
                };
                changes.playlists.push((key, playlist, shown, entries));
            }
        }
        changes.smart = edits
            .smart
            .into_iter()
            .filter(|(key, smart)| smart.as_ref() != snapshot.smart_playlists.get(key))
            .collect();
        let mut folders = snapshot.playlist_folders.clone();
        for (key, folder) in edits.folders {
            match folder {
                Some(folder) => folders.insert(key, folder),
                None => folders.remove(&key),
            };
        }
        if folders != snapshot.playlist_folders {
            changes.folders = Some(folders);
        }
        changes
    }

    /// Whether a staged row differs from the Store, published or not.
    fn changed_rows(&self) -> bool {
        fn any<R: PartialEq>(overlay: &Overlay<R>) -> bool {
            overlay.rows.values().any(Staged::changed)
        }
        any(&self.sources)
            || any(&self.folders)
            || any(&self.artists)
            || any(&self.tags)
            || any(&self.albums)
            || any(&self.tracks)
    }

    fn scopes(&self, user: &UserChanges) -> Vec<Scope> {
        let mut scopes = Vec::new();
        let snapshot = self.catalog.snapshot();
        let source_of = |key: i64| Track::resident_source(&snapshot, key);
        let mut tracks = BTreeMap::<Option<SourceKey>, (Vec<TrackKey>, Vec<TrackKey>)>::new();
        let mut played = Vec::new();
        for key in &self.tracks.order {
            let staged = &self.tracks.rows[key];
            if !staged.changed() {
                continue;
            }
            if let Some(plays) = staged.native_only() {
                if plays {
                    played.push(TrackKey::from_raw(*key));
                }
                continue;
            }
            let mut sources = staged
                .before
                .iter()
                .chain(&staged.current)
                .map(|track| track.source)
                .collect::<Vec<_>>();
            if staged.before.is_none() && staged.current.is_none() {
                sources.push(source_of(*key));
            }
            sources.dedup();
            let added = staged.before.is_none() && staged.current.is_some();
            for source in sources {
                let (keys, new) = tracks.entry(source).or_default();
                keys.push(TrackKey::from_raw(*key));
                if added {
                    new.push(TrackKey::from_raw(*key));
                }
            }
        }
        scopes.extend(
            tracks
                .into_iter()
                .map(|(source, (keys, added))| Scope::Tracks {
                    source,
                    keys,
                    added,
                }),
        );
        let mut collections = BTreeSet::new();
        collect_sources(&self.albums, &snapshot, &mut collections);
        collect_sources(&self.artists, &snapshot, &mut collections);
        collect_sources(&self.tags, &snapshot, &mut collections);
        collect_sources(&self.folders, &snapshot, &mut collections);
        scopes.extend(
            collections
                .into_iter()
                .map(|source| Scope::Collections { source }),
        );
        for key in &self.sources.order {
            let staged = &self.sources.rows[key];
            let phase = match (&staged.before, &staged.current) {
                (_, None) => SourcePhase::Removed,
                (None, Some(_)) => SourcePhase::Added,
                // The freshness marker is read by key only, so clients hear nothing of it.
                (Some(before), Some(current))
                    if (&before.kind, &before.name, before.complete)
                        == (&current.kind, &current.name, current.complete) =>
                {
                    continue;
                }
                _ => SourcePhase::Changed,
            };
            scopes.push(Scope::Source {
                key: SourceKey::from_raw(*key),
                phase,
            });
        }
        for (source, _) in &self.forgotten {
            scopes.push(Scope::Collections { source: *source });
            scopes.push(Scope::Source {
                key: *source,
                phase: SourcePhase::Removed,
            });
            let slot = snapshot.source_slot(*source);
            if snapshot
                .playlists
                .iter()
                .any(|(_, playlist)| Some(playlist.source) == slot)
            {
                scopes.push(Scope::Playlists);
            }
        }
        scopes.extend(
            user.listened
                .iter()
                .map(|(track, _)| *track)
                .chain(played)
                .map(|track| Scope::Listened { track }),
        );
        if !user.downloads.is_empty() {
            scopes.push(Scope::Downloads {
                tracks: user
                    .downloads
                    .iter()
                    .map(|(track, downloaded, uri)| DownloadChange {
                        track: *track,
                        uri: uri.clone(),
                        downloaded: *downloaded,
                    })
                    .collect(),
            });
        }
        let mut listed = false;
        for (key, playlist, shown, entries) in &user.playlists {
            // The link is read by the playlist file feature, not shown.
            if !shown {
                scopes.push(Scope::PlaylistLink {
                    key: PlaylistKey::from_raw(*key),
                });
                continue;
            }
            let resident = snapshot.playlist(*key);
            listed |= playlist.is_none()
                || resident.is_none()
                || playlist
                    .as_ref()
                    .zip(resident.as_ref())
                    .is_some_and(|(next, before)| {
                        (next.folder, next.indexed, next.created)
                            != (before.folder, before.indexed, before.created)
                    });
            scopes.push(Scope::Playlist {
                key: PlaylistKey::from_raw(*key),
                entries: *entries,
            });
        }
        if listed && !scopes.contains(&Scope::Playlists) {
            scopes.push(Scope::Playlists);
        }
        scopes.extend(user.smart.iter().map(|(key, _)| Scope::Smart {
            key: SmartPlaylistKey::from_raw(*key),
        }));
        if user.folders.is_some() {
            scopes.push(Scope::Folders);
        }
        drop(snapshot);
        for (old, new) in &self.rekeys {
            scopes.push(Scope::Rekey {
                old: *old,
                new: *new,
            });
        }
        for scope in &self.scopes {
            if !scopes.contains(scope) {
                scopes.push(scope.clone());
            }
        }
        scopes
    }

    /// Applies the staged rows under the Catalog lock, at most APPLY_RECORDS per hold.
    fn apply(&mut self, user: UserChanges) -> u64 {
        let states = !user.states.is_empty();
        let plays = !user.listened.is_empty();
        let downloads = !user.downloads.is_empty();
        let playlists = !user.playlists.is_empty()
            || !user.smart.is_empty()
            || user.folders.is_some()
            || !self.forgotten.is_empty();
        let mut applier = Applier {
            catalog: self.catalog,
            guard: None,
            held: 0,
        };
        applier.family(&self.sources);
        applier.family(&self.folders);
        applier.family(&self.artists);
        applier.family(&self.tags);
        applier.family(&self.albums);
        applier.family(&self.tracks);
        for (item, state, _) in user.states {
            applier.snapshot().set_user_state(item, state);
        }
        for (key, stats) in user.listened {
            applier.snapshot().set_listened(key, stats);
        }
        for (key, downloaded, _) in user.downloads {
            applier.snapshot().set_downloaded(key, downloaded);
        }
        for (key, playlist, _, _) in user.playlists {
            match playlist {
                Some(playlist) => applier.snapshot().put_playlist(&playlist),
                None => applier.snapshot().remove_playlist(key),
            }
        }
        for (key, smart) in user.smart {
            match smart {
                Some(smart) => applier.snapshot().smart_playlists.insert(key, smart),
                None => applier.snapshot().smart_playlists.remove(&key),
            };
        }
        if let Some(folders) = user.folders {
            applier.snapshot().playlist_folders = folders;
        }
        for (source, kept) in &self.forgotten {
            applier.snapshot().forget_source(*source, kept);
        }
        let facts = self.changed_rows() || !self.forgotten.is_empty();
        let snapshot = applier.snapshot();
        snapshot.revision += 1;
        let revision = snapshot.revision;
        for (changed, stamp) in [
            (facts, &mut snapshot.facts),
            (states, &mut snapshot.states),
            (plays, &mut snapshot.plays),
            (downloads, &mut snapshot.downloads),
            (playlists, &mut snapshot.playlist_stamp),
        ] {
            if changed {
                *stamp = revision;
            }
        }
        revision
    }
}

impl Drop for Write<'_> {
    fn drop(&mut self) {
        if self.stage != Stage::Done {
            self.catalog.image_may_be_current();
        }
        match self.stage {
            Stage::Open => SqliteTransactionManager::start_rollback(&mut self.writer),
            Stage::Committing => {
                SqliteTransactionManager::start_rollback(&mut self.writer);
                self.catalog.require_reload();
            }
            // Memory is behind the Store, so the next Write reloads it first.
            Stage::Committed => self.catalog.require_reload(),
            Stage::Done => {}
        }
    }
}

/// The URI of a row this Write inserted or moved.
fn moved<R: Clone>(staged: &Staged<R>, uri: impl Fn(&R) -> &String) -> Option<&String> {
    let current = staged.current.as_ref()?;
    staged
        .before
        .as_ref()
        .is_none_or(|before| uri(before) != uri(current))
        .then(|| uri(current))
}

fn collect_sources<R: Row>(
    overlay: &Overlay<R>,
    snapshot: &Snapshot,
    sources: &mut BTreeSet<SourceKey>,
) {
    for key in &overlay.order {
        let staged = &overlay.rows[key];
        // Plays the source reports for an album or artist follow the plays of its tracks, and
        // its favorite and rating are state.
        if !staged.changed() || staged.native_only().is_some() {
            continue;
        }
        sources.extend(
            staged
                .before
                .iter()
                .chain(&staged.current)
                .filter_map(Row::source)
                .chain(R::resident_source(snapshot, *key)),
        );
    }
}

/// The columns whose bits a mask sets.
fn masked(columns: &'static [Column], mask: u64) -> Vec<&'static Column> {
    columns
        .iter()
        .enumerate()
        .filter(|(index, _)| mask & (1 << index) != 0)
        .map(|(_, column)| column)
        .collect()
}

/// The bits of the cells that are set.
fn mask(cells: &[Option<Cell>]) -> u64 {
    cells
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.is_some())
        .fold(0, |mask, (index, _)| mask | 1 << index)
}

/// The statements one flush runs, grouped so each runs once per page of rows. A row is the key
/// cell, then the changed cells.
#[derive(Default)]
struct Writes {
    /// Rows by table and the columns they change; the flag marks a details table.
    updates: Vec<Group<(&'static str, u64), (&'static Table, bool)>>,
    run_deletes: Vec<Group<(&'static str, &'static str), &'static Run>>,
    run_inserts: Vec<Group<(&'static str, &'static str), &'static Run>>,
    deletes: Vec<Group<&'static str, &'static Table>>,
}

struct Group<K, V> {
    key: K,
    statement: V,
    rows: Vec<Vec<Cell>>,
}

fn group<K: PartialEq, V>(groups: &mut Vec<Group<K, V>>, key: K, statement: V, row: Vec<Cell>) {
    match groups.iter_mut().find(|group| group.key == key) {
        Some(group) => group.rows.push(row),
        None => groups.push(Group {
            key,
            statement,
            rows: vec![row],
        }),
    }
}

fn persist<R: Row>(overlay: &mut Overlay<R>, writes: &mut Writes) {
    let table = R::TABLE;
    let own = table.columns.len();
    for key in &overlay.order {
        let staged = overlay.rows.get_mut(key).expect("staged keys have rows");
        if !std::mem::take(&mut staged.dirty) {
            continue;
        }
        let Some(record) = &staged.current else {
            if staged.stored.take().is_some() {
                group(
                    &mut writes.deletes,
                    table.name,
                    table,
                    vec![Cell::Int(*key)],
                );
            }
            continue;
        };
        let stored = staged.stored.take();
        let known = |index: usize| stored.as_ref().and_then(|stored| stored.cells.get(index));
        let cells = record.cells();
        for (range, details) in [(0..own, None), (own..cells.len(), table.details)] {
            let start = range.start;
            let changed = range
                .filter(|index| {
                    cells[*index].is_some()
                        && known(*index).is_none_or(|known| *known != cells[*index])
                })
                .collect::<Vec<_>>();
            if changed.is_empty() {
                continue;
            }
            let mask = changed
                .iter()
                .fold(0, |mask, index| mask | 1 << (index - start));
            let mut row = vec![Cell::Int(*key)];
            row.extend(changed.iter().filter_map(|index| cells[*index].clone()));
            let target = details.unwrap_or(table);
            group(
                &mut writes.updates,
                (target.name, mask),
                (target, details.is_some()),
                row,
            );
        }
        let runs = record.runs();
        for (index, (run, items)) in table.runs.iter().zip(&runs).enumerate() {
            let known = stored
                .as_ref()
                .map(|stored| stored.runs.get(index).map_or(&[][..], Vec::as_slice));
            if known == Some(items.as_slice()) {
                continue;
            }
            if known.is_none_or(|known| !known.is_empty()) {
                group(
                    &mut writes.run_deletes,
                    (run.table, run.owner),
                    run,
                    vec![Cell::Int(*key), Cell::Int(run.role)],
                );
            }
            for (position, target) in items.iter().enumerate() {
                group(
                    &mut writes.run_inserts,
                    (run.table, run.owner),
                    run,
                    vec![
                        Cell::Int(*key),
                        Cell::Int(run.role),
                        Cell::Int(position as i64),
                        Cell::Int(*target),
                    ],
                );
            }
        }
        // A cell never read or set stays unknown, so a later stage writes it when it sets it.
        let known = stored.map(|stored| stored.cells).unwrap_or_default();
        staged.stored = Some(Stored {
            cells: cells
                .into_iter()
                .enumerate()
                .map(|(index, cell)| cell.or_else(|| known.get(index).cloned().flatten()))
                .collect(),
            runs,
        });
    }
}

struct Applier<'a> {
    catalog: &'a Catalog,
    guard: Option<std::sync::RwLockWriteGuard<'a, Snapshot>>,
    held: usize,
}

impl Applier<'_> {
    fn snapshot(&mut self) -> &mut Snapshot {
        if self.held == APPLY_RECORDS {
            self.guard = None;
            self.held = 0;
        }
        self.held += 1;
        self.guard
            .get_or_insert_with(|| self.catalog.snapshot_mut())
    }

    fn family<R: Row>(&mut self, overlay: &Overlay<R>) {
        for key in &overlay.order {
            R::apply(self.snapshot(), *key, overlay.rows[key].current.as_ref());
        }
    }
}

/// Favorites and ratings of tracks, albums and artists with these URIs.
async fn read_states(
    connection: &mut SqliteConnection,
    uris: &BTreeSet<String>,
) -> LibraryResult<Vec<StateRow>> {
    let mut states = Vec::new();
    for page in uris.iter().collect::<Vec<_>>().chunks(READ_KEYS) {
        let sql = format!(
            "WITH input(uri) AS(VALUES {}) SELECT input.uri,state.favorite,state.rating,(SELECT track_key FROM tracks WHERE media_uri=input.uri),(SELECT album_key FROM albums WHERE media_uri=input.uri),(SELECT artist_key FROM artists WHERE media_uri=input.uri) FROM input LEFT JOIN user_state state ON state.media_uri=input.uri",
            vec!["(?)"; page.len()].join(",")
        );
        let mut query = sqlx::query_as::<_, StateRow>(sqlx::AssertSqlSafe(sql.as_str()));
        for uri in page {
            query = query.bind(uri.as_str());
        }
        states.extend(query.fetch_all(&mut *connection).await?);
    }
    Ok(states)
}

pub(crate) fn user_state(favorite: Option<bool>, rating: Option<i64>) -> UserState {
    UserState {
        favorite,
        rating: rating.map(|rating| rating.clamp(0, 100) as u8),
    }
}

/// The SQL that counts the plays, skips and last play of the tracks a condition selects from
/// their listens and carried-over counts. The load counts the same way for every track.
pub(crate) fn listened_sql(condition: &str) -> String {
    format!(
        "SELECT track_key,plays+coalesce(legacy_plays,0),skips+coalesce(legacy_skips,0),max(coalesce(last,legacy_last),coalesce(legacy_last,last)) FROM(
            SELECT track.track_key,
                (SELECT count(*) FROM listens listen WHERE listen.media_uri=track.media_uri AND listen.skipped=0) plays,
                (SELECT count(*) FROM listens listen WHERE listen.media_uri=track.media_uri AND listen.skipped=1) skips,
                (SELECT max(started_at) FROM listens listen WHERE listen.media_uri=track.media_uri) last,
                legacy.play_count legacy_plays,legacy.skip_count legacy_skips,legacy.last_played_at legacy_last
            FROM tracks track LEFT JOIN sources source USING(source_key)
            LEFT JOIN legacy_activity legacy ON legacy.source_id=source.object_id AND legacy.track_object_id=track.object_id AND legacy.period='lifetime' AND legacy.item_kind='track'
            WHERE {condition})"
    )
}

/// The playlist header columns the Catalog holds, in load order. Statements that write a
/// header return them.
pub(crate) const PLAYLIST_COLUMNS: &str = "playlist_key,source_key,owner='source',indexed,coalesce(name,''),playlist_folder_key,created_at,writable,metadata_writable,EXISTS(SELECT 1 FROM playlist_file_links link WHERE link.playlist_key=playlists.playlist_key),object_id";

pub(crate) type PlaylistRow = (
    i64,
    Option<i64>,
    bool,
    bool,
    String,
    Option<i64>,
    i64,
    bool,
    bool,
    bool,
    String,
);

pub(crate) fn playlist(row: PlaylistRow, entries: Vec<Entry<TrackKey>>) -> Playlist {
    let (
        key,
        source,
        owned_by_source,
        indexed,
        name,
        folder,
        created,
        writable,
        metadata_writable,
        linked,
        object,
    ) = row;
    Playlist {
        key,
        source: source.map(SourceKey::from_raw),
        owned_by_source,
        indexed,
        object,
        name,
        folder,
        created,
        writable,
        metadata_writable,
        linked,
        entries,
    }
}

/// Applies removed and put entries to entries in rank order. A put entry replaces the one
/// with its key.
fn edit_entries(entries: &mut Vec<Entry<TrackKey>>, removed: &[i64], put: Vec<Entry<TrackKey>>) {
    let gone = removed
        .iter()
        .chain(put.iter().map(|entry| &entry.key))
        .collect::<std::collections::HashSet<_>>();
    entries.retain(|entry| !gone.contains(&entry.key));
    for entry in put {
        let at =
            entries.partition_point(|other| (&other.rank, other.key) < (&entry.rank, entry.key));
        entries.insert(at, entry);
    }
}

/// The smart playlist columns the Catalog holds. Statements that write one return them.
pub(crate) const SMART_COLUMNS: &str =
    "smart_playlist_key,name,playlist_folder_key,position,definition_json,object_id";

pub(crate) type SmartRow = (i64, String, Option<i64>, i64, String, String);

pub(crate) fn smart_playlist(
    (key, name, folder, position, definition, object): SmartRow,
) -> SmartPlaylist {
    SmartPlaylist {
        key,
        object,
        name,
        folder,
        position,
        definition: serde_json::from_str(&definition).unwrap_or_else(|error| {
            tracing::warn!(%error, key, "smart playlist has an unreadable definition");
            crate::SmartPlaylistDefinition::default()
        }),
    }
}

/// The playlist folder columns the Catalog holds. Statements that write one return them.
pub(crate) const FOLDER_COLUMNS: &str = "folder_key,object_id,name,smart";

pub(crate) type FolderRow = (i64, String, String, bool);
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtistMetadata, FavoriteTarget, FolderMetadata, MemberSelection, SourceId, TagMetadata,
    };

    const SOURCE: &str = "source-a";

    fn uri(kind: &str, object: &str) -> String {
        crate::source_entity_uri(&SourceId::new(SOURCE), kind, object)
    }

    fn track(source: SourceKey, object: &str, title: &str) -> Track {
        let mut track = Track::new(Some(source), Some(object.into()), uri("track", object));
        track.indexed = true;
        track.native = true;
        track.facts = TrackFacts {
            title: title.into(),
            artist: "Alpha".into(),
            album: "First".into(),
            sort: title.to_lowercase(),
            duration_ms: 180_000,
            number: 1,
            date_added: Some("2024-02-03".into()),
            comment: Some("kept".into()),
            ..TrackFacts::default()
        };
        track.first_seen = Some(1_700_000_000);
        track.source_path = Some(format!("/music/{object}.flac"));
        track
    }

    async fn assert_reloads(catalog: &Catalog) {
        let applied = catalog.snapshot().export();
        let fresh = crate::catalog::load::snapshot(catalog.path()).unwrap();
        assert_eq!(applied, fresh.export());
        let image = crate::catalog::image::round_trip(&catalog.snapshot()).unwrap();
        assert_eq!(applied, image.export());
    }

    /// The critical flow: after a scripted sequence of writes, memory equals a fresh load and
    /// an image of it.
    #[tokio::test]
    async fn applied_writes_equal_a_reload() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        let mut changes = catalog.changes();

        let mut write = catalog.write().await.unwrap();
        let source = write
            .insert(vec![Source {
                key: SourceKey::from_raw(0),
                object: SOURCE.into(),
                kind: Some("local".into()),
                name: "Local".into(),
                complete: false,
                synced_at: None,
                freshness: None,
            }])
            .await
            .unwrap()
            .remove(0);
        let mut artists = Vec::new();
        for (object, name) in [("a1", "Alpha"), ("a2", "Beta")] {
            let mut artist = Artist::new(source.key, object, uri("artist", object));
            artist.indexed = true;
            artist.metadata = ArtistMetadata {
                name: name.into(),
                ..ArtistMetadata::default()
            };
            artists.push(artist);
        }
        let artists = write.insert(artists).await.unwrap();
        let mut tags = Vec::new();
        for (kind, name) in [(TagKind::Genre, "Rock"), (TagKind::Mood, "Calm")] {
            let mut tag = Tag::new(kind, source.key, name, uri(kind.table().name, name));
            tag.indexed = true;
            tag.metadata = TagMetadata {
                name: name.into(),
                sort_text: None,
            };
            tags.push(tag);
        }
        let tags = write.insert(tags).await.unwrap();
        let mut folder = Folder::new(source.key, "music", uri("folder", "music"));
        folder.indexed = true;
        folder.native = true;
        folder.metadata = FolderMetadata {
            name: Some("Music".into()),
            sort_text: None,
        };
        let folder = write.insert(vec![folder]).await.unwrap().remove(0);
        let mut album = Album::new(source.key, "first", uri("album", "first"));
        album.indexed = true;
        album.facts = AlbumFacts {
            title: "First".into(),
            artist: "Alpha".into(),
            year: Some(2001),
            ..AlbumFacts::default()
        };
        album.first_seen = Some(1_700_000_000);
        album.artists = vec![artists[0].key];
        album.genres = vec![tags[0].key];
        album.binding = Some(br#"[{"image":{"kind":"album"}}]"#.to_vec());
        let album = write.insert(vec![album]).await.unwrap().remove(0);
        let mut inserted = Vec::new();
        for (object, title) in [("t1", "One"), ("t2", "Two"), ("t3", "Three")] {
            let mut record = track(source.key, object, title);
            record.album = Some(album.key);
            record.artists = vec![artists[0].key, artists[1].key];
            record.album_artists = vec![artists[0].key];
            record.tags = vec![(TagKind::Genre, tags[0].key), (TagKind::Mood, tags[1].key)];
            record.folders = vec![folder.key];
            record.state.play_count = Some(3);
            if object == "t1" {
                record.binding = Some(br#"[{"image":{"kind":"embedded"}}]"#.to_vec());
                record.cue_path = Some("/music/album.cue".into());
            }
            record.writes.metadata = Some(serde_json::json!({"bpm_source":1}));
            inserted.push(record);
        }
        let tracks = write.insert(inserted).await.unwrap();
        assert_eq!(
            write
                .by_object::<Track>(source.key, None, "t2")
                .await
                .unwrap()
                .map(|track| track.key),
            Some(tracks[1].key)
        );
        assert_eq!(
            write
                .by_natural::<Track>(&uri("track", "t3"))
                .await
                .unwrap()
                .map(|track| track.key),
            Some(tracks[2].key)
        );
        let first = write.commit().await.unwrap().unwrap();
        assert!(first.scopes.contains(&Scope::Source {
            key: source.key,
            phase: SourcePhase::Added,
        }));
        assert_reloads(&catalog).await;
        assert_eq!(catalog.snapshot().track_count(), 3);

        // Nothing changed: no write and no event.
        let mut write = catalog.write().await.unwrap();
        let unchanged = write
            .get::<Track>(tracks[0].key.raw())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.artists, tracks[0].artists);
        write.put(unchanged);
        assert!(write.commit().await.unwrap().is_none());

        let mut write = catalog.write().await.unwrap();
        let mut one = write
            .get::<Track>(tracks[0].key.raw())
            .await
            .unwrap()
            .unwrap();
        one.facts.title = "One (edit)".into();
        one.artists = vec![artists[1].key];
        one.binding = None;
        write.put(one.clone());
        let mut two = write
            .get::<Track>(tracks[1].key.raw())
            .await
            .unwrap()
            .unwrap();
        two.indexed = false;
        write.put(two);
        write.remove::<Track>(tracks[2].key.raw());
        assert!(
            write
                .get::<Track>(tracks[2].key.raw())
                .await
                .unwrap()
                .is_none()
        );
        let second = write.commit().await.unwrap().unwrap();
        assert!(second.scopes.iter().any(|scope| matches!(
            scope,
            Scope::Tracks { keys, .. } if keys.contains(&one.key)
        )));
        assert_reloads(&catalog).await;
        let hidden: Option<i64> =
            sqlx::query_scalar("SELECT hidden_since FROM tracks WHERE track_key=?1")
                .bind(tracks[1].key)
                .fetch_one(&mut *catalog.acquire_reader().await.unwrap())
                .await
                .unwrap();
        assert!(hidden.is_some());

        // User data written with plain SQL and staged from what its statements returned.
        let target = FavoriteTarget::Track(one.uri.clone());
        catalog.set_favorite(&target, true, true).await.unwrap();
        catalog.set_rating(&target, Some(8), false).await.unwrap();
        let listen: crate::ListenWrite = serde_json::from_value(serde_json::json!({
            "external_id":"listen-1", "media_uri": one.uri,
            "title":"One", "artist":"Alpha", "album":"First", "duration_millis":180000,
            "started_at":1_700_000_100,"local_period":"2023-11","listened_millis":180000,"skipped":false
        }))
        .unwrap();
        catalog.record_listen(&listen, None, &[]).await.unwrap();
        let (playlist, _) = catalog
            .create_playlist(
                "Mix",
                &MemberSelection::Uris(
                    vec![one.uri.clone(), "file:///elsewhere.flac".into()].into(),
                ),
            )
            .await
            .unwrap()
            .unwrap();
        let folder_key = catalog
            .create_playlist_folder("Folder", false)
            .await
            .unwrap();
        catalog
            .move_playlist_to_folder(
                crate::PlaylistFolderMember::Playlist { key: playlist },
                Some(folder_key),
            )
            .await
            .unwrap();
        catalog
            .save_download(&crate::DownloadFile {
                media_uri: one.uri.clone(),
                encoding: "original".into(),
                path: "file:///downloads/one.flac".into(),
                managed: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_reloads(&catalog).await;
        {
            let snapshot = catalog.snapshot();
            let state = snapshot.user_state(UserItem::Track(one.key.raw())).unwrap();
            assert_eq!((state.favorite, state.rating), (Some(true), Some(80)));
            assert_eq!(snapshot.listened(one.key).plays, 1);
            assert_eq!(snapshot.downloaded(one.key), Some(true));
            assert_eq!(
                snapshot.playlist(playlist.raw()).unwrap().folder,
                Some(folder_key.raw())
            );
        }

        // Removing what tracks name leaves them reading as none.
        let mut write = catalog.write().await.unwrap();
        write.remove::<Artist>(artists[1].key.raw());
        write.remove::<Tag>(tags[1].key);
        write.remove::<Folder>(folder.key.raw());
        write.remove::<Album>(album.key.raw());
        write.rekey(tracks[2].key, tracks[0].key);
        let third = write.commit().await.unwrap().unwrap();
        assert!(third.scopes.contains(&Scope::Rekey {
            old: tracks[2].key,
            new: tracks[0].key,
        }));
        assert_reloads(&catalog).await;
        let mut revisions = Vec::new();
        while let Ok(change) = changes.try_recv() {
            revisions.push(change.revision);
        }
        assert!(revisions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(revisions.contains(&third.revision));
    }
}
