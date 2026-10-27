//! Every change to the Store runs in one Write. A Write takes the writer and begins a
//! transaction. Resident rows are staged and compared with the rows the Store holds; only the
//! columns that differ are written. User data an operation writes with plain SQL is named by a
//! touch and read back by key before the commit. When nothing differs the transaction rolls
//! back and nothing is published. Otherwise the Write commits, applies the same rows to the
//! Catalog and publishes one Change before it releases the writer.
//! The lock order is always the writer, then the Catalog.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{Map, Value};
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteTransactionManager;
use sqlx_core::transaction::TransactionManager;

use super::families::{self, Row, Run};
use super::records::*;
use super::snapshot::{
    Playlist, PlaylistFolder, SmartPlaylist, Snapshot, Stats, UserItem, UserState,
};
use crate::db::{Mark, WriterGuard};
use crate::tags::TagKind;
use crate::{
    Catalog, LibraryError, LibraryResult, PlaylistKey, SmartPlaylistKey, SourceKey, TrackKey,
};

const PERSIST_ROWS: usize = 256;
const READ_KEYS: usize = 256;
const APPLY_RECORDS: usize = 8_192;

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
    Tracks {
        source: Option<SourceKey>,
        keys: Vec<TrackKey>,
    },
    /// Albums, artists, tags or folders of a source.
    Collections {
        source: SourceKey,
    },
    Source {
        key: SourceKey,
        phase: SourcePhase,
    },
    /// Favorites and ratings of these media URIs.
    State {
        uris: Vec<String>,
    },
    Listened {
        track: TrackKey,
    },
    Playlist {
        key: PlaylistKey,
        entries: bool,
    },
    /// Playlists were added, removed or renamed.
    Playlists,
    Smart {
        key: SmartPlaylistKey,
    },
    /// Playlist folders.
    Folders,
    Downloads {
        uris: Vec<String>,
    },
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
}

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

/// What the Store holds for a staged row.
#[derive(Clone)]
struct Stored {
    columns: Map<String, Value>,
    details: Option<Map<String, Value>>,
    runs: Vec<(&'static Run, Vec<i64>)>,
}

impl Stored {
    fn of<R: Row>(record: &R) -> Self {
        Self {
            columns: record.columns(),
            details: record.details(),
            runs: record.runs(),
        }
    }
}

struct Staged<R> {
    /// The row when this Write first staged it; None for a row this Write inserted.
    before: Option<R>,
    current: Option<R>,
    /// What the Store holds now. None when the Write did not read the row before staging it.
    stored: Option<Stored>,
    extras: Map<String, Value>,
    details: Map<String, Value>,
    /// The row changed even when its own columns did not: a put carried extras, or the Write
    /// removed a row it never read.
    extra: bool,
    /// Staged since the last flush.
    dirty: bool,
}

impl<R: PartialEq> Staged<R> {
    fn changed(&self) -> bool {
        self.extra || self.before != self.current
    }
}

impl<R: Resident> Staged<R> {
    /// Whether the row changed only in the plays its source reports. Clients show plays as
    /// activity, so such a row changes no catalog page.
    fn played(&self) -> bool {
        let (Some(before), Some(current)) = (&self.before, &self.current) else {
            return false;
        };
        let (mut before, mut current) = (before.clone(), current.clone());
        let (Some(prior), Some(state)) = (before.state(), current.state()) else {
            return false;
        };
        state.play_count = prior.play_count;
        state.last_played = prior.last_played;
        !self.extra && current == before
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
}

impl<R> Default for Overlay<R> {
    fn default() -> Self {
        Self {
            order: Vec::new(),
            rows: HashMap::new(),
            fetched: HashMap::new(),
            naturals: HashMap::new(),
            objects: HashMap::new(),
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

/// A resident family a Write stages.
pub(crate) trait Resident: Row {
    const FAMILY: Family;
    fn overlay<'b>(write: &'b Write<'_>) -> &'b Overlay<Self>;
    fn overlay_mut<'b>(write: &'b mut Write<'_>) -> &'b mut Overlay<Self>;
    /// Fills the links of a row read from the Store from its resident record.
    fn links(&mut self, _snapshot: &Snapshot) {}
    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>);
    /// The source of a resident record, for a removal of a row the Write never read.
    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey>;
    /// Columns the Write keeps consistent: when a track stops being listed, and whether an
    /// album has a header.
    fn normalize(&mut self, _before: Option<&Self>, _extras: &Map<String, Value>) {}
    /// The favorite, rating and plays the row's source reports, for rows that carry them.
    fn state(&mut self) -> Option<&mut NativeState> {
        None
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

/// User data operations name for read-back before the commit.
#[derive(Default)]
struct Touched {
    states: BTreeSet<String>,
    listened: BTreeSet<String>,
    downloads: BTreeSet<String>,
    playlists: BTreeSet<i64>,
    smart: BTreeSet<i64>,
    folders: bool,
}

/// The resident user data a commit applies.
#[derive(Default)]
struct UserChanges {
    states: Vec<(UserItem, UserState, String)>,
    listened: Vec<(TrackKey, Stats)>,
    downloads: Vec<(TrackKey, bool, String)>,
    playlists: Vec<(i64, Option<Playlist>, bool)>,
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
    sources: Overlay<Source>,
    folders: Overlay<Folder>,
    artists: Overlay<Artist>,
    tags: Overlay<Tag>,
    albums: Overlay<Album>,
    tracks: Overlay<Track>,
    touched: Touched,
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
            sources: Overlay::default(),
            folders: Overlay::default(),
            artists: Overlay::default(),
            tags: Overlay::default(),
            albums: Overlay::default(),
            tracks: Overlay::default(),
            touched: Touched::default(),
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

impl<'a> Write<'a> {
    /// A row by key: staged, read before, or read from the Store now.
    pub(crate) async fn get<R: Resident>(&mut self, key: i64) -> LibraryResult<Option<R>> {
        if let Some(known) = R::overlay(self).known(key) {
            return Ok(known.cloned());
        }
        let condition = format!("{}.{}=?1", R::TABLE.name, R::TABLE.key);
        self.read::<R>(&condition, |query| query.bind(key)).await
    }

    pub(crate) async fn by_natural<R: Resident>(
        &mut self,
        natural: &str,
    ) -> LibraryResult<Option<R>> {
        let overlay = R::overlay(self);
        if let Some(&key) = overlay.naturals.get(natural)
            && let Some(known) = overlay.known(key)
        {
            if let Some(record) = known.filter(|record| record.natural() == natural) {
                return Ok(Some(record.clone()));
            }
        }
        let condition = format!("{}.{}=?1", R::TABLE.name, R::TABLE.natural);
        let record = self
            .read::<R>(&condition, |query| query.bind(natural))
            .await?;
        Ok(record.filter(|record| record.natural() == natural))
    }

    pub(crate) async fn by_object<R: Resident>(
        &mut self,
        source: SourceKey,
        tag: Option<TagKind>,
        object: &str,
    ) -> LibraryResult<Option<R>> {
        let overlay = R::overlay(self);
        let identity = (source, tag, object.to_owned());
        let matches = |record: &R| record.object() == Some((source, tag, object));
        if let Some(&key) = overlay.objects.get(&identity)
            && let Some(Some(record)) = overlay.known(key)
            && matches(record)
        {
            return Ok(Some(record.clone()));
        }
        let record = self
            .read::<R>(R::TABLE.object, |query| {
                query
                    .bind(source)
                    .bind(object)
                    .bind(tag.map(|kind| kind as i64))
            })
            .await?;
        Ok(record.filter(matches))
    }

    /// Reads one row. A row this Write staged answers from its staged version, so a row that
    /// moved away from the identity it was found by is not returned for it.
    async fn read<R: Resident>(
        &mut self,
        condition: &str,
        bind: impl FnOnce(
            sqlx::query::Query<'_, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
        )
            -> sqlx::query::Query<'_, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    ) -> LibraryResult<Option<R>> {
        let select = R::TABLE.select(condition);
        let row = bind(sqlx::query(sqlx::AssertSqlSafe(select.as_str())))
            .fetch_optional(&mut *self.writer)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut record = R::read(&row)?;
        if let Some(known) = R::overlay(self).known(record.key()) {
            return Ok(known.cloned());
        }
        record.links(&self.catalog.snapshot());
        let overlay = R::overlay_mut(self);
        overlay.remember(&record);
        overlay.fetched.insert(record.key(), record.clone());
        Ok(Some(record))
    }

    /// Whether this Write staged a row of the family.
    pub(crate) fn staged<R: Resident>(&self) -> bool {
        !R::overlay(self).order.is_empty()
    }

    /// Inserts new rows at once, so they get SQLite's keys through RETURNING. Extras carry
    /// columns a row does not hold; for tracks they go to the details row.
    pub(crate) async fn insert<R: Resident>(
        &mut self,
        rows: Vec<(R, Map<String, Value>)>,
    ) -> LibraryResult<Vec<R>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        self.flush().await?;
        let table = R::TABLE;
        let rows = rows
            .into_iter()
            .map(|(mut record, extras)| {
                record.normalize(None, &extras);
                (record, extras)
            })
            .collect::<Vec<_>>();
        let input = rows
            .iter()
            .map(|(record, extras)| {
                let mut columns = record.columns();
                if table.details.is_none() {
                    columns.extend(extras.clone());
                }
                Value::Object(columns)
            })
            .collect::<Vec<_>>();
        let mut keys = HashMap::new();
        for page in input.chunks(PERSIST_ROWS) {
            let inserted: Vec<(i64, String)> =
                sqlx::query_as(sqlx::AssertSqlSafe(&*families::statements(table).insert))
                    .bind(Value::Array(page.to_vec()).to_string())
                    .fetch_all(&mut *self.writer)
                    .await?;
            keys.extend(inserted.into_iter().map(|(key, natural)| (natural, key)));
        }
        let mut records = Vec::with_capacity(rows.len());
        let mut details = Vec::new();
        let mut links = BTreeMap::<(&'static str, &'static str), Vec<Value>>::new();
        for (mut record, extras) in rows {
            let key = *keys.get(record.natural()).ok_or_else(|| {
                LibraryError::InvalidStore(format!("{} insert returned no key", table.name))
            })?;
            record.set_key(key);
            if let Some(mut row) = record.details() {
                row.retain(|_, value| !value.is_null());
                row.extend(extras);
                if !row.is_empty() {
                    row.insert(table.key.into(), key.into());
                    details.push(Value::Object(row));
                }
            }
            for (run, items) in record.runs() {
                links
                    .entry((run.table, run.owner))
                    .or_default()
                    .extend(link_rows(run, key, &items));
            }
            let stored = Stored::of(&record);
            self.stage(record.clone(), None, Some(stored), Map::new(), Map::new());
            R::overlay_mut(self)
                .rows
                .get_mut(&key)
                .expect("the row was staged")
                .dirty = false;
            records.push(record);
        }
        if let Some(table) = table.details {
            persist_details(&mut self.writer, table, details).await?;
        }
        for ((table, owner), rows) in links {
            execute(&mut self.writer, &families::run_insert(table, owner), rows).await?;
        }
        Ok(records)
    }

    /// Stages a changed row. Extras carry columns the row does not hold; for tracks they go to
    /// the details row. A row equal to the known one stages nothing.
    pub(crate) fn put<R: Resident>(&mut self, mut record: R, extras: Map<String, Value>) {
        let key = record.key();
        let overlay = R::overlay(self);
        let previous = match overlay.rows.get(&key) {
            Some(staged) => staged.current.clone(),
            None => overlay.fetched.get(&key).cloned(),
        };
        record.normalize(previous.as_ref(), &extras);
        if previous.as_ref() == Some(&record) && extras.is_empty() {
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
        let (row_extras, details) = if R::TABLE.details.is_some() {
            (Map::new(), extras)
        } else {
            (extras, Map::new())
        };
        self.stage(record, before, stored, row_extras, details);
    }

    pub(crate) fn remove<R: Resident>(&mut self, key: i64) {
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
                        columns: Map::new(),
                        details: None,
                        runs: Vec::new(),
                    },
                    Stored::of,
                )),
                extra: before.is_none(),
                before,
                current: None,
                extras: Map::new(),
                details: Map::new(),
                dirty: true,
            },
        );
    }

    fn stage<R: Resident>(
        &mut self,
        record: R,
        before: Option<R>,
        stored: Option<Stored>,
        extras: Map<String, Value>,
        details: Map<String, Value>,
    ) {
        self.dirty = true;
        let overlay = R::overlay_mut(self);
        let key = record.key();
        overlay.remember(&record);
        overlay.fetched.remove(&key);
        match overlay.rows.get_mut(&key) {
            Some(staged) => {
                staged.extra |= !extras.is_empty() || !details.is_empty();
                staged.current = Some(record);
                staged.extras.extend(extras);
                staged.details.extend(details);
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
                        extra: !extras.is_empty() || !details.is_empty(),
                        extras,
                        details,
                        dirty: true,
                    },
                );
            }
        }
    }

    /// Records that an observation saw a row. A walk keeps it as seen; a write outside a walk
    /// keeps every open walk of the row's source from changing it.
    pub(crate) fn see<R: Resident>(&mut self, record: &R) {
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
        };
        Ok(self.insert(vec![(source, Map::new())]).await?.remove(0).key)
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

    /// Names a media URI whose favorite or rating plain SQL may have changed.
    pub(crate) fn touch_state(&mut self, uri: &str) {
        self.touched.states.insert(uri.to_owned());
    }

    /// Names a media URI whose listens or carried-over counts plain SQL may have changed.
    pub(crate) fn touch_listened(&mut self, uri: &str) {
        self.touched.listened.insert(uri.to_owned());
    }

    /// Names a media URI whose downloaded copy plain SQL may have added or removed.
    pub(crate) fn touch_download(&mut self, uri: &str) {
        self.touched.downloads.insert(uri.to_owned());
    }

    pub(crate) fn touch_playlist(&mut self, key: i64) {
        self.touched.playlists.insert(key);
    }

    pub(crate) fn touch_smart_playlist(&mut self, key: i64) {
        self.touched.smart.insert(key);
    }

    pub(crate) fn touch_playlist_folders(&mut self) {
        self.touched.folders = true;
    }

    /// The open transaction for plain SQL and Store reads. Staged rows are written first, so
    /// the SQL sees them. Plain SQL never writes a resident table; it touches user data instead.
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
        for (table, rows) in writes.updates {
            let columns = table
                .columns
                .iter()
                .filter(|column| rows.iter().any(|row| row.get(column.name).is_some()))
                .collect::<Vec<_>>();
            if !columns.is_empty() {
                execute(connection, &families::update(table, &columns), rows).await?;
            }
        }
        persist_details(connection, &families::TRACK_DETAILS, writes.details).await?;
        for ((table, owner), rows) in writes.run_deletes {
            execute(connection, &families::run_delete(table, owner), rows).await?;
        }
        for ((table, owner), rows) in writes.run_inserts {
            execute(connection, &families::run_insert(table, owner), rows).await?;
        }
        for (table, keys) in writes.deletes.into_iter().rev() {
            // Links name their targets without a foreign key, so they go with them.
            let targets = match table.name {
                "artists" => Some("0,1"),
                "tags" => Some("2,3"),
                "folders" => Some("4"),
                _ => None,
            };
            if let Some(roles) = targets {
                let input = Value::Array(keys.clone()).to_string();
                for links in ["track_links", "album_links"] {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "DELETE FROM {links} WHERE role IN({roles}) AND target IN(SELECT value FROM json_each(?1))"
                    )))
                    .bind(&input)
                    .execute(&mut *connection)
                    .await?;
                }
            }
            execute(connection, &families::statements(table).delete, keys).await?;
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
            self.user_changes().await?
        };
        let mut scopes = self.scopes(&user);
        if self.reload && !scopes.contains(&Scope::Reload) {
            scopes.insert(0, Scope::Reload);
        }
        // A commit of rows memory does not hold leaves the Catalog, its revision and the image
        // as they are.
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
            self.catalog.reload_under(&mut self.writer).await?
        } else if resident {
            self.apply(user)
        } else {
            self.catalog.snapshot().revision
        };
        self.stage = Stage::Done;
        if resident {
            self.catalog.committed();
        }
        self.catalog
            .record_seen(self.walk, std::mem::take(&mut self.seen));
        if scopes.is_empty() {
            return Ok(None);
        }
        let change = Arc::new(Change { revision, scopes });
        self.catalog.publish(Arc::clone(&change));
        Ok(Some(change))
    }

    /// Reads back the user data operations touched, and the user data of rows this Write
    /// inserted or moved to another URI, and keeps what differs from the Catalog.
    async fn user_changes(&mut self) -> LibraryResult<UserChanges> {
        let mut touched = std::mem::take(&mut self.touched);
        for key in &self.tracks.order {
            let staged = &self.tracks.rows[key];
            if let Some(current) = &staged.current
                && staged.before.as_ref().is_none_or(|before| {
                    before.uri != current.uri
                        || before.source != current.source
                        || before.object != current.object
                })
            {
                touched.states.insert(current.uri.clone());
                touched.listened.insert(current.uri.clone());
                touched.downloads.insert(current.uri.clone());
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
            touched.states.insert(uri.clone());
        }
        let connection = &mut *self.writer;
        let catalog = self.catalog;
        let mut changes = UserChanges::default();
        if !touched.states.is_empty() {
            for (item, state, uri) in read_states(connection, &touched.states).await? {
                // A row this Write inserted holds no user state yet.
                let resident = catalog.snapshot().user_state(item).unwrap_or_default();
                if resident != state {
                    changes.states.push((item, state, uri));
                }
            }
        }
        if !touched.listened.is_empty() {
            for (key, stats) in read_listened(connection, &touched.listened).await? {
                if catalog.snapshot().listened(key) != stats {
                    changes.listened.push((key, stats));
                }
            }
        }
        if !touched.downloads.is_empty() {
            for (key, downloaded, uri) in read_downloads(connection, &touched.downloads).await? {
                if catalog.snapshot().downloaded(key).unwrap_or(false) != downloaded {
                    changes.downloads.push((key, downloaded, uri));
                }
            }
        }
        for key in &touched.playlists {
            let playlist = read_playlist(connection, *key).await?;
            let resident = catalog.snapshot().playlist(*key);
            if playlist != resident {
                let entries = playlist.as_ref().map(|playlist| &playlist.entries)
                    != resident.as_ref().map(|playlist| &playlist.entries);
                changes.playlists.push((*key, playlist, entries));
            }
        }
        for key in &touched.smart {
            let smart = read_smart_playlist(connection, *key).await?;
            if smart.as_ref() != catalog.snapshot().smart_playlists.get(key) {
                changes.smart.push((*key, smart));
            }
        }
        if touched.folders {
            let folders = read_playlist_folders(connection).await?;
            if folders != catalog.snapshot().playlist_folders {
                changes.folders = Some(folders);
            }
        }
        Ok(changes)
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
        let mut tracks = BTreeMap::<Option<SourceKey>, Vec<TrackKey>>::new();
        let mut played = Vec::new();
        for key in &self.tracks.order {
            let staged = &self.tracks.rows[key];
            if !staged.changed() {
                continue;
            }
            if staged.played() {
                played.push(TrackKey::from_raw(*key));
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
            for source in sources {
                tracks
                    .entry(source)
                    .or_default()
                    .push(TrackKey::from_raw(*key));
            }
        }
        scopes.extend(
            tracks
                .into_iter()
                .map(|(source, keys)| Scope::Tracks { source, keys }),
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
        if !user.states.is_empty() {
            let mut uris = user
                .states
                .iter()
                .map(|(_, _, uri)| uri.clone())
                .collect::<Vec<_>>();
            uris.dedup();
            scopes.push(Scope::State { uris });
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
                uris: user
                    .downloads
                    .iter()
                    .map(|(_, _, uri)| uri.clone())
                    .collect(),
            });
        }
        let mut listed = false;
        for (key, playlist, entries) in &user.playlists {
            let resident = snapshot.playlist(*key);
            listed |= playlist.is_none()
                || resident.is_none()
                || playlist
                    .as_ref()
                    .zip(resident.as_ref())
                    .is_some_and(|(next, before)| {
                        (&next.name, next.folder, next.indexed, next.created)
                            != (&before.name, before.folder, before.indexed, before.created)
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
        for (key, playlist, _) in user.playlists {
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
        if facts {
            snapshot.facts = snapshot.revision;
        }
        snapshot.revision
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

fn collect_sources<R: Resident>(
    overlay: &Overlay<R>,
    snapshot: &Snapshot,
    sources: &mut BTreeSet<SourceKey>,
) {
    for key in &overlay.order {
        let staged = &overlay.rows[key];
        // Plays the source reports for an album or artist follow the plays of its tracks.
        if !staged.changed() || staged.played() {
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

/// The statements one flush runs, grouped so each runs once per page of rows.
#[derive(Default)]
struct Writes {
    updates: Vec<(&'static families::Table, Vec<Value>)>,
    details: Vec<Value>,
    run_deletes: BTreeMap<(&'static str, &'static str), Vec<Value>>,
    run_inserts: BTreeMap<(&'static str, &'static str), Vec<Value>>,
    deletes: Vec<(&'static families::Table, Vec<Value>)>,
}

fn link_rows(run: &Run, owner: i64, items: &[i64]) -> Vec<Value> {
    items
        .iter()
        .enumerate()
        .map(|(position, target)| serde_json::json!([owner, run.role, position, target]))
        .collect()
}

/// The columns of `next` that differ from `stored`.
fn differing(stored: Option<&Map<String, Value>>, next: Map<String, Value>) -> Map<String, Value> {
    next.into_iter()
        .filter(|(name, value)| stored.and_then(|stored| stored.get(name)) != Some(value))
        .collect()
}

fn persist<R: Resident>(overlay: &mut Overlay<R>, writes: &mut Writes) {
    let table = R::TABLE;
    for key in &overlay.order {
        let staged = overlay.rows.get_mut(key).expect("staged keys have rows");
        if !std::mem::take(&mut staged.dirty) {
            continue;
        }
        let Some(record) = &staged.current else {
            if staged.stored.take().is_some() {
                let key = Value::from(*key);
                match writes
                    .deletes
                    .iter_mut()
                    .find(|(known, _)| std::ptr::eq(*known, table))
                {
                    Some((_, keys)) => keys.push(key),
                    None => writes.deletes.push((table, vec![key])),
                }
            }
            continue;
        };
        let stored = staged.stored.take();
        let mut columns = differing(
            stored.as_ref().map(|stored| &stored.columns),
            record.columns(),
        );
        columns.extend(std::mem::take(&mut staged.extras));
        if !columns.is_empty() {
            columns.insert(table.key.into(), Value::from(*key));
            match writes
                .updates
                .iter_mut()
                .find(|(known, _)| std::ptr::eq(*known, table))
            {
                Some((_, rows)) => rows.push(Value::Object(columns)),
                None => writes.updates.push((table, vec![Value::Object(columns)])),
            }
        }
        if let Some(details) = record.details() {
            let mut details = differing(
                stored.as_ref().and_then(|stored| stored.details.as_ref()),
                details,
            );
            details.extend(std::mem::take(&mut staged.details));
            if !details.is_empty() {
                details.insert(table.key.into(), Value::from(*key));
                writes.details.push(Value::Object(details));
            }
        }
        for (run, items) in record.runs() {
            let known = stored.as_ref().map(|stored| {
                stored
                    .runs
                    .iter()
                    .find(|(other, _)| std::ptr::eq(*other, run))
                    .map_or(&[][..], |(_, items)| items.as_slice())
            });
            if known == Some(items.as_slice()) {
                continue;
            }
            if known.is_none_or(|known| !known.is_empty()) {
                writes
                    .run_deletes
                    .entry((run.table, run.owner))
                    .or_default()
                    .push(serde_json::json!([key, run.role]));
            }
            writes
                .run_inserts
                .entry((run.table, run.owner))
                .or_default()
                .extend(link_rows(run, *key, &items));
        }
        staged.stored = Some(Stored::of(record));
    }
}

async fn persist_details(
    connection: &mut SqliteConnection,
    table: &'static families::Table,
    rows: Vec<Value>,
) -> LibraryResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    execute(
        connection,
        &families::statements(table).insert,
        rows.clone(),
    )
    .await?;
    let columns = table
        .columns
        .iter()
        .filter(|column| rows.iter().any(|row| row.get(column.name).is_some()))
        .collect::<Vec<_>>();
    execute(connection, &families::update(table, &columns), rows).await
}

async fn execute(
    connection: &mut SqliteConnection,
    statement: &str,
    rows: Vec<Value>,
) -> LibraryResult<()> {
    for page in rows.chunks(PERSIST_ROWS) {
        sqlx::query(sqlx::AssertSqlSafe(statement.to_owned()))
            .bind(Value::Array(page.to_vec()).to_string())
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
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

    fn family<R: Resident>(&mut self, overlay: &Overlay<R>) {
        for key in &overlay.order {
            R::apply(self.snapshot(), *key, overlay.rows[key].current.as_ref());
        }
    }
}

/// Favorites and ratings of tracks, albums and artists with these URIs.
async fn read_states(
    connection: &mut SqliteConnection,
    uris: &BTreeSet<String>,
) -> LibraryResult<Vec<(UserItem, UserState, String)>> {
    let mut states = Vec::new();
    for page in uris.iter().collect::<Vec<_>>().chunks(READ_KEYS) {
        let rows: Vec<(String, i64, String, Option<bool>, Option<i64>)> = sqlx::query_as(
            "WITH input AS(SELECT value uri FROM json_each(?1)),
             items AS(SELECT 'track' kind,track_key key,media_uri FROM tracks WHERE media_uri IN(SELECT uri FROM input)
                UNION ALL SELECT 'album',album_key,media_uri FROM albums WHERE media_uri IN(SELECT uri FROM input)
                UNION ALL SELECT 'artist',artist_key,media_uri FROM artists WHERE media_uri IN(SELECT uri FROM input))
             SELECT items.kind,items.key,items.media_uri,state.favorite,state.rating FROM items LEFT JOIN user_media_state state USING(media_uri)",
        )
        .bind(serde_json::to_string(page)?)
        .fetch_all(&mut *connection)
        .await?;
        for (kind, key, uri, favorite, rating) in rows {
            let item = match kind.as_str() {
                "track" => UserItem::Track(key),
                "album" => UserItem::Album(key),
                _ => UserItem::Artist(key),
            };
            states.push((item, user_state(favorite, rating), uri));
        }
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
                (SELECT count(*) FROM listens listen WHERE listen.media_uri=track.media_uri) plays,
                (SELECT count(*) FROM listens listen WHERE listen.media_uri=track.media_uri AND listen.skipped=1) skips,
                (SELECT max(started_at) FROM listens listen WHERE listen.media_uri=track.media_uri) last,
                legacy.play_count legacy_plays,legacy.skip_count legacy_skips,legacy.last_played_at legacy_last
            FROM tracks track LEFT JOIN sources source USING(source_key)
            LEFT JOIN legacy_activity legacy ON legacy.source_id=source.object_id AND legacy.track_object_id=track.object_id AND legacy.period='lifetime' AND legacy.item_kind='track'
            WHERE {condition})"
    )
}

async fn read_listened(
    connection: &mut SqliteConnection,
    uris: &BTreeSet<String>,
) -> LibraryResult<Vec<(TrackKey, Stats)>> {
    let mut stats = Vec::new();
    for page in uris.iter().collect::<Vec<_>>().chunks(READ_KEYS) {
        let rows: Vec<(i64, i64, i64, Option<i64>)> = sqlx::query_as(sqlx::AssertSqlSafe(
            listened_sql("track.media_uri IN(SELECT value FROM json_each(?1))"),
        ))
        .bind(serde_json::to_string(page)?)
        .fetch_all(&mut *connection)
        .await?;
        stats.extend(rows.into_iter().map(|(key, plays, skips, last)| {
            (TrackKey::from_raw(key), Stats::listened(plays, skips, last))
        }));
    }
    Ok(stats)
}

async fn read_downloads(
    connection: &mut SqliteConnection,
    uris: &BTreeSet<String>,
) -> LibraryResult<Vec<(TrackKey, bool, String)>> {
    let mut downloads = Vec::new();
    for page in uris.iter().collect::<Vec<_>>().chunks(READ_KEYS) {
        let rows: Vec<(i64, bool, String)> = sqlx::query_as("SELECT track.track_key,EXISTS(SELECT 1 FROM local_locators locator WHERE locator.media_uri=track.media_uri AND locator.origin='download'),track.media_uri FROM tracks track WHERE track.media_uri IN(SELECT value FROM json_each(?1))")
            .bind(serde_json::to_string(page)?)
            .fetch_all(&mut *connection)
            .await?;
        downloads.extend(
            rows.into_iter()
                .map(|(key, downloaded, uri)| (TrackKey::from_raw(key), downloaded, uri)),
        );
    }
    Ok(downloads)
}

/// The playlist header columns the Catalog holds, in load order.
pub(crate) const PLAYLIST_COLUMNS: &str = "playlist_key,source_key,owner='source',indexed,coalesce(name,''),playlist_folder_key,created_at,writable,metadata_writable";

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
);

pub(crate) fn playlist(row: PlaylistRow, entries: Vec<(i64, TrackKey)>) -> Playlist {
    let (key, source, owned_by_source, indexed, name, folder, created, writable, metadata_writable) =
        row;
    Playlist {
        key,
        source: source.map(SourceKey::from_raw),
        owned_by_source,
        indexed,
        name,
        folder,
        created,
        writable,
        metadata_writable,
        entries,
    }
}

async fn read_playlist(
    connection: &mut SqliteConnection,
    key: i64,
) -> LibraryResult<Option<Playlist>> {
    let row: Option<PlaylistRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {PLAYLIST_COLUMNS} FROM playlists WHERE playlist_key=?1"
    )))
    .bind(key)
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let entries: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT playlist_entry_key,track_key FROM playlist_entries WHERE playlist_key=?1 ORDER BY position",
    )
    .bind(key)
    .fetch_all(&mut *connection)
    .await?;
    Ok(Some(playlist(
        row,
        entries
            .into_iter()
            .map(|(entry, track)| (entry, TrackKey::from_raw(track)))
            .collect(),
    )))
}

pub(crate) const SMART_COLUMNS: &str =
    "smart_playlist_key,name,playlist_folder_key,position,definition_json";

pub(crate) fn smart_playlist(
    (key, name, folder, position, definition): (i64, String, Option<i64>, i64, String),
) -> SmartPlaylist {
    SmartPlaylist {
        key,
        name,
        folder,
        position,
        definition: serde_json::from_str(&definition).unwrap_or_else(|error| {
            tracing::warn!(%error, key, "smart playlist has an unreadable definition");
            crate::SmartPlaylistDefinition::default()
        }),
    }
}

async fn read_smart_playlist(
    connection: &mut SqliteConnection,
    key: i64,
) -> LibraryResult<Option<SmartPlaylist>> {
    let row: Option<(i64, String, Option<i64>, i64, String)> = sqlx::query_as(sqlx::AssertSqlSafe(
        format!("SELECT {SMART_COLUMNS} FROM smart_playlists WHERE smart_playlist_key=?1"),
    ))
    .bind(key)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(row.map(smart_playlist))
}

pub(crate) async fn read_playlist_folders(
    connection: &mut SqliteConnection,
) -> LibraryResult<BTreeMap<i64, PlaylistFolder>> {
    let rows: Vec<(i64, String, String, bool)> =
        sqlx::query_as("SELECT folder_key,object_id,name,smart FROM playlist_folders")
            .fetch_all(&mut *connection)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(key, object, name, smart)| {
            (
                key,
                PlaylistFolder {
                    key,
                    object,
                    name,
                    smart,
                },
            )
        })
        .collect())
}

macro_rules! overlay {
    ($field:ident) => {
        fn overlay<'b>(write: &'b Write<'_>) -> &'b Overlay<Self> {
            &write.$field
        }

        fn overlay_mut<'b>(write: &'b mut Write<'_>) -> &'b mut Overlay<Self> {
            &mut write.$field
        }
    };
}

impl Resident for Source {
    const FAMILY: Family = Family::Source;
    overlay!(sources);

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

impl Resident for Track {
    const FAMILY: Family = Family::Track;
    overlay!(tracks);

    fn links(&mut self, snapshot: &Snapshot) {
        if let Some(record) = snapshot.tracks.get(self.key.raw()) {
            let links = snapshot.track_links(record);
            self.artists = links.artists;
            self.album_artists = links.album_artists;
            self.tags = links.tags;
            self.folders = links.folders;
        }
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_track(record),
            None => snapshot.remove_track(TrackKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        use super::snapshot::Packed;
        snapshot
            .tracks
            .get(key)
            .and_then(|record| snapshot.source_key(record.source()))
    }

    /// A track is hidden from when it stops being listed, or from its insertion when nothing
    /// lists it, so the expiry can forget it.
    fn normalize(&mut self, before: Option<&Self>, _extras: &Map<String, Value>) {
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
}

impl Resident for Album {
    const FAMILY: Family = Family::Album;
    overlay!(albums);

    fn links(&mut self, snapshot: &Snapshot) {
        if let Some(record) = snapshot.albums.get(self.key.raw()) {
            (self.artists, self.genres) = snapshot.album_links(record);
        }
    }

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_album(record),
            None => snapshot.remove_album(crate::AlbumKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        use super::snapshot::Packed;
        snapshot
            .albums
            .get(key)
            .and_then(|record| snapshot.source_key(record.source()))
    }

    fn normalize(&mut self, _before: Option<&Self>, extras: &Map<String, Value>) {
        if let Some(input) = extras.get("input_metadata") {
            self.input = !input.is_null();
        }
    }

    fn state(&mut self) -> Option<&mut NativeState> {
        Some(&mut self.state)
    }
}

impl Resident for Artist {
    const FAMILY: Family = Family::Artist;
    overlay!(artists);

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_artist(record),
            None => snapshot.remove_artist(crate::ArtistKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        use super::snapshot::Packed;
        snapshot
            .artists
            .get(key)
            .and_then(|record| snapshot.source_key(record.source()))
    }

    fn state(&mut self) -> Option<&mut NativeState> {
        Some(&mut self.state)
    }
}

impl Resident for Tag {
    const FAMILY: Family = Family::Tag;
    overlay!(tags);

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_tag(record),
            None => snapshot.remove_tag(key),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        use super::snapshot::Packed;
        snapshot
            .tags
            .get(key)
            .and_then(|record| snapshot.source_key(record.source()))
    }
}

impl Resident for Folder {
    const FAMILY: Family = Family::Folder;
    overlay!(folders);

    fn apply(snapshot: &mut Snapshot, key: i64, record: Option<&Self>) {
        match record {
            Some(record) => snapshot.put_folder(record),
            None => snapshot.remove_folder(crate::FolderKey::from_raw(key)),
        }
    }

    fn resident_source(snapshot: &Snapshot, key: i64) -> Option<SourceKey> {
        use super::snapshot::Packed;
        snapshot
            .folders
            .get(key)
            .and_then(|record| snapshot.source_key(record.source()))
    }
}

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
            .insert(vec![(
                Source {
                    key: SourceKey::from_raw(0),
                    object: SOURCE.into(),
                    kind: Some("local".into()),
                    name: "Local".into(),
                    complete: false,
                    freshness: None,
                },
                Map::new(),
            )])
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
            artists.push((artist, Map::new()));
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
            tags.push((tag, Map::new()));
        }
        let tags = write.insert(tags).await.unwrap();
        let mut folder = Folder::new(source.key, "music", uri("folder", "music"));
        folder.indexed = true;
        folder.native = true;
        folder.metadata = FolderMetadata {
            name: Some("Music".into()),
            sort_text: None,
        };
        let folder = write
            .insert(vec![(folder, Map::new())])
            .await
            .unwrap()
            .remove(0);
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
        let album = write
            .insert(vec![(album, Map::new())])
            .await
            .unwrap()
            .remove(0);
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
            inserted.push((
                record,
                super::super::merge::extras([("metadata", serde_json::json!({"bpm_source":1}))]),
            ));
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
        write.put(unchanged, Map::new());
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
        write.put(one.clone(), Map::new());
        let mut two = write
            .get::<Track>(tracks[1].key.raw())
            .await
            .unwrap()
            .unwrap();
        two.indexed = false;
        write.put(two, Map::new());
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

        // User data written with plain SQL and read back by key.
        let target = FavoriteTarget::Track(one.uri.clone());
        catalog.set_favorite(&target, true).await.unwrap();
        catalog.set_rating(&target, Some(8)).await.unwrap();
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
        let mut write = catalog.write().await.unwrap();
        crate::local::write_local_locator(
            &mut write,
            &crate::LocalLocatorWrite {
                source_id: None,
                media_uri: one.uri.clone(),
                origin: "download".into(),
                path: "/downloads/one.flac".into(),
                root: "/downloads".into(),
                relative_path: "one.flac".into(),
                access_uri: "file:///downloads/one.flac".into(),
            },
        )
        .await
        .unwrap();
        write.commit().await.unwrap();
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
