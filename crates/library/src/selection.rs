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
fn in_ranges<T: Copy>(order: &[T], ranges: Option<&[Range<usize>]>) -> Vec<T> {
    crate::query::selection_ranges(ranges)
        .into_iter()
        .flat_map(|range| &order[range.start.min(order.len())..range.end.min(order.len())])
        .copied()
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
                if let Collection::History { source } = &read.collection {
                    let order = Catalog::history(
                        connection,
                        source.as_ref(),
                        &read.filter,
                        read.descending,
                    )
                    .await?;
                    rows.extend(
                        in_ranges(&order, ranges.as_deref())
                            .into_iter()
                            .map(|(key, _)| (key, None)),
                    );
                } else {
                    let order = catalog.member_order(connection, read).await?;
                    rows.extend(catalog.member_keys(&in_ranges(&order, ranges.as_deref())));
                }
            }
            MemberSelection::Parents { read, ranges } => {
                let order = catalog.list_order(connection, read).await?;
                let parents = in_ranges(&order, ranges.as_deref());
                let members = catalog.parent_order(connection, read, &parents).await?;
                rows.extend(catalog.member_keys(&members));
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
                    let read = ListRead::new(ListKind::Albums, None, CatalogSort::TrackNumber);
                    let members = catalog.parent_order(connection, &read, &albums).await?;
                    rows.extend(catalog.member_keys(&members));
                    continue;
                }
                for collection in collections {
                    let order = catalog
                        .member_order(connection, &MemberRead::new(collection.clone()))
                        .await?;
                    rows.extend(catalog.member_keys(&order));
                }
            }
            MemberSelection::Keys { source, keys } => {
                let found: Vec<TrackKey> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value WHERE ?2 IS NULL OR track.source_key=?2 ORDER BY CAST(requested.key AS INTEGER)")
                    .bind(serde_json::to_string(keys.as_ref())?)
                    .bind(source)
                    .fetch_all(&mut *connection)
                    .await?;
                rows.extend(found.into_iter().map(|key| (key, None)));
            }
            MemberSelection::Uris(uris) => {
                let found: Vec<TrackKey> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)")
                    .bind(serde_json::to_string(uris.as_ref())?)
                    .fetch_all(&mut *connection)
                    .await?;
                rows.extend(found.into_iter().map(|key| (key, None)));
            }
            MemberSelection::Objects { source, ids } => {
                let found: Vec<TrackKey> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested JOIN tracks track ON track.source_key=?2 AND track.object_id=requested.value ORDER BY CAST(requested.key AS INTEGER)")
                    .bind(serde_json::to_string(ids.as_ref())?)
                    .bind(source)
                    .fetch_all(&mut *connection)
                    .await?;
                rows.extend(found.into_iter().map(|key| (key, None)));
            }
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
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let order = self.list_order(&mut connection, read).await?;
        Ok(in_ranges(&order, Some(ranges))
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
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
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
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
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
