//! Owns one bounded, consistent Home read over provider and Rufin-defined sections.

use std::collections::BTreeMap;

use sqlx::{Connection, FromRow, SqliteConnection};

use crate::{
    AlbumKey, AlbumRow, Database, FolderKey, GenreKey, LibraryResult, ReadCancellation, SourceKey,
    TrackKey, TrackRow,
};

const HOME_LIMIT: i64 = 24;

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
    pub section_id: String,
    pub position: i64,
    pub kind: HomeEntryKind,
    pub entity_object_id: String,
    pub title: String,
    pub subtitle: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeTrackRow {
    pub track: TrackRow,
    pub title: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HomeAlbumRow {
    pub album: AlbumRow,
    pub title: String,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Home showcase reads are hard-bounded and keep final rows inline"
)]
pub enum HomeShowcaseRow {
    Track(HomeTrackRow),
    Album(HomeAlbumRow),
}

#[derive(Clone, Debug, FromRow)]
struct HomeTrackFact {
    track_key: TrackKey,
    title: String,
    artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow)]
struct HomeAlbumFact {
    album_key: AlbumKey,
    title: String,
    artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct HomeGenreRow {
    pub genre_key: GenreKey,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
    pub album_count: i64,
    pub track_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeSectionRows {
    pub tracks: Vec<HomeTrackRow>,
    pub albums: Vec<HomeAlbumRow>,
}

#[derive(Default)]
struct HomeSectionFacts {
    tracks: Vec<HomeTrackFact>,
    albums: Vec<HomeAlbumFact>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomeProviderSection {
    pub section_id: String,
    pub rows: HomeSectionRows,
}

struct HomeProviderFacts {
    section_id: String,
    rows: HomeSectionFacts,
}

struct HomePageFacts {
    showcase: Option<HomeShowcaseFact>,
    explore: Vec<HomeTrackFact>,
    most_played: HomeSectionFacts,
    newly_added: HomeSectionFacts,
    recently_played: HomeSectionFacts,
    recently_released: HomeSectionFacts,
    genres: Vec<HomeGenreRow>,
    provider_sections: Vec<HomeProviderFacts>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HomePage {
    pub showcase: Option<HomeShowcaseRow>,
    pub explore: Vec<HomeTrackRow>,
    pub most_played: HomeSectionRows,
    pub newly_added: HomeSectionRows,
    pub recently_played: HomeSectionRows,
    pub recently_released: HomeSectionRows,
    pub genres: Vec<HomeGenreRow>,
    pub provider_sections: Vec<HomeProviderSection>,
}

enum HomeShowcaseFact {
    Track(HomeTrackFact),
    Album(HomeAlbumFact),
}

impl Database {
    pub async fn replace_home_section(
        &self,
        source: SourceKey,
        section_id: &str,
        entries: &[HomeEntryInput],
    ) -> LibraryResult<()> {
        if entries.len() > HOME_LIMIT as usize
            || entries.iter().any(|entry| entry.section_id != section_id)
        {
            return Err(crate::LibraryError::InvalidRequest(
                "invalid Home section batch".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let connection = writer
            .as_mut()
            .ok_or(crate::LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        sqlx::query("DELETE FROM home_entries WHERE source_key=?1 AND section_id=?2")
            .bind(source)
            .bind(section_id)
            .execute(&mut *transaction)
            .await?;
        for entry in entries {
            let key =
                match entry.kind {
                    HomeEntryKind::Track => {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT track_key FROM tracks WHERE source_key=?1 AND object_id=?2",
                        )
                        .bind(source)
                        .bind(&entry.entity_object_id)
                        .fetch_optional(&mut *transaction)
                        .await?
                    }
                    HomeEntryKind::Album => {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT album_key FROM albums WHERE source_key=?1 AND object_id=?2",
                        )
                        .bind(source)
                        .bind(&entry.entity_object_id)
                        .fetch_optional(&mut *transaction)
                        .await?
                    }
                    HomeEntryKind::Artist => {
                        sqlx::query_scalar::<_, i64>(
                            "SELECT artist_key FROM artists WHERE source_key=?1 AND object_id=?2",
                        )
                        .bind(source)
                        .bind(&entry.entity_object_id)
                        .fetch_optional(&mut *transaction)
                        .await?
                    }
                    HomeEntryKind::Playlist => sqlx::query_scalar::<_, i64>(
                        "SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2",
                    )
                    .bind(source)
                    .bind(&entry.entity_object_id)
                    .fetch_optional(&mut *transaction)
                    .await?,
                };
            if let Some(key) = key {
                sqlx::query("INSERT INTO home_entries(source_key,section_id,position,entity_kind,entity_key,title,subtitle,artwork_binding) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)").bind(source).bind(section_id).bind(entry.position).bind(entry.kind.as_str()).bind(key).bind(&entry.title).bind(&entry.subtitle).bind(entry.artwork_binding.as_deref()).execute(&mut *transaction).await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }
    pub async fn home_page(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        variation: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<HomePage> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;

        let album_count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM albums album
             WHERE album.source_key=?1 AND (?2 IS NULL OR EXISTS (
               SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key)
               WHERE track.album_key=album.album_key AND scope.folder_key=?2))",
        )
        .bind(source)
        .bind(folder)
        .fetch_one(&mut *transaction)
        .await?;
        let showcase_offset = if album_count == 0 {
            0
        } else {
            variation.rem_euclid(album_count)
        };
        let album_showcase = sqlx::query_as::<_, HomeAlbumFact>(
            "SELECT album_key,title,artwork_binding FROM albums
             WHERE source_key=?1 AND (?3 IS NULL OR EXISTS (
               SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key)
               WHERE track.album_key=albums.album_key AND scope.folder_key=?3))
             ORDER BY album_key LIMIT 1 OFFSET ?2",
        )
        .bind(source)
        .bind(showcase_offset)
        .bind(folder)
        .fetch_optional(&mut *transaction)
        .await?;
        let track_count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM tracks track WHERE track.source_key=?1 AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))",
        )
        .bind(source)
        .bind(folder)
        .fetch_one(&mut *transaction)
        .await?;
        let track_showcase = if track_count == 0 {
            None
        } else {
            sqlx::query_as::<_, HomeTrackFact>(
                "SELECT track_key,title,artwork_binding FROM tracks WHERE source_key=?1 AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3)) ORDER BY track_key LIMIT 1 OFFSET ?2",
            )
            .bind(source)
            .bind(variation.rem_euclid(track_count))
            .bind(folder)
            .fetch_optional(&mut *transaction)
            .await?
        };
        let showcase = if variation.rem_euclid(2) == 1 {
            track_showcase
                .map(HomeShowcaseFact::Track)
                .or_else(|| album_showcase.map(HomeShowcaseFact::Album))
        } else {
            album_showcase
                .map(HomeShowcaseFact::Album)
                .or_else(|| track_showcase.map(HomeShowcaseFact::Track))
        };

        let track_pivot = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(max(track.track_key),0) FROM tracks track
             WHERE track.source_key=?1 AND (?2 IS NULL OR EXISTS (
               SELECT 1 FROM track_folders scope
               WHERE scope.track_key=track.track_key AND scope.folder_key=?2))",
        )
        .bind(source)
        .bind(folder)
        .fetch_one(&mut *transaction)
        .await?;
        let track_pivot = if track_pivot == 0 {
            0
        } else {
            variation.rem_euclid(track_pivot + 1)
        };
        let mut explore = sqlx::query_as::<_, HomeTrackFact>(
            "SELECT track_key,title,artwork_binding FROM tracks
             WHERE source_key=?1 AND track_key>=?2 AND (?3 IS NULL OR EXISTS (
               SELECT 1 FROM track_folders scope
               WHERE scope.track_key=tracks.track_key AND scope.folder_key=?3))
             ORDER BY track_key LIMIT 24",
        )
        .bind(source)
        .bind(track_pivot)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;
        if explore.len() < HOME_LIMIT as usize {
            let rest = sqlx::query_as::<_, HomeTrackFact>(
                "SELECT track_key,title,artwork_binding FROM tracks
                 WHERE source_key=?1 AND track_key<?2 AND (?4 IS NULL OR EXISTS (
                   SELECT 1 FROM track_folders scope
                   WHERE scope.track_key=tracks.track_key AND scope.folder_key=?4))
                 ORDER BY track_key LIMIT ?3",
            )
            .bind(source)
            .bind(track_pivot)
            .bind(HOME_LIMIT - explore.len() as i64)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?;
            explore.extend(rest);
        }

        let mut most_played =
            provider_section(&mut transaction, source, folder, "most-played").await?;
        if most_played.tracks.is_empty() && most_played.albums.is_empty() {
            most_played.tracks = sqlx::query_as::<_, HomeTrackFact>(
                "WITH listen_count AS (
                   SELECT track_key,count(*) plays FROM listens
                   WHERE source_key=?1 AND track_key IS NOT NULL GROUP BY track_key)
                 SELECT track.track_key,track.title,track.artwork_binding
                 FROM tracks track
                 LEFT JOIN activity_baseline baseline ON baseline.source_key=track.source_key AND baseline.track_object_id=track.object_id AND baseline.period='lifetime' AND baseline.item_kind='track'
                 LEFT JOIN listen_count listen USING(track_key)
                 WHERE track.source_key=?1
                   AND COALESCE(baseline.play_count,0)+COALESCE(listen.plays,0)>0
                   AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
                 ORDER BY COALESCE(baseline.play_count,0)+COALESCE(listen.plays,0) DESC,track.sort_text,track.track_key LIMIT 24",
            )
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?;
        }

        let mut newly_added =
            provider_section(&mut transaction, source, folder, "newly-added").await?;
        if newly_added.tracks.is_empty() && newly_added.albums.is_empty() {
            newly_added.albums = sqlx::query_as::<_, HomeAlbumFact>(
                "SELECT album_key,title,artwork_binding FROM albums
                 WHERE source_key=?1 AND (?2 IS NULL OR EXISTS (
                   SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key)
                   WHERE track.album_key=albums.album_key AND scope.folder_key=?2))
                 ORDER BY COALESCE(unixepoch(date_added),first_seen_at) DESC NULLS LAST,sort_text,album_key LIMIT 24",
            )
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?;
        }

        let mut recently_played =
            provider_section(&mut transaction, source, folder, "recently-played").await?;
        if recently_played.tracks.is_empty() && recently_played.albums.is_empty() {
            recently_played.tracks = sqlx::query_as::<_, HomeTrackFact>(
                "WITH latest AS (
                   SELECT track_key,max(started_at) played_at FROM listens
                   WHERE source_key=?1 AND track_key IS NOT NULL GROUP BY track_key)
                 SELECT track.track_key,track.title,track.artwork_binding
                 FROM latest JOIN tracks track USING(track_key)
                 WHERE (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
                 ORDER BY latest.played_at DESC,track.track_key LIMIT 24",
            )
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?;
        }

        let mut recently_released =
            provider_section(&mut transaction, source, folder, "recently-released").await?;
        if recently_released.tracks.is_empty() && recently_released.albums.is_empty() {
            recently_released.albums = sqlx::query_as::<_, HomeAlbumFact>(
                "SELECT album_key,title,artwork_binding FROM albums
                 WHERE source_key=?1 AND (release_date IS NOT NULL OR COALESCE(year,0)<>0)
                   AND (?2 IS NULL OR EXISTS (
                     SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key)
                     WHERE track.album_key=albums.album_key AND scope.folder_key=?2))
                 ORDER BY release_date DESC NULLS LAST,year DESC NULLS LAST,sort_text,album_key LIMIT 24",
            )
            .bind(source)
            .bind(folder)
            .fetch_all(&mut *transaction)
            .await?;
        }

        let genres = sqlx::query_as::<_, HomeGenreRow>(
            "SELECT genre.genre_key,genre.name,genre.artwork_binding,
                    count(DISTINCT track.album_key) album_count,
                    count(DISTINCT relation.track_key) track_count
             FROM genres genre
             LEFT JOIN track_genres relation USING(genre_key)
             LEFT JOIN tracks track ON track.track_key=relation.track_key
             WHERE genre.source_key=?1 AND (?2 IS NULL OR EXISTS (
               SELECT 1 FROM track_genres credit JOIN track_folders scope USING(track_key)
               WHERE credit.genre_key=genre.genre_key AND scope.folder_key=?2))
             GROUP BY genre.genre_key
             ORDER BY count(relation.track_key) DESC,genre.sort_text,genre.genre_key LIMIT 12",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *transaction)
        .await?;

        let provider_section_ids = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT section_id FROM home_entries
             WHERE source_key=?1 AND section_id NOT IN ('most-played','newly-added','recently-played','recently-released')
             ORDER BY section_id LIMIT 12",
        )
        .bind(source)
        .fetch_all(&mut *transaction)
        .await?;
        let mut provider_sections = Vec::with_capacity(provider_section_ids.len());
        for section_id in provider_section_ids {
            let rows = provider_section(&mut transaction, source, folder, &section_id).await?;
            provider_sections.push(HomeProviderFacts { section_id, rows });
        }

        transaction.commit().await?;
        Database::clear_progress(&mut connection).await?;
        drop(connection);
        drop(_permit);
        enrich_home_page(
            self,
            source,
            folder,
            HomePageFacts {
                showcase,
                explore,
                most_played,
                newly_added,
                recently_played,
                recently_released,
                genres,
                provider_sections,
            },
            cancellation,
        )
        .await
    }
}

async fn enrich_home_page(
    database: &Database,
    source: SourceKey,
    folder: Option<FolderKey>,
    facts: HomePageFacts,
    cancellation: &ReadCancellation,
) -> LibraryResult<HomePage> {
    let mut track_keys = facts
        .explore
        .iter()
        .map(|row| row.track_key)
        .collect::<Vec<_>>();
    if let Some(HomeShowcaseFact::Track(row)) = facts.showcase.as_ref() {
        track_keys.push(row.track_key);
    }
    let mut album_keys = facts
        .showcase
        .iter()
        .filter_map(|row| match row {
            HomeShowcaseFact::Album(row) => Some(row.album_key),
            HomeShowcaseFact::Track(_) => None,
        })
        .collect::<Vec<_>>();
    for section in [
        &facts.most_played,
        &facts.newly_added,
        &facts.recently_played,
        &facts.recently_released,
    ] {
        track_keys.extend(section.tracks.iter().map(|row| row.track_key));
        album_keys.extend(section.albums.iter().map(|row| row.album_key));
    }
    for section in &facts.provider_sections {
        track_keys.extend(section.rows.tracks.iter().map(|row| row.track_key));
        album_keys.extend(section.rows.albums.iter().map(|row| row.album_key));
    }
    track_keys.sort_unstable();
    track_keys.dedup();
    album_keys.sort_unstable();
    album_keys.dedup();

    let mut tracks = BTreeMap::new();
    for keys in track_keys.chunks(128) {
        for row in database.track_rows(source, keys, cancellation).await? {
            tracks.insert(row.track_key, row);
        }
    }
    let mut albums = BTreeMap::new();
    for keys in album_keys.chunks(128) {
        for row in database
            .album_rows(source, keys, folder, cancellation)
            .await?
        {
            albums.insert(row.album_key, row);
        }
    }

    let section = |facts: HomeSectionFacts| HomeSectionRows {
        tracks: facts
            .tracks
            .into_iter()
            .filter_map(|fact| {
                tracks
                    .get(&fact.track_key)
                    .cloned()
                    .map(|track| HomeTrackRow {
                        track,
                        title: fact.title,
                        artwork_binding: fact.artwork_binding,
                    })
            })
            .collect(),
        albums: facts
            .albums
            .into_iter()
            .filter_map(|fact| {
                albums
                    .get(&fact.album_key)
                    .cloned()
                    .map(|album| HomeAlbumRow {
                        album,
                        title: fact.title,
                        artwork_binding: fact.artwork_binding,
                    })
            })
            .collect(),
    };
    Ok(HomePage {
        showcase: facts.showcase.and_then(|fact| match fact {
            HomeShowcaseFact::Track(fact) => tracks.get(&fact.track_key).cloned().map(|track| {
                HomeShowcaseRow::Track(HomeTrackRow {
                    track,
                    title: fact.title,
                    artwork_binding: fact.artwork_binding,
                })
            }),
            HomeShowcaseFact::Album(fact) => albums.get(&fact.album_key).cloned().map(|album| {
                HomeShowcaseRow::Album(HomeAlbumRow {
                    album,
                    title: fact.title,
                    artwork_binding: fact.artwork_binding,
                })
            }),
        }),
        explore: facts
            .explore
            .into_iter()
            .filter_map(|fact| {
                tracks
                    .get(&fact.track_key)
                    .cloned()
                    .map(|track| HomeTrackRow {
                        track,
                        title: fact.title,
                        artwork_binding: fact.artwork_binding,
                    })
            })
            .collect(),
        most_played: section(facts.most_played),
        newly_added: section(facts.newly_added),
        recently_played: section(facts.recently_played),
        recently_released: section(facts.recently_released),
        genres: facts.genres,
        provider_sections: facts
            .provider_sections
            .into_iter()
            .map(|provider| HomeProviderSection {
                section_id: provider.section_id,
                rows: section(provider.rows),
            })
            .collect(),
    })
}

async fn provider_section(
    connection: &mut SqliteConnection,
    source: SourceKey,
    folder: Option<FolderKey>,
    section_id: &str,
) -> LibraryResult<HomeSectionFacts> {
    let tracks = sqlx::query_as::<_, HomeTrackFact>(
        "SELECT track.track_key,entry.title,
                COALESCE(entry.artwork_binding,track.artwork_binding) artwork_binding
         FROM home_entries entry JOIN tracks track ON track.track_key=entry.entity_key
         WHERE entry.source_key=?1 AND entry.section_id=?2 AND entry.entity_kind='track'
           AND (?3 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?3))
         ORDER BY entry.position LIMIT 24",
    )
    .bind(source)
    .bind(section_id)
    .bind(folder)
    .fetch_all(&mut *connection)
    .await?;
    let albums = sqlx::query_as::<_, HomeAlbumFact>(
        "SELECT album.album_key,entry.title,
                COALESCE(entry.artwork_binding,album.artwork_binding) artwork_binding
         FROM home_entries entry JOIN albums album ON album.album_key=entry.entity_key
         WHERE entry.source_key=?1 AND entry.section_id=?2 AND entry.entity_kind='album'
           AND (?3 IS NULL OR EXISTS (
             SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key)
             WHERE track.album_key=album.album_key AND scope.folder_key=?3))
         ORDER BY entry.position LIMIT 24",
    )
    .bind(source)
    .bind(section_id)
    .bind(folder)
    .fetch_all(&mut *connection)
    .await?;
    Ok(HomeSectionFacts { tracks, albums })
}
