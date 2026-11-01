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
    Parents {
        read: ListRead,
        ranges: Option<Vec<Range<usize>>>,
    },
    Collections(Vec<Collection>),
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

/// The keys of a list order's selected rows; playlist folders are negative.
fn parents(order: &Order, ranges: Option<&[Range<usize>]>) -> Vec<i64> {
    in_ranges(order, ranges)
        .into_iter()
        .filter_map(|item| {
            Some(match item {
                Item::Album(key) | Item::AlbumHeading(key) => key.raw(),
                Item::Artist { key, .. } => key.raw(),
                Item::Genre(key) => key.raw(),
                Item::Mood(key) => key.raw(),
                Item::Playlist(key) => key.raw(),
                Item::Smart(key) => key.raw(),
                Item::PlaylistFolder(key) => -key.raw(),
                _ => return None,
            })
        })
        .collect()
}

/// The tracks a selection names, in order, with the playlist entry each comes from.
pub(crate) async fn selected_tracks_on(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    selection: &MemberSelection,
) -> LibraryResult<Vec<(TrackKey, Option<PlaylistEntryKey>)>> {
    let mut pending = vec![selection];
    let mut rows = Vec::new();
    while let Some(next) = pending.pop() {
        match next {
            MemberSelection::Concat(selections) => pending.extend(selections.iter().rev()),
            MemberSelection::Members { read, ranges } => {
                let order = catalog
                    .order_on(connection, &Read::Members(read.clone()))
                    .await?;
                rows.extend(
                    in_ranges(&order, ranges.as_deref()).into_iter().filter_map(
                        |item| match item {
                            Item::Track { key, entry } => Some((key, entry)),
                            Item::Listen(index) => Some((order.listen_track(index)?, None)),
                            _ => None,
                        },
                    ),
                );
            }
            MemberSelection::Parents { read, ranges } => {
                let order = catalog
                    .order_on(connection, &Read::List(read.clone()))
                    .await?;
                let parents = parents(&order, ranges.as_deref());
                rows.extend(catalog.parent_order(connection, read, &parents).await?);
            }
            MemberSelection::Collections(collections) => {
                if collections
                    .iter()
                    .all(|collection| matches!(collection, Collection::Album(_)))
                {
                    let albums = collections
                        .iter()
                        .map(|collection| match collection {
                            Collection::Album(key) => key.raw(),
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>();
                    let read = ListRead::new(ListKind::Albums, None, LibraryField::TrackNumber);
                    rows.extend(catalog.parent_order(connection, &read, &albums).await?);
                    continue;
                }
                for collection in collections {
                    rows.extend(
                        catalog
                            .members(connection, MemberRead::new(collection.clone()))
                            .await?
                            .into_iter()
                            .map(|key| (key, None)),
                    );
                }
            }
            MemberSelection::Keys { source, keys } => {
                let snapshot = catalog.snapshot();
                let slot = source.map(|source| snapshot.source_slot(source));
                rows.extend(
                    keys.iter()
                        .filter(|key| {
                            snapshot.tracks.get(key.raw()).is_some_and(|record| {
                                slot.is_none_or(|slot| slot == Some(record.source))
                            })
                        })
                        .map(|key| (*key, None)),
                );
            }
            MemberSelection::Uris(uris) => rows.extend(
                crate::read::track_keys_on(connection, uris)
                    .await?
                    .into_iter()
                    .map(|key| (key, None)),
            ),
            MemberSelection::Objects { source, ids } => rows.extend(
                catalog
                    .track_keys_by_objects(connection, Some(*source), ids)
                    .await?
                    .into_iter()
                    .map(|key| (TrackKey::from_raw(key), None)),
            ),
            MemberSelection::Entries(entries) => {
                let found: Vec<(TrackKey, PlaylistEntryKey)> = sqlx::query_as("SELECT entry.track_key,entry.playlist_entry_key FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
                    .bind(serde_json::to_string(entries.as_ref())?)
                    .fetch_all(&mut *connection)
                    .await?;
                rows.extend(found.into_iter().map(|(key, entry)| (key, Some(entry))));
            }
        }
    }
    Ok(rows)
}

fn parent_collection(read: &ListRead, key: i64) -> Collection {
    match read.kind {
        ListKind::Albums | ListKind::ArtistAlbums { .. } => {
            Collection::Album(AlbumKey::from_raw(key))
        }
        ListKind::Artists { album_artist } => Collection::Artist {
            key: ArtistKey::from_raw(key),
            album_artist,
        },
        ListKind::Genres => Collection::Genre(GenreKey::from_raw(key)),
        ListKind::Moods => Collection::Mood(MoodKey::from_raw(key)),
        ListKind::Playlists => Collection::Playlist(PlaylistKey::from_raw(key)),
        ListKind::SmartPlaylists { now } => Collection::Smart {
            key: SmartPlaylistKey::from_raw(key),
            now,
        },
    }
}

impl Catalog {
    pub async fn selected_collections(
        &self,
        read: &ListRead,
        ranges: &[Range<usize>],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Collection>> {
        let order = self.order(&Read::List(read.clone()), cancellation).await?;
        Ok(parents(&order, Some(ranges))
            .into_iter()
            .filter(|key| read.playlist_scope == PlaylistScope::All || *key >= 0)
            .map(|key| parent_collection(read, key))
            .collect())
    }

    pub async fn selected_track_keys(
        &self,
        selection: &MemberSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        Ok(selected_tracks_on(self, &mut connection, selection)
            .await?
            .into_iter()
            .map(|(key, _)| key)
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
        let mut keys = selected_tracks_on(self, &mut connection, selection)
            .await?
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>();
        keys.truncate(limit.unwrap_or(usize::MAX));
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(&mut *connection)
            .await?)
    }
}
