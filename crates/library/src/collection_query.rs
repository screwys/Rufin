use crate::collections::{
    COLLECTION_ROW_LIMIT, album_detail_spans_sql, album_detail_track_count, album_scope,
    album_track_scope, artist_scope, load_album_order, load_album_rows, load_artist_order,
    load_artist_rows, load_genre_order, load_genre_rows, load_mood_order, load_mood_rows,
    named_scope,
};
use crate::playlists::{PLAYLIST_ROW_LIMIT, playlist_order_query};
use crate::smart_playlists::{
    load_smart_playlist_order, load_smart_playlist_rows, smart_membership_sql,
};
use crate::tracks::load_track_rows;
use crate::*;
use sqlx::AssertSqlSafe;
use sqlx::Connection;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CollectionKind {
    Albums {
        source: SourceKey,
        sort: AlbumSort,
        inline_tracks: Option<usize>,
    },
    Artists {
        source: SourceKey,
        sort: ArtistSort,
        album_artist: bool,
    },
    Genres {
        source: SourceKey,
        sort: GenreSort,
    },
    Moods {
        source: SourceKey,
        sort: MoodSort,
    },
    Playlists {
        source: Option<SourceKey>,
        sort: PlaylistSort,
    },
    SmartPlaylists {
        source: Option<SourceKey>,
        sort: SmartPlaylistListSort,
        now: i64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollectionQuery {
    pub kind: CollectionKind,
    pub folder: Option<FolderKey>,
    pub filter: String,
    pub descending: bool,
    pub favorites_only: bool,
}

pub enum CollectionRows {
    Albums(Vec<crate::AlbumRow>),
    AlbumDetails(Vec<crate::AlbumDetailRow>),
    Artists(Vec<crate::ArtistRow>),
    Genres(Vec<crate::GenreRow>),
    Moods(Vec<crate::MoodRow>),
    Playlists(Vec<crate::PlaylistRow>),
    SmartPlaylists(Vec<crate::SmartPlaylistRow>),
}

impl CollectionRows {
    pub fn albums(self) -> Vec<crate::AlbumRow> {
        let Self::Albums(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
    pub fn album_details(self) -> Vec<crate::AlbumDetailRow> {
        let Self::AlbumDetails(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
    pub fn artists(self) -> Vec<crate::ArtistRow> {
        let Self::Artists(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
    pub fn genres(self) -> Vec<crate::GenreRow> {
        let Self::Genres(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
    pub fn moods(self) -> Vec<crate::MoodRow> {
        let Self::Moods(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
    pub fn playlists(self) -> Vec<crate::PlaylistRow> {
        let Self::Playlists(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
    pub fn smart_playlists(self) -> Vec<crate::SmartPlaylistRow> {
        let Self::SmartPlaylists(rows) = self else {
            unreachable!("collection row kind matches its query")
        };
        rows
    }
}

impl CollectionQuery {
    pub async fn count(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<usize> {
        let folder = self.folder;
        let filter = self.filter.as_str();
        let favorites_only = self.favorites_only;
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let result: LibraryResult<i64> = match self.kind {
            CollectionKind::Albums {
                source,
                sort: _,
                inline_tracks: None,
            } => Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM albums album WHERE {}",
                album_scope(source, folder, favorites_only, filter)
            )))
            .fetch_one(&mut *connection)
            .await?),
            CollectionKind::Albums {
                source,
                sort: _,
                inline_tracks: Some(inline_tracks),
            } => Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COALESCE(sum(1+max({}-{},0)),0) FROM albums album WHERE {}",
                album_detail_track_count(folder),
                inline_tracks,
                album_scope(source, folder, favorites_only, filter)
            )))
            .fetch_one(&mut *connection)
            .await?),
            CollectionKind::Artists {
                source,
                sort: _,
                album_artist: album_artists_only,
            } => Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM artists artist WHERE {}",
                artist_scope(source, folder, album_artists_only, favorites_only, filter)
            )))
            .fetch_one(&mut *connection)
            .await?),
            CollectionKind::Genres { source, sort: _ } => {
                Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM genres genre WHERE {}",
                    named_scope("genre", source, folder, filter)
                )))
                .fetch_one(&mut *connection)
                .await?)
            }
            CollectionKind::Moods { source, sort: _ } => {
                Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM moods mood WHERE {}",
                    named_scope("mood", source, folder, filter)
                )))
                .fetch_one(&mut *connection)
                .await?)
            }
            CollectionKind::Playlists { source, sort: _ } => Ok(playlist_order_query(
                source,
                folder,
                PlaylistSort::Position,
                false,
                filter,
                true,
            )
            .build_query_scalar::<i64>()
            .fetch_one(&mut *connection)
            .await?),
            CollectionKind::SmartPlaylists {
                source,
                sort: _,
                now,
            } => Ok(if folder.is_none() {
                sqlx::query_scalar(
                    "SELECT count(*) FROM smart_playlists WHERE instr(normalized_name,lower(?1))>0",
                )
                .bind(filter.trim())
                .fetch_one(&mut *connection)
                .await?
            } else {
                let policy =
                    smart_membership_sql(&mut connection, source, folder, now, None).await?;
                sqlx::query_scalar(AssertSqlSafe(format!("{policy} SELECT count(*) FROM definitions WHERE instr(normalized_name,lower(?1))>0 AND (current_scope=0 OR EXISTS(SELECT 1 FROM selected WHERE selected.definition_key=definitions.definition_key))")))
                .bind(filter.trim()).fetch_one(&mut *connection).await?
            }),
        };
        result.map(|count| count.max(0) as usize)
    }
    pub async fn rows(
        &self,
        database: &Database,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<CollectionRows> {
        let folder = self.folder;
        let filter = self.filter.as_str();
        let descending = self.descending;
        let favorites_only = self.favorites_only;
        let (_permit, mut connection) = database.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let rows: LibraryResult<CollectionRows> = match self.kind {
            CollectionKind::Albums {
                source,
                sort,
                inline_tracks: None,
            } => {
                let order = load_album_order::<AlbumKey>(
                    &mut transaction,
                    source,
                    folder,
                    favorites_only,
                    filter,
                    sort,
                    descending,
                    Some((offset, limit)),
                    false,
                )
                .await?;
                let rows = load_album_rows(&mut transaction, source, &order, folder).await?;
                Ok(CollectionRows::Albums(rows))
            }
            CollectionKind::Albums {
                source,
                sort,
                inline_tracks: Some(inline_tracks),
            } => {
                let offset = offset.min(i64::MAX as usize);
                let end = offset
                    .saturating_add(limit.min(COLLECTION_ROW_LIMIT))
                    .min(i64::MAX as usize);
                let spans =
                    sqlx::query_as::<_, (AlbumKey, i64, i64)>(sqlx::AssertSqlSafe(format!(
                        "{} SELECT album_key,track_count,first_position FROM spans
             WHERE first_position<?1 AND first_position+max(track_count-{inline_tracks},0)>=?2
             ORDER BY first_position",
                        album_detail_spans_sql(
                            source,
                            folder,
                            favorites_only,
                            filter,
                            sort,
                            descending,
                            inline_tracks,
                            end,
                        )
                    )))
                    .bind(end.min(i64::MAX as usize) as i64)
                    .bind(offset.min(i64::MAX as usize) as i64)
                    .fetch_all(&mut *transaction)
                    .await?;
                let mut items = Vec::new();
                let mut albums = Vec::new();
                let mut tracks = Vec::new();
                for (album, track_count, first_position) in spans {
                    let first_position = first_position as usize;
                    let track_count = track_count as usize;
                    let header = first_position >= offset;
                    if header {
                        albums.push(album);
                    }
                    let first_track = first_position.saturating_add(1);
                    let start = if header {
                        0
                    } else {
                        inline_tracks.saturating_add(offset.saturating_sub(first_track))
                    };
                    let count = inline_tracks
                        .saturating_add(end.saturating_sub(first_track))
                        .min(track_count)
                        .saturating_sub(start);
                    let scope = album_track_scope(folder).replace("album.album_key", "?1");
                    let keys = sqlx::query_scalar::<_, TrackKey>(sqlx::AssertSqlSafe(format!(
                "SELECT item.track_key FROM tracks item WHERE {scope}
                 ORDER BY item.disc_number,item.track_number,item.sort_text,item.track_key LIMIT ?2 OFFSET ?3"
            )))
            .bind(album)
            .bind(count as i64)
            .bind(start as i64)
            .fetch_all(&mut *transaction)
            .await?;
                    tracks.extend(&keys);
                    items.push((album, header, start, track_count, keys));
                }
                let mut albums = load_album_rows(&mut transaction, source, &albums, folder)
                    .await?
                    .into_iter()
                    .map(|row| (row.album_key, row))
                    .collect::<BTreeMap<_, _>>();
                let mut tracks = load_track_rows(&mut transaction, &tracks)
                    .await?
                    .into_iter()
                    .map(|row| (row.track_key, row))
                    .collect::<BTreeMap<_, _>>();
                let mut rows = Vec::new();
                for (key, header, start, track_count, keys) in items {
                    let mut loaded = keys.into_iter().filter_map(|key| tracks.remove(&key));
                    if header && let Some(album) = albums.remove(&key) {
                        rows.push(AlbumDetailRow::Album {
                            album,
                            tracks: loaded.by_ref().take(inline_tracks).collect(),
                            last: track_count <= inline_tracks,
                        });
                    }
                    for (position, track) in loaded.enumerate() {
                        let index = position + start.max(inline_tracks);
                        rows.push(AlbumDetailRow::Track {
                            track,
                            index,
                            last: index + 1 == track_count,
                        });
                    }
                }
                Ok(CollectionRows::AlbumDetails(rows))
            }
            CollectionKind::Artists {
                source,
                sort,
                album_artist: album_artists_only,
            } => {
                let order = load_artist_order::<ArtistKey>(
                    &mut transaction,
                    source,
                    folder,
                    album_artists_only,
                    favorites_only,
                    filter,
                    sort,
                    descending,
                    Some((offset, limit)),
                    false,
                )
                .await?;
                let rows =
                    load_artist_rows(&mut transaction, source, &order, album_artists_only, folder)
                        .await?;
                Ok(CollectionRows::Artists(rows))
            }
            CollectionKind::Genres { source, sort } => {
                let keys = load_genre_order::<GenreKey>(
                    &mut transaction,
                    source,
                    folder,
                    filter,
                    sort,
                    descending,
                    Some((offset, limit)),
                    false,
                )
                .await?;
                let rows = load_genre_rows(&mut transaction, source, &keys, folder).await?;
                Ok(CollectionRows::Genres(rows))
            }
            CollectionKind::Moods { source, sort } => {
                let result = load_mood_order::<MoodKey>(
                    &mut transaction,
                    source,
                    folder,
                    filter,
                    sort,
                    descending,
                    offset,
                    limit,
                    false,
                )
                .await?;
                let first_rows = load_mood_rows(&mut transaction, source, &result, folder).await?;
                Ok(CollectionRows::Moods(first_rows))
            }
            CollectionKind::Playlists { source, sort } => {
                let keys = playlist_order_query(source, folder, sort, descending, filter, false)
                    .push(" LIMIT ")
                    .push_bind(limit.min(PLAYLIST_ROW_LIMIT) as i64)
                    .push(" OFFSET ")
                    .push_bind(offset.min(i64::MAX as usize) as i64)
                    .build_query_scalar::<PlaylistKey>()
                    .persistent(false)
                    .fetch_all(&mut *transaction)
                    .await?;
                let rows = Database::load_playlist_rows(&mut transaction, &keys).await?;
                Ok(CollectionRows::Playlists(rows))
            }
            CollectionKind::SmartPlaylists { source, sort, now } => {
                let keys = load_smart_playlist_order::<SmartPlaylistKey>(
                    &mut transaction,
                    source,
                    folder,
                    sort,
                    descending,
                    filter,
                    now,
                    offset,
                    limit,
                    false,
                )
                .await?;
                let rows =
                    load_smart_playlist_rows(&mut transaction, source, &keys, folder, now).await?;
                Ok(CollectionRows::SmartPlaylists(rows))
            }
        };
        let rows = rows?;
        transaction.commit().await?;
        Ok(rows)
    }
    pub async fn sections(
        &self,
        database: &Database,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<ScrollSection>> {
        let folder = self.folder;
        let filter = self.filter.as_str();
        let descending = self.descending;
        let favorites_only = self.favorites_only;
        match self.kind {
            CollectionKind::Albums {
                source,
                sort,
                inline_tracks: None,
            } => {
                if !matches!(sort, AlbumSort::Title | AlbumSort::AlbumArtist) {
                    return Ok(Vec::new());
                }
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                crate::scroll_sections::decode_sections(
                    load_album_order::<String>(
                        &mut connection,
                        source,
                        folder,
                        favorites_only,
                        filter,
                        sort,
                        descending,
                        None,
                        true,
                    )
                    .await?,
                )
            }
            CollectionKind::Albums {
                inline_tracks: Some(_),
                ..
            } => Ok(Vec::new()),
            CollectionKind::Artists {
                source,
                sort,
                album_artist: album_artists_only,
            } => {
                if sort != ArtistSort::Title {
                    return Ok(Vec::new());
                }
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                crate::scroll_sections::decode_sections(
                    load_artist_order::<String>(
                        &mut connection,
                        source,
                        folder,
                        album_artists_only,
                        favorites_only,
                        filter,
                        sort,
                        descending,
                        None,
                        true,
                    )
                    .await?,
                )
            }
            CollectionKind::Genres { source, sort } => {
                if sort != GenreSort::Title {
                    return Ok(Vec::new());
                }
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                crate::scroll_sections::decode_sections(
                    load_genre_order::<String>(
                        &mut connection,
                        source,
                        folder,
                        filter,
                        sort,
                        descending,
                        None,
                        true,
                    )
                    .await?,
                )
            }
            CollectionKind::Moods { source, sort } => {
                if sort != MoodSort::Title {
                    return Ok(Vec::new());
                }
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                crate::scroll_sections::decode_sections(
                    load_mood_order::<String>(
                        &mut connection,
                        source,
                        folder,
                        filter,
                        sort,
                        descending,
                        0,
                        usize::MAX,
                        true,
                    )
                    .await?,
                )
            }
            CollectionKind::Playlists { source, sort } => {
                if sort != PlaylistSort::Title {
                    return Ok(Vec::new());
                }
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                crate::scroll_sections::builder_sections(
                    playlist_order_query(source, folder, sort, descending, filter, false),
                    "playlist.sort_text",
                    &mut connection,
                )
                .await
            }
            CollectionKind::SmartPlaylists { source, sort, now } => {
                if sort != SmartPlaylistListSort::Title {
                    return Ok(Vec::new());
                }
                let (_permit, mut connection) = database.acquire_general(cancellation).await?;
                crate::scroll_sections::decode_sections(
                    load_smart_playlist_order::<String>(
                        &mut connection,
                        source,
                        folder,
                        sort,
                        descending,
                        filter,
                        now,
                        0,
                        usize::MAX,
                        true,
                    )
                    .await?,
                )
            }
        }
    }

    pub async fn page(
        &self,
        database: &Database,
        window: RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, usize, CollectionRows)> {
        let count = self.count(database, cancellation).await?;
        let range = window.range(count);
        Ok((
            count,
            range.start,
            self.rows(database, range.start, range.len(), cancellation)
                .await?,
        ))
    }
}
