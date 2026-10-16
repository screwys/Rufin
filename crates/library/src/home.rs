//! Owns one bounded, consistent Home read over provider and Rufin-defined sections.

use serde::{Deserialize, Serialize};
use sqlx::{Connection, SqliteConnection};

use crate::{
    AlbumKey, AlbumView, Database, FolderKey, LibraryResult, ReadCancellation, SourceKey, TrackKey,
    TrackView,
};

const HOME_LIMIT: i64 = 24;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum HomeBlockKind {
    Showcase,
    Explore,
    MostPlayed,
    NewlyAdded,
    RecentlyPlayed,
    RecentlyReleased,
    Genres,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum HomeSectionKind {
    Explore,
    MostPlayed,
    NewlyAdded,
    RecentlyPlayed,
    RecentlyReleased,
}

impl HomeBlockKind {
    pub const fn all() -> [Self; 7] {
        [
            Self::Showcase,
            Self::Explore,
            Self::MostPlayed,
            Self::NewlyAdded,
            Self::RecentlyPlayed,
            Self::RecentlyReleased,
            Self::Genres,
        ]
    }

    pub const fn section_kind(self) -> Option<HomeSectionKind> {
        match self {
            Self::Explore => Some(HomeSectionKind::Explore),
            Self::MostPlayed => Some(HomeSectionKind::MostPlayed),
            Self::NewlyAdded => Some(HomeSectionKind::NewlyAdded),
            Self::RecentlyPlayed => Some(HomeSectionKind::RecentlyPlayed),
            Self::RecentlyReleased => Some(HomeSectionKind::RecentlyReleased),
            Self::Showcase | Self::Genres => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeEntryKind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl HomeEntryKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeEntryInput {
    pub position: i64,
    pub kind: HomeEntryKind,
    pub entity_object_id: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeTrackRow {
    pub track: TrackView,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeAlbumRow {
    pub album: AlbumView,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeSectionRows {
    pub tracks: Vec<HomeTrackRow>,
    pub albums: Vec<HomeAlbumRow>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeProviderSection {
    pub section_id: String,
    pub title: Option<String>,
    pub rows: HomeSectionRows,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomePage {
    pub showcase: Option<HomeAlbumRow>,
    pub explore: Vec<HomeAlbumRow>,
    pub most_played: HomeSectionRows,
    pub newly_added: HomeSectionRows,
    pub recently_played: HomeSectionRows,
    pub recently_released: HomeSectionRows,
    pub genres: Vec<crate::GenreView>,
    pub provider_sections: Vec<HomeProviderSection>,
}

impl Database {
    pub async fn home_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        showcase_variation: i64,
        explore_variation: i64,
        blocks: &[HomeBlockKind],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<HomePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;

        let showcase = if blocks.contains(&HomeBlockKind::Showcase) {
            let album_count = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM albums album
             WHERE (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1)) AND album.source_key=?1 AND (?2 IS NULL OR album.album_key IN (
               SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
               WHERE track.indexed=1 AND scope.folder_key=?2))",
            )
            .bind(source)
            .bind(folder)
            .fetch_one(&mut *transaction)
            .await?;
            let showcase_offset = if album_count == 0 {
                0
            } else {
                showcase_variation.rem_euclid(album_count)
            };
            let row = sqlx::query_as::<_, (AlbumKey, String)>(
                "SELECT album_key,COALESCE(title,'') title FROM albums
             WHERE (indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=albums.album_key AND admitted.indexed=1)) AND source_key=?1 AND (?3 IS NULL OR album_key IN (
               SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
               WHERE track.indexed=1 AND scope.folder_key=?3))
             ORDER BY album_key LIMIT 1 OFFSET ?2",
            )
            .bind(source)
            .bind(showcase_offset)
            .bind(folder)
            .fetch_optional(&mut *transaction)
            .await?;
            home_albums(&mut transaction, source, row.into_iter().collect())
                .await?
                .pop()
        } else {
            None
        };

        let explore = if blocks.contains(&HomeBlockKind::Explore) {
            // Seek into the indexed shuffled order, rather than sorting the library
            // on every refresh or selecting neighboring import IDs.
            let pivot = explore_album_pivot(source, explore_variation);
            let mut explore = sqlx::query_as::<_, (AlbumKey, String)>(
                "SELECT album_key,COALESCE(title,'') title FROM albums
             WHERE (indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=albums.album_key AND admitted.indexed=1)) AND source_key=?1 AND ((album_key*1103515245)%2147483647)>=?2
               AND (?3 IS NULL OR album_key IN (
                 SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
                 WHERE track.indexed=1 AND track.source_key=?1 AND scope.folder_key=?3))
             ORDER BY ((album_key*1103515245)%2147483647),album_key LIMIT ?4",
            )
            .bind(source)
            .bind(pivot)
            .bind(folder)
            .bind(HOME_LIMIT)
            .fetch_all(&mut *transaction)
            .await?;
            if explore.len() < HOME_LIMIT as usize {
                let rest = sqlx::query_as::<_, (AlbumKey, String)>(
                    "SELECT album_key,COALESCE(title,'') title FROM albums
                 WHERE (indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=albums.album_key AND admitted.indexed=1)) AND source_key=?1 AND ((album_key*1103515245)%2147483647)<?2
                   AND (?4 IS NULL OR album_key IN (
                     SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
                     WHERE track.indexed=1 AND track.source_key=?1 AND scope.folder_key=?4))
                 ORDER BY ((album_key*1103515245)%2147483647),album_key LIMIT ?3",
                )
                .bind(source)
                .bind(pivot)
                .bind(HOME_LIMIT - explore.len() as i64)
                .bind(folder)
                .fetch_all(&mut *transaction)
                .await?;
                explore.extend(rest);
            }

            home_albums(&mut transaction, source, explore).await?
        } else {
            Vec::new()
        };

        let most_played = if blocks.contains(&HomeBlockKind::MostPlayed) {
            let mut most_played =
                provider_section(&mut transaction, source, folder, "most-played").await?;
            if most_played.tracks.is_empty() && most_played.albums.is_empty() {
                most_played.tracks = home_activity_tracks(
                    &mut transaction,
                    source,
                    folder,
                    HomeSectionKind::MostPlayed,
                )
                .await?;
            }

            most_played
        } else {
            HomeSectionRows::default()
        };

        let newly_added = if blocks.contains(&HomeBlockKind::NewlyAdded) {
            let mut newly_added =
                provider_section(&mut transaction, source, folder, "newly-added").await?;
            if newly_added.tracks.is_empty() && newly_added.albums.is_empty() {
                let keys = sqlx::query_as::<_, (AlbumKey, String)>(concat!(
                    "SELECT album_key,COALESCE(title,'') title FROM albums
                 WHERE (indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=albums.album_key AND admitted.indexed=1)) AND source_key=?1 AND (?2 IS NULL OR album_key IN (
                   SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
                   WHERE track.indexed=1 AND scope.folder_key=?2))
                 ORDER BY ",
                    "(SELECT added_sort FROM album_order WHERE album_key=albums.album_key)",
                    " DESC NULLS LAST,(SELECT title_sort FROM album_order WHERE album_key=albums.album_key),album_key LIMIT 24"
                ))
                .bind(source)
                .bind(folder)
                .fetch_all(&mut *transaction)
                .await?;
                newly_added.albums = home_albums(&mut transaction, source, keys).await?;
            }

            newly_added
        } else {
            HomeSectionRows::default()
        };

        let recently_played = if blocks.contains(&HomeBlockKind::RecentlyPlayed) {
            let mut recently_played =
                provider_section(&mut transaction, source, folder, "recently-played").await?;
            if recently_played.tracks.is_empty() && recently_played.albums.is_empty() {
                recently_played.tracks = home_activity_tracks(
                    &mut transaction,
                    source,
                    folder,
                    HomeSectionKind::RecentlyPlayed,
                )
                .await?;
            }

            recently_played
        } else {
            HomeSectionRows::default()
        };

        let recently_released = if blocks.contains(&HomeBlockKind::RecentlyReleased) {
            let mut recently_released =
                provider_section(&mut transaction, source, folder, "recently-released").await?;
            if recently_released.tracks.is_empty() && recently_released.albums.is_empty() {
                let keys = sqlx::query_as::<_, (AlbumKey, String)>(
                "SELECT album_key,COALESCE(title,'') title FROM albums
                 WHERE (indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=albums.album_key AND admitted.indexed=1)) AND source_key=?1 AND ((SELECT release_sort FROM album_order WHERE album_key=albums.album_key) IS NOT NULL OR COALESCE((SELECT year_sort FROM album_order WHERE album_key=albums.album_key),0)<>0)
                   AND (?2 IS NULL OR album_key IN (
                     SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
                     WHERE track.indexed=1 AND scope.folder_key=?2))
                 ORDER BY (SELECT release_sort FROM album_order WHERE album_key=albums.album_key) DESC NULLS LAST,(SELECT year_sort FROM album_order WHERE album_key=albums.album_key) DESC NULLS LAST,(SELECT title_sort FROM album_order WHERE album_key=albums.album_key),album_key LIMIT 24",
            )
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?;
                recently_released.albums = home_albums(&mut transaction, source, keys).await?;
            }

            recently_released
        } else {
            HomeSectionRows::default()
        };

        let genres = if blocks.contains(&HomeBlockKind::Genres) {
            let input = crate::CatalogQuery {
                subject: crate::CatalogSubject::Genres {
                    source,
                    sort: crate::CatalogSort::TrackCount,
                },
                folder,
                filter: String::new(),
                descending: true,
                favorites_only: false,
                downloaded_only: false,
                activity: false,
            };
            let query = crate::collections::collection_query(&input);
            let keys = crate::query::selected_values_on::<crate::GenreKey>(
                &mut transaction,
                &query,
                &query.key,
                std::slice::from_ref(&(0..12)),
            )
            .await?;
            crate::collections::load_genre_rows(&mut transaction, &keys, folder).await?
        } else {
            Vec::new()
        };

        let provider_section_ids = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT section_id,max(section_title) FROM home_entries
             WHERE source_key=?1 AND section_id NOT IN ('most-played','newly-added','recently-played','recently-released')
             GROUP BY section_id ORDER BY section_id LIMIT 12",
        )
        .bind(source)
        .fetch_all(&mut *transaction)
        .await?;
        let mut provider_sections = Vec::with_capacity(provider_section_ids.len());
        for (section_id, title) in provider_section_ids {
            let rows = provider_section(&mut transaction, source, folder, &section_id).await?;
            provider_sections.push(HomeProviderSection {
                section_id,
                title,
                rows,
            });
        }

        transaction.commit().await?;
        Ok(HomePage {
            showcase,
            explore,
            most_played,
            newly_added,
            recently_played,
            recently_released,
            genres,
            provider_sections,
        })
    }
}

fn explore_album_pivot(source: SourceKey, variation: i64) -> i64 {
    let mut mixed = (variation as u64) ^ (source.raw() as u64).rotate_left(17);
    mixed = mixed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^= mixed >> 31;
    (mixed % 2_147_483_647) as i64
}

async fn home_activity_tracks(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    kind: HomeSectionKind,
) -> LibraryResult<Vec<HomeTrackRow>> {
    let local = sqlx::query_scalar::<_, bool>(
        "SELECT COALESCE(kind IN ('local','files','webdav','smb','documents'),0)
         FROM sources WHERE source_key=?1",
    )
    .bind(source)
    .fetch_optional(&mut *connection)
    .await?
    .unwrap_or(false);
    let (field, present, tie) = match kind {
        HomeSectionKind::MostPlayed => ("play_count", ">0", ",track.sort_text"),
        HomeSectionKind::RecentlyPlayed => ("last_played", "IS NOT NULL", ""),
        _ => unreachable!(),
    };
    let scope = "track.indexed=1 AND track.source_key=?1 AND (?2 IS NULL OR EXISTS(
        SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))";
    let sql = if local {
        let (current, legacy_field, aggregate, legacy_order) = match kind {
            HomeSectionKind::MostPlayed => (
                "SELECT track_key,local_play_count value FROM tracks
                    WHERE indexed=1 AND source_key=?1 AND local_play_count>0".into(),
                "play_count", "sum", "",
            ),
            HomeSectionKind::RecentlyPlayed => (
                format!("SELECT track.track_key,listen.started_at value FROM listens listen
                    CROSS JOIN tracks track ON track.media_uri=listen.media_uri
                    WHERE {scope} AND listen.listen_key=(SELECT latest.listen_key FROM listens latest
                        WHERE latest.media_uri=listen.media_uri ORDER BY latest.started_at DESC,latest.listen_key DESC LIMIT 1)
                    ORDER BY listen.started_at DESC,track.track_key LIMIT 24"),
                "last_played_at", "max", " ORDER BY old.last_played_at DESC,track.track_key LIMIT 24",
            ),
            _ => unreachable!(),
        };
        // Read only played songs. Each recent input is already ordered and bounded;
        // merging their maxima keeps the latest 24 even when both contain a song.
        format!(
            "WITH current AS ({current}), legacy AS (
            SELECT track.track_key,old.{legacy_field} value FROM legacy_activity old
            JOIN tracks track ON track.source_key=?1 AND track.object_id=old.track_object_id
            WHERE old.source_id=(SELECT object_id FROM sources WHERE source_key=?1)
                AND old.period='lifetime' AND old.item_kind='track' AND old.{legacy_field} {present}
                AND {scope}{legacy_order}
            ), activity AS (SELECT * FROM current UNION ALL SELECT * FROM legacy),
            played AS (SELECT track_key,{aggregate}(value) value FROM activity GROUP BY track_key)
            SELECT track.track_key,track.title FROM played CROSS JOIN tracks track USING(track_key)
            WHERE {scope} ORDER BY played.value DESC{tie},track.track_key LIMIT 24"
        )
    } else {
        format!(
            "SELECT track.track_key,track.title FROM tracks track
            WHERE {scope} AND (SELECT {field} FROM native_media_state WHERE media_uri=track.media_uri) {present}
            ORDER BY (SELECT {field} FROM native_media_state WHERE media_uri=track.media_uri) DESC{tie},track.track_key LIMIT 24"
        )
    };
    let keys = sqlx::query_as::<_, (TrackKey, String)>(sqlx::AssertSqlSafe(sql))
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await?;
    home_tracks(connection, keys).await
}

async fn home_tracks(
    connection: &mut SqliteConnection,
    members: Vec<(TrackKey, String)>,
) -> LibraryResult<Vec<HomeTrackRow>> {
    let keys = members.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    Ok(crate::tracks::load_track_rows(connection, &keys)
        .await?
        .into_iter()
        .zip(members)
        .map(|(track, (_, title))| HomeTrackRow { track, title })
        .collect())
}

async fn home_albums(
    connection: &mut SqliteConnection,
    _source: SourceKey,
    members: Vec<(AlbumKey, String)>,
) -> LibraryResult<Vec<HomeAlbumRow>> {
    let keys = members.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    Ok(
        crate::collections::load_album_rows(connection, &keys, false)
            .await?
            .into_iter()
            .zip(members)
            .map(|(album, (_, title))| {
                let title = if title.is_empty() {
                    album.metadata.title.clone()
                } else {
                    title
                };
                HomeAlbumRow { album, title }
            })
            .collect(),
    )
}

async fn provider_section(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    section_id: &str,
) -> LibraryResult<HomeSectionRows> {
    let tracks = sqlx::query_as::<_, (TrackKey, String)>(
        "SELECT track.track_key,entry.title
         FROM home_entries entry JOIN tracks track ON track.track_key=entry.entity_key
         WHERE track.indexed=1 AND entry.source_key=?1 AND entry.section_id=?2 AND entry.entity_kind='track'
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
         ORDER BY entry.position LIMIT 24",
    )
    .bind(source)
    .bind(section_id)
    .bind(folder)
    .fetch_all(&mut *connection)
    .await?;
    let albums = sqlx::query_as::<_, (AlbumKey, String)>(
        "SELECT album.album_key,entry.title
         FROM home_entries entry JOIN albums album ON album.album_key=entry.entity_key
         WHERE (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1)) AND entry.source_key=?1 AND entry.section_id=?2 AND entry.entity_kind='album'
           AND (?3 IS NULL OR album.album_key IN (
             SELECT track.album_key FROM track_folders scope CROSS JOIN tracks track USING(track_key)
             WHERE track.indexed=1 AND scope.folder_key=?3))
         ORDER BY entry.position LIMIT 24",
    )
    .bind(source)
    .bind(section_id)
    .bind(folder)
    .fetch_all(&mut *connection)
    .await?;
    Ok(HomeSectionRows {
        tracks: home_tracks(connection, tracks).await?,
        albums: home_albums(connection, source, albums).await?,
    })
}
