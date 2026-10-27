//! Album, Artist, Genre, Mood and Folder rows and details. Orders, totals, visibility and
//! pictures come from the resident Catalog; the rest of a row is read from the Store by key.

use sqlx::{FromRow, SqliteConnection};

use crate::catalog::snapshot::Packed;
use crate::catalog::views::{Art, Context, Owner, Totals, favorite, rating};
use crate::{
    AlbumKey, ArtistKey, Catalog, FolderKey, GenreKey, LibraryError, LibraryResult, MoodKey,
    ReadCancellation, SourceKey, TrackView,
};

pub(crate) const COLLECTION_ROW_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "rows are held only for a visible page"
)]
pub enum AlbumDetailRow {
    Album {
        album: AlbumView,
        last: bool,
    },
    Track {
        track: TrackView,
        index: usize,
        last: bool,
    },
}

#[derive(Clone, Debug, FromRow)]
pub struct GenreChoice {
    pub genre_key: GenreKey,
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumArtistLink {
    pub artist_key: ArtistKey,
    pub media_uri: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumGenreLink {
    pub genre_key: GenreKey,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AlbumReleaseClass {
    Album,
    Ep,
    Single,
    Collection,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CollectionSummary {
    pub album_count: i64,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumLinks {
    #[sqlx(json)]
    pub album_artists: Vec<AlbumArtistLink>,
    #[sqlx(json)]
    pub genres: Vec<AlbumGenreLink>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AlbumView {
    pub album_key: AlbumKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    #[sqlx(json)]
    pub metadata: crate::AlbumMetadata,
    #[sqlx(flatten)]
    pub state: crate::TrackState,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    #[sqlx(flatten)]
    pub links: AlbumLinks,
    pub artwork_binding: Option<Vec<u8>>,
    pub release_lookup_identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ArtistView {
    pub artist_key: ArtistKey,
    pub source_key: SourceKey,
    pub object_id: String,
    pub media_uri: String,
    #[sqlx(json)]
    pub metadata: crate::ArtistMetadata,
    #[sqlx(flatten)]
    pub state: crate::TrackState,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GenreView {
    pub genre_key: GenreKey,
    pub source_key: SourceKey,
    pub source_id: String,
    pub object_id: String,
    #[sqlx(json)]
    pub metadata: crate::TagMetadata,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    pub artwork_binding: Option<Vec<u8>>,
    #[sqlx(skip)]
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MoodView {
    pub mood_key: MoodKey,
    pub source_key: SourceKey,
    #[sqlx(json)]
    pub metadata: crate::TagMetadata,
    #[sqlx(flatten)]
    pub summary: CollectionSummary,
    #[sqlx(skip)]
    pub representative_artwork: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct FolderChoice {
    pub folder_key: FolderKey,
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FolderView {
    pub folder_key: FolderKey,
    pub source_key: SourceKey,
    pub object_id: String,
    #[sqlx(json)]
    pub metadata: crate::FolderMetadata,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
}

#[derive(Clone, Debug, Eq, FromRow, PartialEq)]
pub struct AlbumReleaseCandidate {
    pub album_key: AlbumKey,
    pub media_uri: String,
    pub lookup_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlbumReleaseResult {
    Found { release_types: Vec<String> },
    Missing,
}

/// The resident part of a collection row.
#[derive(Clone, Copy)]
struct Resident {
    totals: Totals,
    favorite: bool,
    rating: Option<i64>,
    art: Art,
}

/// Displayed play facts: the source's own counts, else the members' when asked for.
fn played(
    native: (Option<i64>, Option<i64>),
    totals: &Totals,
    activity: bool,
) -> (i64, Option<i64>) {
    let (plays, last) = if activity {
        (Some(totals.plays as i64), totals.last_played.map(i64::from))
    } else {
        (None, None)
    };
    (native.0.or(plays).unwrap_or(0), native.1.or(last))
}

#[derive(Clone, Copy)]
enum Kind {
    Album,
    Artist(bool),
    Tag,
    Folder,
}

impl Catalog {
    fn resident_collections(&self, kind: Kind, keys: &[i64]) -> Vec<Option<Resident>> {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot, self.distinct_track_covers());
        let derived = self.derived(&context, None);
        keys.iter()
            .map(|key| {
                Some(match kind {
                    Kind::Album => {
                        let slot = snapshot.albums.slot(*key)?;
                        let record = &snapshot.albums.records[slot as usize];
                        let mut totals = derived.albums[slot as usize];
                        totals.albums = u32::from(totals.tracks > 0);
                        Resident {
                            totals,
                            favorite: favorite(record.flags).unwrap_or(false),
                            rating: rating(record.rating, record.native_rating)
                                .map(|rating| i64::from(rating / 10)),
                            art: context.album_art(&derived, slot),
                        }
                    }
                    Kind::Artist(album_artist) => {
                        let slot = snapshot.artists.slot(*key)?;
                        let record = &snapshot.artists.records[slot as usize];
                        Resident {
                            totals: derived.artists[usize::from(album_artist)][slot as usize],
                            favorite: favorite(record.flags).unwrap_or(false),
                            rating: rating(record.rating, record.native_rating)
                                .map(|rating| i64::from(rating / 10)),
                            art: context.artist_art(&derived, slot),
                        }
                    }
                    Kind::Tag => Resident {
                        totals: derived.tags[snapshot.tags.slot(*key)? as usize],
                        favorite: false,
                        rating: None,
                        art: Art::None,
                    },
                    Kind::Folder => Resident {
                        totals: derived.folders[snapshot.folders.slot(*key)? as usize],
                        favorite: false,
                        rating: None,
                        art: Art::None,
                    },
                })
            })
            .collect()
    }
}

pub(crate) async fn load_album_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[AlbumKey],
    activity: bool,
) -> LibraryResult<Vec<AlbumView>> {
    // Collection rows show totals, which wait for the Catalog.
    catalog.loaded().await;
    use crate::schema::{LINK_ARTIST, LINK_GENRE};
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let raw = keys.iter().map(|key| key.raw()).collect::<Vec<_>>();
    let resident = catalog.resident_collections(Kind::Album, &raw);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        native_play_count: Option<i64>,
        native_last_played: Option<i64>,
        #[sqlx(flatten)]
        album: AlbumView,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT CAST(requested.key AS INTEGER) ordinal,album.native_play_count,album.native_last_played,
        album.album_key,album.source_key,album.object_id,album.media_uri,
        json_patch(json(album.metadata),json_object('title',album.title,'artist',album.display_artist,'sort_text',album.sort_text,'year',album.year,'release_date',album.release_date,'date_added',coalesce(date(album.date_added),album.date_added,date(album.first_seen_at,'unixepoch')),'is_compilation',json(CASE album.is_compilation WHEN 1 THEN 'true' WHEN 0 THEN 'false' END))) metadata,
        0 favorite,NULL rating,NULL last_played,0 play_count,0 skip_count,0 album_count,0 track_count,0 duration_millis,0 downloaded_count,
        (SELECT json_group_array(json_object('artist_key',artist.artist_key,'media_uri',artist.media_uri,'name',artist.name) ORDER BY credit.position) FROM album_links credit JOIN artists artist ON artist.artist_key=credit.target WHERE credit.album_key=album.album_key AND credit.role={LINK_ARTIST}) album_artists,
        (SELECT json_group_array(json_object('genre_key',tag.tag_key,'name',tag.name) ORDER BY credit.position) FROM album_links credit JOIN tags tag ON tag.tag_key=credit.target WHERE credit.album_key=album.album_key AND credit.role={LINK_GENRE}) genres,
        NULL artwork_binding,album.release_lookup_identity
        FROM json_each(?1) requested JOIN albums album ON album.album_key=requested.value ORDER BY CAST(requested.key AS INTEGER)"
    )))
    .bind(serde_json::to_string(&raw)?)
    .fetch_all(&mut *connection)
    .await?;
    let mut albums = Vec::with_capacity(rows.len());
    let mut arts = Vec::with_capacity(rows.len());
    for row in rows {
        let mut album = row.album;
        let resident = resident
            .get(row.ordinal as usize)
            .copied()
            .flatten()
            .unwrap_or(Resident {
                totals: Totals::default(),
                favorite: false,
                rating: None,
                art: Art::None,
            });
        let (play_count, last_played) = played(
            (row.native_play_count, row.native_last_played),
            &resident.totals,
            activity,
        );
        album.state = crate::TrackState {
            favorite: resident.favorite,
            rating: resident.rating,
            last_played,
            play_count,
            skip_count: 0,
        };
        album.summary = resident.totals.summary();
        arts.push(resident.art);
        albums.push(album);
    }
    for (album, binding) in albums
        .iter_mut()
        .zip(crate::artwork::art_bindings(connection, &arts).await?)
    {
        album.artwork_binding = binding;
    }
    Ok(albums)
}

pub(crate) async fn load_artist_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[ArtistKey],
    album_artist: bool,
    activity: bool,
) -> LibraryResult<Vec<ArtistView>> {
    catalog.loaded().await;
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let raw = keys.iter().map(|key| key.raw()).collect::<Vec<_>>();
    let resident = catalog.resident_collections(Kind::Artist(album_artist), &raw);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        native_play_count: Option<i64>,
        native_last_played: Option<i64>,
        #[sqlx(flatten)]
        artist: ArtistView,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT CAST(requested.key AS INTEGER) ordinal,artist.native_play_count,artist.native_last_played,
        artist.artist_key,artist.source_key,artist.object_id,artist.media_uri,
        json_object('name',artist.name,'sort_text',artist.sort_text,'musicbrainz_artist_id',artist.musicbrainz_artist_id) metadata,
        0 favorite,NULL rating,NULL last_played,0 play_count,0 skip_count,0 album_count,0 track_count,0 duration_millis,0 downloaded_count,NULL artwork_binding
        FROM json_each(?1) requested JOIN artists artist ON artist.artist_key=requested.value ORDER BY CAST(requested.key AS INTEGER)",
    )
    .bind(serde_json::to_string(&raw)?)
    .fetch_all(&mut *connection)
    .await?;
    let mut artists = Vec::with_capacity(rows.len());
    let mut arts = Vec::with_capacity(rows.len());
    for row in rows {
        let mut artist = row.artist;
        let Some(resident) = resident.get(row.ordinal as usize).copied().flatten() else {
            arts.push(Art::None);
            artists.push(artist);
            continue;
        };
        let (play_count, last_played) = played(
            (row.native_play_count, row.native_last_played),
            &resident.totals,
            activity,
        );
        artist.state = crate::TrackState {
            favorite: resident.favorite,
            rating: resident.rating,
            last_played,
            play_count,
            skip_count: 0,
        };
        artist.summary = resident.totals.summary();
        arts.push(resident.art);
        artists.push(artist);
    }
    for (artist, binding) in artists
        .iter_mut()
        .zip(crate::artwork::art_bindings(connection, &arts).await?)
    {
        artist.artwork_binding = binding;
    }
    Ok(artists)
}

/// The bindings of each row's mosaic, in order: covers of the tracks it picked.
async fn gallery_bindings(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    mosaics: &[Option<String>],
) -> LibraryResult<Vec<Vec<Vec<u8>>>> {
    let picks = mosaics
        .iter()
        .map(|mosaic| {
            mosaic
                .as_deref()
                .map(serde_json::from_str::<Vec<String>>)
                .transpose()
                .map(Option::unwrap_or_default)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let keys: Vec<Option<i64>> = sqlx::query_scalar("SELECT track.track_key FROM json_each(?1) requested LEFT JOIN tracks track ON track.media_uri=requested.value ORDER BY requested.key")
        .bind(serde_json::to_string(&picks.iter().flatten().collect::<Vec<_>>())?)
        .fetch_all(&mut *connection)
        .await?;
    let arts = catalog.arts(keys.into_iter().map(|key| key.map(Owner::Track)));
    let mut bindings = crate::artwork::art_bindings(connection, &arts)
        .await?
        .into_iter();
    Ok(picks
        .iter()
        .map(|picks| bindings.by_ref().take(picks.len()).flatten().collect())
        .collect())
}

pub(crate) async fn load_genre_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[GenreKey],
) -> LibraryResult<Vec<GenreView>> {
    catalog.loaded().await;
    let raw = keys.iter().map(|key| key.raw()).collect::<Vec<_>>();
    let resident = catalog.resident_collections(Kind::Tag, &raw);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        mosaic: Option<String>,
        #[sqlx(flatten)]
        genre: GenreView,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT CAST(requested.key AS INTEGER) ordinal,genre.mosaic,genre.tag_key genre_key,genre.source_key,source.object_id source_id,genre.object_id,
        json_object('name',genre.name,'sort_text',genre.sort_text) metadata,0 album_count,0 track_count,0 duration_millis,0 downloaded_count,{} artwork_binding
        FROM json_each(?1) requested JOIN tags genre ON genre.tag_key=requested.value AND genre.kind=0 JOIN sources source ON source.source_key=genre.source_key
        ORDER BY CAST(requested.key AS INTEGER)",
        crate::artwork::preview_sql("genre.native_artwork")
    )))
    .bind(serde_json::to_string(&raw)?)
    .fetch_all(&mut *connection)
    .await?;
    let mosaics = rows
        .iter()
        .map(|row| {
            row.mosaic
                .clone()
                .filter(|_| row.genre.artwork_binding.is_none())
        })
        .collect::<Vec<_>>();
    let galleries = gallery_bindings(catalog, connection, &mosaics).await?;
    Ok(rows
        .into_iter()
        .zip(galleries)
        .map(|(row, gallery)| {
            let mut genre = row.genre;
            if let Some(resident) = resident.get(row.ordinal as usize).copied().flatten() {
                genre.summary = resident.totals.summary();
            }
            genre.representative_artwork = gallery;
            genre
        })
        .collect())
}

pub(crate) async fn load_mood_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[MoodKey],
) -> LibraryResult<Vec<MoodView>> {
    catalog.loaded().await;
    let raw = keys.iter().map(|key| key.raw()).collect::<Vec<_>>();
    let resident = catalog.resident_collections(Kind::Tag, &raw);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        mosaic: Option<String>,
        #[sqlx(flatten)]
        mood: MoodView,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT CAST(requested.key AS INTEGER) ordinal,mood.mosaic,mood.tag_key mood_key,mood.source_key,
        json_object('name',mood.name,'sort_text',mood.sort_text) metadata,0 album_count,0 track_count,0 duration_millis,0 downloaded_count
        FROM json_each(?1) requested JOIN tags mood ON mood.tag_key=requested.value AND mood.kind=1
        ORDER BY CAST(requested.key AS INTEGER)",
    )
    .bind(serde_json::to_string(&raw)?)
    .fetch_all(&mut *connection)
    .await?;
    let mosaics = rows
        .iter()
        .map(|row| row.mosaic.clone())
        .collect::<Vec<_>>();
    let galleries = gallery_bindings(catalog, connection, &mosaics).await?;
    Ok(rows
        .into_iter()
        .zip(galleries)
        .map(|(row, gallery)| {
            let mut mood = row.mood;
            if let Some(resident) = resident.get(row.ordinal as usize).copied().flatten() {
                mood.summary = resident.totals.summary();
            }
            mood.representative_artwork = gallery;
            mood
        })
        .collect())
}

pub(crate) async fn load_folder_rows(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    keys: &[i64],
) -> LibraryResult<Vec<FolderView>> {
    catalog.loaded().await;
    let resident = catalog.resident_collections(Kind::Folder, keys);
    #[derive(FromRow)]
    struct Row {
        ordinal: i64,
        #[sqlx(flatten)]
        folder: FolderView,
    }
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT CAST(requested.key AS INTEGER) ordinal,folder.folder_key,folder.source_key,folder.object_id,
        json_object('name',folder.name,'sort_text',folder.sort_text) metadata,{} artwork_binding,0 track_count
        FROM json_each(?1) requested JOIN folders folder ON folder.folder_key=requested.value ORDER BY CAST(requested.key AS INTEGER)",
        crate::artwork::preview_sql("folder.native_artwork")
    )))
    .bind(serde_json::to_string(keys)?)
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut folder = row.folder;
            if let Some(resident) = resident.get(row.ordinal as usize).copied().flatten() {
                folder.track_count = i64::from(resident.totals.tracks);
            }
            folder
        })
        .collect())
}

impl Catalog {
    /// Letter sections of names in the order given.
    pub async fn ordered_name_sections<'a>(
        &self,
        names: impl Iterator<Item = &'a str>,
        _cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        Ok(crate::catalog::views::sections(
            names.map(|name| Some(name.to_owned())),
        ))
    }

    pub async fn folder_section_positions(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        filter: &str,
        descending: bool,
        _cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<crate::ScrollSection>> {
        let order = self.folder_order(source, parent, filter, descending).await;
        Ok(crate::catalog::views::sections(
            order.into_iter().map(|(_, sort)| Some(sort)),
        ))
    }

    /// Child folders of a folder, or a source's top folders, by key and sort text.
    async fn folder_order(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        filter: &str,
        descending: bool,
    ) -> Vec<(i64, String)> {
        self.loaded().await;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot, false);
        let parent = match parent {
            Some(parent) => match snapshot.folders.slot(parent.raw()) {
                Some(slot) => Some(slot),
                None => return Vec::new(),
            },
            None => None,
        };
        context.folder_order(source, parent, filter, descending)
    }

    pub async fn genre_choices(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<GenreChoice>> {
        let read = crate::ListRead {
            folder,
            ..crate::ListRead::new(
                crate::ListKind::Genres,
                Some(source),
                crate::CatalogSort::Title,
            )
        };
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let order = self.list_order(&mut connection, &read).await?;
        Ok(sqlx::query_as("SELECT genre.tag_key genre_key,genre.object_id,genre.name FROM json_each(?1) requested JOIN tags genre ON genre.tag_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(order.as_ref())?)
            .fetch_all(&mut *connection)
            .await?)
    }

    /// Track URIs of a source's latest albums, album by album.
    pub async fn latest_album_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let read = crate::ListRead {
            folder,
            descending: true,
            ..crate::ListRead::new(
                crate::ListKind::Albums,
                Some(source),
                crate::CatalogSort::DateAdded,
            )
        };
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        let order = self.list_order(&mut connection, &read).await?;
        let tracks = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot, false);
            let folder = folder.and_then(|folder| snapshot.folders.slot(folder.raw()));
            let mut albums = order
                .iter()
                .take(album_limit.clamp(1, 100))
                .filter_map(|key| snapshot.albums.slot(*key).map(|slot| (*key, slot)))
                .collect::<Vec<_>>();
            albums.sort_unstable();
            let mut members = context.album_tracks(
                &albums.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
                folder,
            );
            albums
                .into_iter()
                .flat_map(|(_, slot)| members.remove(&slot).unwrap_or_default())
                .filter_map(|slot| snapshot.tracks.key_of(slot))
                .collect::<Vec<_>>()
        };
        Ok(sqlx::query_scalar("SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&tracks)?)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn album_release_candidates(
        &self,
        source: SourceKey,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumReleaseCandidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(100);
        self.loaded().await;
        let mut candidates = Vec::new();
        let mut after = 0;
        while candidates.len() < limit {
            let page: Vec<AlbumReleaseCandidate> = {
                let (_permit, mut connection) = self.acquire_general(cancellation).await?;
                sqlx::query_as("SELECT album_key,media_uri,release_identity lookup_identity FROM albums WHERE source_key=?1 AND album_key>?2 AND release_identity IS NOT NULL AND release_lookup_identity IS NOT release_identity ORDER BY album_key LIMIT 256")
                    .bind(source)
                    .bind(after)
                    .fetch_all(&mut *connection)
                    .await?
            };
            let Some(last) = page.last() else { break };
            after = last.album_key.raw();
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot, false);
            let derived = self.covers(&context);
            candidates.extend(
                page.into_iter()
                    .filter(|candidate| {
                        snapshot
                            .albums
                            .slot(candidate.album_key.raw())
                            .is_some_and(|slot| derived.albums[slot as usize].tracks > 0)
                    })
                    .take(limit - candidates.len()),
            );
        }
        Ok(candidates)
    }

    pub async fn accept_album_release_result(
        &self,
        source: SourceKey,
        album: AlbumKey,
        lookup_identity: &str,
        result: AlbumReleaseResult,
    ) -> LibraryResult<Option<AlbumKey>> {
        if lookup_identity.is_empty()
            || matches!(&result,AlbumReleaseResult::Found { release_types } if release_types.is_empty() || release_types.iter().any(|value|value.trim().is_empty()))
        {
            return Err(LibraryError::InvalidRequest(
                "invalid Album release lookup result".to_string(),
            ));
        }
        let mut write = self.write().await?;
        let Some(mut record) = write
            .get::<crate::catalog::records::Album>(album.raw())
            .await?
            .filter(|record| record.source == source)
        else {
            return Ok(None);
        };
        if record.release_identity.as_deref() != Some(lookup_identity)
            || record.release_lookup_identity.as_deref() == Some(lookup_identity)
        {
            return Ok(None);
        }
        write.keep();
        if let AlbumReleaseResult::Found { release_types } = result {
            sqlx::query("INSERT INTO album_release_observations(album_key,lookup_identity,release_types) VALUES(?1,?2,jsonb(?3)) ON CONFLICT(album_key) DO UPDATE SET lookup_identity=excluded.lookup_identity,release_types=excluded.release_types").bind(album).bind(lookup_identity).bind(serde_json::to_string(&release_types)?).execute(write.connection().await?).await?;
        }
        record.release_lookup_identity = Some(lookup_identity.to_owned());
        write.put(record, serde_json::Map::new());
        crate::catalog::albums::resolve(&mut write, &[album.raw()].into()).await?;
        write.commit().await?;
        Ok(Some(album))
    }

    pub async fn music_folders(&self, source: SourceKey) -> LibraryResult<Vec<FolderChoice>> {
        self.loaded().await;
        let keys = {
            let snapshot = self.snapshot();
            let Some(owner) = snapshot.source_slot(source) else {
                return Ok(Vec::new());
            };
            let mut folders = snapshot
                .folders
                .iter()
                .filter(|(_, folder)| folder.source == owner && folder.indexed())
                .map(|(_, folder)| (snapshot.name_sort(folder.name, folder.sort), folder.key))
                .collect::<Vec<_>>();
            folders.sort();
            folders.truncate(256);
            folders.into_iter().map(|(_, key)| key).collect::<Vec<_>>()
        };
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT folder.folder_key,folder.object_id,coalesce(folder.name,'') name FROM json_each(?1) requested JOIN folders folder ON folder.folder_key=requested.value ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&keys)?)
            .fetch_all(&mut *connection)
            .await?)
    }

    pub async fn folder_identity_key(
        &self,
        source: SourceKey,
        object_id: &str,
    ) -> LibraryResult<Option<FolderKey>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT folder_key FROM folders WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn folder_page(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<FolderView>> {
        Ok(self
            .filtered_folder_page(source, parent, "", false, offset, limit, cancellation)
            .await?
            .1)
    }

    pub async fn filtered_folder_page(
        &self,
        source: SourceKey,
        parent: Option<FolderKey>,
        filter: &str,
        descending: bool,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, Vec<FolderView>)> {
        let order = self.folder_order(source, parent, filter, descending).await;
        let keys = order
            .iter()
            .skip(offset)
            .take(limit.min(COLLECTION_ROW_LIMIT))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        let (_permit, mut connection) = self.acquire_loaded(cancellation).await?;
        Ok((
            order.len(),
            load_folder_rows(self, &mut connection, &keys).await?,
        ))
    }
}

struct AlbumSpan {
    key: AlbumKey,
    count: usize,
    first: usize,
    genres: bool,
    /// The album's visible tracks in disc order.
    tracks: Vec<crate::TrackKey>,
}
// Compact keys and counts describe layout; metadata is read only for the window.
pub struct AlbumDetailIndex {
    read: crate::ListRead,
    count: usize,
    albums: Vec<AlbumSpan>,
}

impl crate::ListRead {
    pub async fn album_detail_index(
        &self,
        catalog: &Catalog,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<std::sync::Arc<AlbumDetailIndex>> {
        let order = {
            let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
            catalog.list_order(&mut connection, self).await?
        };
        let snapshot = catalog.snapshot();
        let context = Context::new(&snapshot, false);
        let folder = self
            .folder
            .and_then(|folder| snapshot.folders.slot(folder.raw()));
        let slots = order
            .iter()
            .filter_map(|key| Some((*key, snapshot.albums.slot(*key)?)))
            .collect::<Vec<_>>();
        let mut members = context.album_tracks(
            &slots.iter().map(|(_, slot)| *slot).collect::<Vec<_>>(),
            folder,
        );
        let mut albums = Vec::with_capacity(order.len());
        let mut first = 0;
        for (key, slot) in slots {
            let tracks = members
                .remove(&slot)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|slot| snapshot.tracks.key_of(slot))
                .map(crate::TrackKey::from_raw)
                .collect::<Vec<_>>();
            let count = tracks.len();
            albums.push(AlbumSpan {
                key: AlbumKey::from_raw(key),
                count,
                first,
                genres: !snapshot.album_runs(&snapshot.albums.records[slot as usize])[1].is_empty(),
                tracks,
            });
            first += 1 + count;
        }
        Ok(std::sync::Arc::new(AlbumDetailIndex {
            read: self.clone(),
            count: first,
            albums,
        }))
    }
}

impl AlbumDetailIndex {
    pub fn spans(&self) -> impl Iterator<Item = (AlbumKey, usize, bool, usize)> + '_ {
        self.albums
            .iter()
            .map(|album| (album.key, album.count, album.genres, album.first))
    }
    pub fn matches(&self, read: &crate::ListRead) -> bool {
        &self.read == read
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub async fn page(
        &self,
        catalog: &Catalog,
        window: crate::RouteSeedWindow,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<crate::Page<AlbumDetailRow>> {
        let range = window.range(self.count);
        Ok(crate::Page {
            count: self.count,
            first_row_position: range.start,
            rows: self
                .rows(catalog, range.start, range.len(), cancellation)
                .await?,
        })
    }
    pub async fn rows(
        &self,
        catalog: &Catalog,
        offset: usize,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<AlbumDetailRow>> {
        if limit == 0 || offset >= self.count {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(limit.min(256));
        let start = self
            .albums
            .partition_point(|album| album.first <= offset)
            .saturating_sub(1);
        let spans = self.albums[start..]
            .iter()
            .take_while(|album| album.first < end)
            .collect::<Vec<_>>();
        // Positions of the window: an album header or one of its tracks.
        let mut headers = Vec::new();
        let mut tracks = Vec::new();
        for span in &spans {
            if (offset..end).contains(&span.first) {
                headers.push(span.key);
            }
            for (index, track) in span.tracks.iter().enumerate() {
                let position = span.first + 1 + index;
                if (offset..end).contains(&position) {
                    tracks.push((position, *track, index, index + 1 == span.count));
                }
            }
        }
        let (_permit, mut connection) = catalog.acquire_loaded(cancellation).await?;
        let mut rows = Vec::new();
        for album in load_album_rows(catalog, &mut connection, &headers, self.read.activity).await?
        {
            let span = spans
                .iter()
                .find(|span| span.key == album.album_key)
                .expect("requested album span");
            rows.push((
                span.first,
                AlbumDetailRow::Album {
                    album,
                    last: span.count == 0,
                },
            ));
        }
        let keys = tracks.iter().map(|(_, key, ..)| *key).collect::<Vec<_>>();
        for track in crate::tracks::load_track_rows(catalog, &mut connection, &keys).await? {
            if let Some((position, _, index, last)) =
                tracks.iter().find(|(_, key, ..)| *key == track.track_key)
            {
                rows.push((
                    *position,
                    AlbumDetailRow::Track {
                        track,
                        index: *index,
                        last: *last,
                    },
                ));
            }
        }
        rows.sort_unstable_by_key(|(position, _)| *position);
        Ok(rows.into_iter().map(|(_, row)| row).collect())
    }
}
