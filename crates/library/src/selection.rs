//! Accepted member selections, including parent ranges and playlist occurrences.
use crate::*;
use sqlx::{Connection, SqliteConnection};
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

pub(crate) async fn selected_tracks_on(
    connection: &mut SqliteConnection,
    selection: &MemberSelection,
) -> LibraryResult<Vec<(TrackKey, Option<PlaylistEntryKey>)>> {
    selection_rows_on(connection, selection, "track.track_key,$entry", None).await
}

async fn selection_rows_on<T>(
    connection: &mut SqliteConnection,
    selection: &MemberSelection,
    columns: &str,
    limit: Option<usize>,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    enum Pending<'a> {
        Selection(&'a MemberSelection),
        Collection(Collection, Option<FolderKey>),
    }
    let mut pending = vec![Pending::Selection(selection)];
    let mut rows = Vec::new();
    while let Some(next) = pending.pop() {
        let remaining = limit.map(|limit| limit.saturating_sub(rows.len()));
        if remaining == Some(0) {
            break;
        }
        let mut ranges = None;
        let query = match next {
            Pending::Collection(collection, folder) => {
                let mut read = MemberRead::new(collection);
                read.folder = folder;
                crate::query::member_query_on(connection, &read).await?
            }
            Pending::Selection(MemberSelection::Concat(selections)) => {
                pending.extend(selections.iter().rev().map(Pending::Selection));
                continue;
            }
            Pending::Selection(MemberSelection::Collections(collections)) => {
                if collections
                    .iter()
                    .all(|collection| matches!(collection, Collection::Album(_)))
                {
                    let keys = collections
                        .iter()
                        .map(|collection| match collection {
                            Collection::Album(key) => *key,
                            _ => unreachable!(),
                        })
                        .collect::<Vec<_>>();
                    let requested = crate::query::RequestedKeys::batch(&keys)?;
                    let query = parent_members(&ListKind::Albums, requested);
                    rows.extend(
                        crate::query::selected_rows_on(
                            connection,
                            &query,
                            &columns.replace("$entry", &query.entry_key),
                            None,
                            remaining,
                        )
                        .await?,
                    );
                    continue;
                }
                pending.extend(
                    collections
                        .iter()
                        .rev()
                        .cloned()
                        .map(|collection| Pending::Collection(collection, None)),
                );
                continue;
            }
            Pending::Selection(MemberSelection::Parents {
                read,
                ranges: parent_ranges,
            }) => {
                let query = crate::query::list_query_on(connection, read).await?;
                let requested = query.requested_ranges(parent_ranges.as_deref());
                if let ListKind::SmartPlaylists { now } = read.kind {
                    crate::smart_playlists::smart_parent_query(
                        connection,
                        read.source,
                        read.folder,
                        now,
                        requested,
                    )
                    .await?
                } else {
                    parent_members(&read.kind, requested)
                }
            }
            Pending::Selection(MemberSelection::Members {
                read,
                ranges: selected,
            }) => {
                ranges = selected.as_deref();
                crate::query::member_query_on(connection, read).await?
            }
            Pending::Selection(explicit) => explicit_query(explicit)?,
        };
        rows.extend(
            crate::query::selected_rows_on(
                connection,
                &query,
                &columns.replace("$entry", &query.entry_key),
                ranges,
                remaining,
            )
            .await?,
        );
    }
    Ok(rows)
}

fn parent_members(
    kind: &ListKind,
    requested: crate::query::RequestedKeys,
) -> crate::query::OrderedQuery {
    let (from,sort,entry,predicate)=match kind {
        ListKind::Albums|ListKind::ArtistAlbums { .. }=>("tracks track INDEXED BY tracks_album_order_idx ON track.album_key=parent.value".to_string(),CatalogSort::TrackNumber,"NULL","track.indexed=1"),
        ListKind::Artists { album_artist:false }=>("track_artists membership ON membership.artist_key=parent.value CROSS JOIN tracks track USING(track_key)".into(),CatalogSort::Title,"NULL","track.indexed=1"),
        ListKind::Artists { album_artist:true }=>(format!("tracks track ON track.track_key IN ({})",crate::collections::album_artist_keys("parent.value")),CatalogSort::Title,"NULL","track.indexed=1"),
        ListKind::Genres=>(format!("tracks track ON track.track_key IN ({})",crate::collections::genre_keys("parent.value")),CatalogSort::Title,"NULL","track.indexed=1"),
        ListKind::Moods=>("track_moods membership ON membership.mood_key=parent.value CROSS JOIN tracks track USING(track_key)".into(),CatalogSort::Title,"NULL","track.indexed=1"),
        ListKind::Playlists=>("playlist_entries entry ON entry.playlist_key=parent.value CROSS JOIN tracks track USING(track_key)".into(),CatalogSort::Position,"entry.playlist_entry_key","1"),
        ListKind::SmartPlaylists { .. }=>unreachable!(),
    };
    let mut order = vec!["parent.ordinal".into()];
    order.extend(sort.order_terms(false, matches!(kind, ListKind::Playlists)));
    crate::query::OrderedQuery {
        key: "track.track_key".into(),
        text: None,
        from: format!("({}) parent CROSS JOIN {from}", requested.sql),
        predicate: predicate.into(),
        order,
        entry_key: entry.into(),
        arguments: requested.arguments,
    }
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
            source: read.source,
            now,
        },
    }
}

fn explicit_query(selection: &MemberSelection) -> LibraryResult<crate::query::OrderedQuery> {
    use crate::query::{OrderedQuery, RequestedKeys};
    let (requested,from,predicate,entry_key) = match selection {
        MemberSelection::Keys { source,keys } => (RequestedKeys::batch(keys.as_ref())?,"tracks track ON track.track_key=requested.value".to_owned(),source.map_or_else(||"1".into(),|source|format!("track.source_key={}",source.raw())),"NULL"),
        MemberSelection::Uris(uris) => (RequestedKeys::batch(uris.as_ref())?,"tracks track ON track.media_uri=requested.value".into(),"1".into(),"NULL"),
        MemberSelection::Objects { source,ids } => (RequestedKeys::batch(ids.as_ref())?,format!("tracks track ON track.source_key={} AND track.object_id=requested.value",source.raw()),"1".into(),"NULL"),
        MemberSelection::Entries(keys) => (RequestedKeys::batch(keys.as_ref())?,"playlist_entries entry ON entry.playlist_entry_key=requested.value JOIN tracks track USING(track_key)".into(),"1".into(),"entry.playlist_entry_key"),
        _ => unreachable!(),
    };
    Ok(OrderedQuery {
        key: "track.track_key".into(),
        text: None,
        from: format!("({}) requested CROSS JOIN {from}", requested.sql),
        predicate,
        order: vec!["requested.ordinal".into()],
        entry_key: entry_key.into(),
        arguments: requested.arguments,
    })
}

impl Database {
    pub async fn selected_collections(
        &self,
        read: &ListRead,
        ranges: &[Range<usize>],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Collection>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let query = crate::query::list_query_on(&mut connection, read).await?;
        let rows: Vec<(i64,)> =
            crate::query::selected_rows_on(&mut connection, &query, &query.key, Some(ranges), None)
                .await?;
        Ok(rows
            .into_iter()
            .filter(|(key,)| read.playlist_scope == PlaylistScope::All || *key >= 0)
            .map(|(key,)| parent_collection(read, key))
            .collect())
    }

    pub async fn source_track_uri_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        after: Option<TrackKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<(TrackKey, String)>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as(
            "SELECT track_key,media_uri FROM tracks WHERE source_key=?1 AND indexed=1
             AND track_key>?2 AND (?3 IS NULL OR track_key IN
                 (SELECT track_key FROM track_folders WHERE folder_key=?3))
             ORDER BY track_key LIMIT 128",
        )
        .bind(source)
        .bind(after.map_or(0, TrackKey::raw))
        .bind(folder)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn selected_track_keys(
        &self,
        selection: &MemberSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackKey>> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows = selected_tracks_on(&mut transaction, selection).await?;
        transaction.commit().await?;
        Ok(rows.into_iter().map(|(key, _)| key).collect())
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
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows: Vec<(String,)> =
            selection_rows_on(&mut transaction, selection, "track.media_uri", limit).await?;
        transaction.commit().await?;
        Ok(rows.into_iter().map(|(uri,)| uri).collect())
    }
}
