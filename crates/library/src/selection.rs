//! One ordered song relation for browsing and actions.

use std::{ops::Range, sync::Arc};

use sqlx::{Connection, SqliteConnection};

use crate::{
    AlbumKey, ArtistKey, CatalogQuery, Database, FolderKey, GenreKey, LibraryResult, MoodKey,
    PlaylistEntryKey, PlaylistKey, ReadCancellation, SmartPlaylistKey, SourceId, SourceKey,
    TrackKey,
};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TrackScope {
    Tracks(SourceKey),
    Objects {
        source: SourceKey,
        ids: Arc<[String]>,
    },
    Album(AlbumKey),
    Artist {
        key: ArtistKey,
        album_artist: bool,
    },
    Genre(GenreKey),
    Mood(MoodKey),
    Playlist(PlaylistKey),
    Smart {
        key: SmartPlaylistKey,
        source: Option<SourceKey>,
        now: i64,
    },
    History {
        source: Option<SourceId>,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub enum TrackSelection {
    Query {
        query: CatalogQuery,
        ranges: Option<Vec<Range<usize>>>,
    },
    Concat(Vec<TrackSelection>),
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl TrackSelection {
    #[cfg_attr(feature = "bindings", uniffi::constructor)]
    pub fn from_uris(uris: Vec<String>) -> Arc<Self> {
        Arc::new(CatalogQuery::uris(crate::CatalogKind::Track, uris).selection())
    }
}

pub(crate) async fn selected_tracks_on(
    connection: &mut SqliteConnection,
    selection: &TrackSelection,
) -> LibraryResult<Vec<(TrackKey, Option<PlaylistEntryKey>)>> {
    selection_rows_on(connection, selection, "track.track_key,$entry", None).await
}

async fn selection_rows_on<T>(
    connection: &mut SqliteConnection,
    selection: &TrackSelection,
    columns: &str,
    limit: Option<usize>,
) -> LibraryResult<Vec<T>>
where
    T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> + Send + Unpin,
{
    let mut pending = vec![selection];
    let mut rows = Vec::new();
    while let Some(selection) = pending.pop() {
        let remaining = limit.map(|limit| limit.saturating_sub(rows.len()));
        if remaining == Some(0) {
            break;
        }
        match selection {
            TrackSelection::Concat(selections) => pending.extend(selections.iter().rev()),
            TrackSelection::Query { query, ranges } => {
                let query = crate::query::compile_on(connection, query).await?;
                rows.extend(
                    crate::query::selected_rows_on(
                        connection,
                        &query,
                        &columns.replace("$entry", &query.entry_key),
                        ranges.as_deref(),
                        remaining,
                    )
                    .await?,
                );
            }
        }
    }
    Ok(rows)
}

impl Database {
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
        selection: &TrackSelection,
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
        selection: &TrackSelection,
        limit: Option<usize>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows =
            selection_rows_on::<(String,)>(&mut transaction, selection, "track.media_uri", limit)
                .await?;
        transaction.commit().await?;
        Ok(rows.into_iter().map(|(uri,)| uri).collect())
    }
}
