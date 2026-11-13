//! Accepted member selections, including parent ranges and playlist occurrences. Ordered
//! selections come from the resident Catalog; explicit keys, URIs, objects and entries are
//! read on the caller's connection, so a Write sees the rows it added.
use crate::*;
use sqlx::SqliteConnection;
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub enum MemberSelection {
    Members {
        read: MemberRead,
        ranges: Option<Vec<Range<usize>>>,
    },
    /// Selected rows of a list, each row's collection read as `members` reads, its collection
    /// replaced; a smart playlist's as `smart` reads, when given. A playlist folder's row names
    /// its members, as its page lists them.
    Parents {
        read: ListRead,
        ranges: Option<Vec<Range<usize>>>,
        members: Box<MemberRead>,
        smart: Option<Box<MemberRead>>,
    },
    Keys {
        source: Option<SourceKey>,
        keys: Arc<[TrackKey]>,
    },
    Uris(Arc<[String]>),
    Entries(Arc<[PlaylistEntryKey]>),
    Objects {
        source: SourceKey,
        ids: Arc<[String]>,
    },
    Concat(Vec<MemberSelection>),
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl MemberSelection {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn from_uris(uris: Vec<String>) -> Arc<Self> {
        Arc::new(Self::Uris(uris.into()))
    }
}

/// The items of selected ranges of an order.
fn in_ranges(order: &Order, ranges: Option<&[Range<usize>]>) -> Vec<Item> {
    crate::query::selection_ranges(ranges)
        .into_iter()
        .flat_map(|range| order.items(range))
        .collect()
}

/// The collection a list row names; None for a playlist folder, whose members its page
/// lists.
fn parent(read: &ListRead, item: Item) -> Option<Collection> {
    Some(match item {
        Item::Album(key) | Item::AlbumHeading(key) => Collection::Album(key),
        Item::Artist { key, album_artist } => Collection::Artist { key, album_artist },
        Item::Genre(key) => Collection::Genre(key),
        Item::Mood(key) => Collection::Mood(key),
        Item::Playlist(key) => Collection::Playlist(key),
        Item::Smart(key) => Collection::Smart {
            key,
            now: read.kind.now()?,
        },
        _ => return None,
    })
}

/// The tracks of an order's selected ranges, with the playlist entry each comes from.
pub(crate) fn order_tracks(
    order: &Order,
    ranges: Option<&[Range<usize>]>,
) -> Vec<(TrackKey, Option<PlaylistEntryKey>)> {
    in_ranges(order, ranges)
        .into_iter()
        .filter_map(|item| match item {
            Item::Track { key, entry } => Some((key, entry)),
            Item::Listen(index) => Some((order.listen_track(index)?, None)),
            _ => None,
        })
        .collect()
}

/// The tracks each selection names, in order, with the playlist entry each comes from. The
/// whole collections they name are read together, from one pass over the catalog.
pub(crate) async fn selected_tracks_on(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    selections: &[&MemberSelection],
) -> LibraryResult<Vec<Vec<(TrackKey, Option<PlaylistEntryKey>)>>> {
    enum Part {
        Rows(Vec<(TrackKey, Option<PlaylistEntryKey>)>),
        Read(usize),
    }
    let mut reads = Vec::new();
    let mut parts = Vec::new();
    for (group, selection) in selections.iter().enumerate() {
        let mut pending = vec![*selection];
        while let Some(next) = pending.pop() {
            let rows = match next {
                MemberSelection::Concat(selections) => {
                    pending.extend(selections.iter().rev());
                    continue;
                }
                MemberSelection::Members { read, ranges: None } => {
                    parts.push((group, Part::Read(reads.len())));
                    reads.push(read.clone());
                    continue;
                }
                MemberSelection::Members { read, ranges } => {
                    let order = catalog
                        .order_on(connection, &Read::Members(read.clone()))
                        .await?;
                    order_tracks(&order, ranges.as_deref())
                }
                MemberSelection::Parents {
                    read,
                    ranges,
                    members,
                    smart,
                } => {
                    let order = catalog
                        .order_on(connection, &Read::List(read.clone()))
                        .await?;
                    let mut items = Vec::new();
                    for item in in_ranges(&order, ranges.as_deref()) {
                        let Item::PlaylistFolder(key) = item else {
                            items.push(item);
                            continue;
                        };
                        let folder = ListRead {
                            playlist_scope: PlaylistScope::Folder { key },
                            filter: String::new(),
                            ..read.clone()
                        };
                        let order = catalog.order_on(connection, &Read::List(folder)).await?;
                        items.extend(order.items(0..order.len()));
                    }
                    for collection in items.into_iter().filter_map(|item| parent(read, item)) {
                        let members = match (&collection, smart) {
                            (Collection::Smart { .. }, Some(smart)) => smart,
                            _ => members,
                        };
                        parts.push((group, Part::Read(reads.len())));
                        reads.push(MemberRead {
                            collection,
                            ..(**members).clone()
                        });
                    }
                    continue;
                }
                MemberSelection::Keys { source, keys } => {
                    let snapshot = catalog.snapshot();
                    let slot = source.map(|source| snapshot.source_slot(source));
                    keys.iter()
                        .filter(|key| {
                            snapshot.tracks.get(key.raw()).is_some_and(|record| {
                                slot.is_none_or(|slot| slot == Some(record.source))
                            })
                        })
                        .map(|key| (*key, None))
                        .collect()
                }
                MemberSelection::Uris(uris) => crate::read::track_keys_on(connection, uris)
                    .await?
                    .into_iter()
                    .map(|key| (key, None))
                    .collect(),
                MemberSelection::Objects { source, ids } => catalog
                    .track_keys_by_objects(connection, Some(*source), ids)
                    .await?
                    .into_iter()
                    .map(|key| (TrackKey::from_raw(key), None))
                    .collect(),
                MemberSelection::Entries(entries) => {
                    let found: Vec<(TrackKey, PlaylistEntryKey)> = sqlx::query_as("SELECT entry.track_key,entry.playlist_entry_key FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
                        .bind(serde_json::to_string(entries.as_ref())?)
                        .fetch_all(&mut *connection)
                        .await?;
                    found
                        .into_iter()
                        .map(|(key, entry)| (key, Some(entry)))
                        .collect()
                }
            };
            parts.push((group, Part::Rows(rows)));
        }
    }
    let mut read = catalog.member_tracks_on(connection, &reads).await?;
    let mut groups = vec![Vec::new(); selections.len()];
    for (group, part) in parts {
        groups[group].extend(match part {
            Part::Rows(rows) => rows,
            Part::Read(index) => std::mem::take(&mut read[index]),
        });
    }
    Ok(groups)
}

impl Catalog {
    pub async fn selected_collections(
        &self,
        read: &ListRead,
        ranges: &[Range<usize>],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Collection>> {
        let order = self.order(&Read::List(read.clone()), cancellation).await?;
        Ok(in_ranges(&order, Some(ranges))
            .into_iter()
            .filter_map(|item| parent(read, item))
            .collect())
    }

    pub async fn selected_track_uris(
        &self,
        selection: &MemberSelection,
        limit: Option<usize>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let mut connection = self.acquire_loaded(cancellation).await?;
        let mut keys = selected_tracks_on(self, &mut connection, &[selection])
            .await?
            .into_iter()
            .flatten()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        keys.truncate(limit.unwrap_or(usize::MAX));
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(&mut *connection)
            .await?)
    }
}
