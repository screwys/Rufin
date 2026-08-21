//! The one selected source library kept in memory.
//!
//! Accepted source batches build this value while they are persisted; launch
//! and reopen hydrate it from the Store. Routes take short read projections
//! that clone shared item handles and immediately release the guard. Exact
//! accepted mutations replace only affected handles; selection and
//! source-session lifetime remain Rufin's responsibility.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::marker::PhantomData;
use std::ops::{Deref, Index};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use thiserror::Error;

use crate::{
    AcceptedLibraryChange, AcceptedTrackReplacement, Album, AlbumArtwork, AlbumArtworkFacts,
    AlbumId, AlbumRelations, Artist, ArtistCredit, ArtistId, FavoriteAcknowledgement,
    FavoriteItemId, Folder, FolderId, Genre, GenreCredit, GenreId, HomeFacts, LocalAccessFile,
    LocalAccessMapping, LocalFile, LocalFileKind, LocalImport, Mood, MoodId, MusicFolder,
    MusicFolderId, Playlist, PlaylistEntry, PlaylistId, PlaylistSnapshot, RecentPlay,
    SmartPlaylistId, SmartPlaylistRecord, SourceArtwork, SourceId, StoredLoudnessMeasurement,
    Track, TrackActivity, TrackId,
    activity::apply_track_activity_value,
    items::color_seed,
    local_playback::{LocalMatchKey, index_local_access},
};

#[derive(Debug, Error)]
pub enum LibraryQueryError {
    #[error("the source library lock was poisoned")]
    Unavailable,
    #[error("the accepted {kind} {id} is not present in the source library")]
    MissingItem { kind: &'static str, id: String },
    #[error("the loaded Track selection changed before playback")]
    StaleTrackSelection,
}

pub type LibraryQueryResult<T> = Result<T, LibraryQueryError>;

#[derive(Clone, Debug)]
pub(crate) struct LibraryInput {
    pub(crate) library_id: i64,
    pub(crate) source_id: Option<SourceId>,
    pub(crate) input_digest: [u8; 32],
    pub(crate) freshness: Option<crate::ProviderFreshness>,
    pub(crate) albums: Vec<Album>,
    pub(crate) tracks: Vec<Track>,
    pub(crate) artists: Vec<Artist>,
    pub(crate) genres: Vec<Genre>,
    pub(crate) music_folders: Vec<MusicFolder>,
    pub(crate) playlists: Vec<PlaylistSnapshot>,
    pub(crate) smart_playlists: Vec<SmartPlaylistRecord>,
    pub(crate) local_files: Vec<LocalFile>,
    pub(crate) local_access: Vec<LocalAccessFile>,
    pub(crate) home: Option<HomeFacts>,
    pub(crate) activity: Vec<TrackActivity>,
    pub(crate) recent_plays: Vec<RecentPlay>,
    pub(crate) local_imports: Vec<LocalImport>,
    pub(crate) local_favorites: Vec<FavoriteItemId>,
    pub(crate) loudness: Vec<crate::LoudnessMeasurementWrite>,
}

#[derive(Debug, Default)]
pub(crate) struct ItemReplacement {
    pub(crate) albums: Vec<Album>,
    pub(crate) tracks: Vec<Track>,
    pub(crate) artists: Vec<Artist>,
    pub(crate) genres: Vec<Genre>,
    pub(crate) removed_albums: Vec<AlbumId>,
    pub(crate) removed_tracks: Vec<TrackId>,
    pub(crate) removed_artists: Vec<ArtistId>,
    pub(crate) removed_genres: Vec<GenreId>,
}

#[derive(Debug)]
pub(crate) struct LocalFavoriteUpdate {
    pub(crate) targets: Vec<FavoriteItemId>,
    pub(crate) transfers: Vec<LocalFavoriteTransfer>,
}

#[derive(Clone, Debug)]
pub(crate) struct LocalFavoriteTransfer {
    pub(crate) removed: FavoriteItemId,
    pub(crate) replacement: FavoriteItemId,
}

impl ItemReplacement {
    pub(crate) fn is_empty(&self) -> bool {
        self.albums.is_empty()
            && self.tracks.is_empty()
            && self.artists.is_empty()
            && self.genres.is_empty()
            && self.removed_albums.is_empty()
            && self.removed_tracks.is_empty()
            && self.removed_artists.is_empty()
            && self.removed_genres.is_empty()
    }
}

impl LibraryInput {
    pub(crate) fn new(
        source_id: SourceId,
        library_id: i64,
        input_digest: [u8; 32],
        freshness: Option<crate::ProviderFreshness>,
    ) -> Self {
        Self {
            library_id,
            source_id: Some(source_id),
            input_digest,
            freshness,
            albums: Vec::new(),
            tracks: Vec::new(),
            artists: Vec::new(),
            genres: Vec::new(),
            music_folders: Vec::new(),
            playlists: Vec::new(),
            smart_playlists: Vec::new(),
            local_files: Vec::new(),
            local_access: Vec::new(),
            home: None,
            activity: Vec::new(),
            recent_plays: Vec::new(),
            local_imports: Vec::new(),
            local_favorites: Vec::new(),
            loudness: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedPlaylist {
    pub playlist: Arc<Playlist>,
    pub entries: Arc<[PlaylistEntry]>,
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedLocalFolder {
    pub(crate) folder: Folder,
    pub(crate) parent_id: Option<FolderId>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum LocalArtworkItemId {
    Album(AlbumSlot),
    Track(TrackSlot),
    Artist(ArtistSlot),
}

#[derive(Debug, Eq, Hash, PartialEq)]
pub(crate) struct ItemSlot<Id> {
    index: u32,
    item: PhantomData<fn() -> Id>,
}

impl<Id> Clone for ItemSlot<Id> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Id> Copy for ItemSlot<Id> {}

impl<Id> ItemSlot<Id> {
    pub(super) const fn index(self) -> usize {
        self.index as usize
    }
}

#[derive(Debug)]
pub(crate) struct LoadedItems<Id, Value> {
    by_id: HashMap<Id, ItemSlot<Id>>,
    values: Vec<Option<Value>>,
    live: usize,
}

impl<Id, Value> LoadedItems<Id, Value>
where
    Id: Clone + Eq + std::hash::Hash,
{
    fn from_values(values: HashMap<Id, Value>) -> Self {
        let mut loaded = Self {
            by_id: HashMap::with_capacity(values.len()),
            values: Vec::with_capacity(values.len()),
            live: 0,
        };
        for (id, value) in values {
            loaded.insert(id, value);
        }
        loaded
    }

    pub(crate) fn get(&self, id: &Id) -> Option<&Value> {
        self.slot(id).and_then(|slot| self.get_slot(slot))
    }

    pub(crate) fn get_mut(&mut self, id: &Id) -> Option<&mut Value> {
        let slot = self.slot(id)?;
        self.values.get_mut(slot.index as usize)?.as_mut()
    }

    pub(crate) fn slot(&self, id: &Id) -> Option<ItemSlot<Id>> {
        self.by_id.get(id).copied()
    }

    pub(crate) fn get_slot(&self, slot: ItemSlot<Id>) -> Option<&Value> {
        self.values.get(slot.index as usize)?.as_ref()
    }

    pub(crate) fn get_index(&self, index: usize) -> Option<&Value> {
        self.values.get(index)?.as_ref()
    }

    pub(crate) fn contains_key(&self, id: &Id) -> bool {
        self.get(id).is_some()
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &Id> {
        self.by_id
            .iter()
            .filter(|(_, slot)| self.get_slot(**slot).is_some())
            .map(|(id, _)| id)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &Value> {
        self.values.iter().filter_map(Option::as_ref)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Id, &Value)> {
        self.by_id
            .iter()
            .filter_map(|(id, slot)| self.get_slot(*slot).map(|value| (id, value)))
    }

    pub(crate) const fn len(&self) -> usize {
        self.live
    }

    pub(super) const fn slot_capacity(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn live_slots(&self) -> impl Iterator<Item = ItemSlot<Id>> + '_ {
        self.values
            .iter()
            .enumerate()
            .filter(|(_, value)| value.is_some())
            .map(|(index, _)| ItemSlot {
                index: index as u32,
                item: PhantomData,
            })
    }

    pub(crate) fn insert(&mut self, id: Id, value: Value) -> Option<Value> {
        if let Some(slot) = self.by_id.get(&id).copied() {
            let previous = self.values[slot.index as usize].replace(value);
            if previous.is_none() {
                self.live += 1;
            }
            return previous;
        }
        let index = u32::try_from(self.values.len())
            .expect("a loaded library cannot contain more than u32::MAX items");
        self.values.push(Some(value));
        self.live += 1;
        self.by_id.insert(
            id,
            ItemSlot {
                index,
                item: PhantomData,
            },
        );
        None
    }

    pub(crate) fn remove(&mut self, id: &Id) -> Option<Value> {
        let slot = self.by_id.remove(id)?;
        let removed = self.values[slot.index as usize].take();
        if removed.is_some() {
            self.live -= 1;
        }
        removed
    }
}

impl<Id, Value> Index<&Id> for LoadedItems<Id, Value>
where
    Id: Clone + Eq + std::hash::Hash,
{
    type Output = Value;

    fn index(&self, id: &Id) -> &Self::Output {
        self.get(id).expect("loaded item ID must resolve")
    }
}

pub(crate) type AlbumSlot = ItemSlot<AlbumId>;
pub(crate) type ArtistSlot = ItemSlot<ArtistId>;
pub(crate) type PlaylistSlot = ItemSlot<PlaylistId>;
pub(crate) type TrackSlot = ItemSlot<TrackId>;

#[derive(Clone, Debug)]
pub(crate) struct LoadedAlbum {
    pub(crate) album: Arc<Album>,
    pub(crate) artwork: AlbumArtwork,
    pub(crate) source_provided: bool,
    pub(crate) tracks: Vec<TrackSlot>,
}

impl Deref for LoadedAlbum {
    type Target = Album;

    fn deref(&self) -> &Self::Target {
        &self.album
    }
}

impl LoadedAlbum {
    fn new(album: Album, source_provided: bool) -> Self {
        let album = Arc::new(album);
        Self {
            artwork: AlbumArtwork {
                album: Arc::clone(&album),
                representative_track: None,
            },
            album,
            source_provided,
            tracks: Vec::new(),
        }
    }

    fn replace(&mut self, album: Album) {
        let album = Arc::new(album);
        self.album = Arc::clone(&album);
        self.artwork.album = album;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedArtist {
    pub(crate) artist: Arc<Artist>,
    pub(crate) artwork: crate::ArtistArtwork,
    pub(crate) source_provided: bool,
    /// Tracks that name this Artist directly. Album-level tracks are composed
    /// only when a projection asks for them.
    pub(crate) tracks: Vec<TrackSlot>,
    /// Albums that name this Artist directly. Guest-track albums are composed
    /// only when a projection asks for them.
    pub(crate) albums: Vec<AlbumSlot>,
}

impl Deref for LoadedArtist {
    type Target = Artist;

    fn deref(&self) -> &Self::Target {
        &self.artist
    }
}

impl LoadedArtist {
    fn new(artist: Artist, source_provided: bool) -> Self {
        let artist = Arc::new(artist);
        Self {
            artwork: crate::ArtistArtwork {
                artist: Arc::clone(&artist),
                representative_albums: Arc::default(),
            },
            artist,
            source_provided,
            tracks: Vec::new(),
            albums: Vec::new(),
        }
    }

    fn replace(&mut self, artist: Artist) {
        let artist = Arc::new(artist);
        self.artist = Arc::clone(&artist);
        self.artwork.artist = artist;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedGenre {
    pub(crate) genre: Arc<Genre>,
    pub(crate) source_provided: bool,
    /// Tracks carrying this Genre directly.
    pub(crate) tracks: Vec<TrackSlot>,
    /// Albums carrying this Genre directly.
    pub(crate) albums: Vec<AlbumSlot>,
}

impl Deref for LoadedGenre {
    type Target = Genre;

    fn deref(&self) -> &Self::Target {
        &self.genre
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedMood {
    pub(crate) mood: Arc<Mood>,
    pub(crate) tracks: Vec<TrackSlot>,
}

impl Deref for LoadedMood {
    type Target = Mood;

    fn deref(&self) -> &Self::Target {
        &self.mood
    }
}

#[derive(Debug)]
pub(crate) struct LoadedState {
    pub(crate) albums: LoadedItems<AlbumId, LoadedAlbum>,
    pub(crate) tracks: LoadedItems<TrackId, Track>,
    pub(crate) artists: LoadedItems<ArtistId, LoadedArtist>,
    pub(crate) genres: LoadedItems<GenreId, LoadedGenre>,
    pub(crate) moods: LoadedItems<MoodId, LoadedMood>,
    pub(crate) music_folders: LoadedItems<MusicFolderId, Arc<MusicFolder>>,
    pub(crate) playlists: LoadedItems<PlaylistId, Arc<LoadedPlaylist>>,
    pub(crate) smart_playlists: HashMap<SmartPlaylistId, Arc<crate::SmartPlaylist>>,
    pub(crate) music_folder_tracks: HashMap<MusicFolderId, Vec<TrackSlot>>,
    pub(crate) local_folders: HashMap<FolderId, LoadedLocalFolder>,
    pub(crate) local_folder_children: HashMap<Option<FolderId>, Vec<FolderId>>,
    pub(crate) local_folder_tracks: HashMap<Option<FolderId>, Vec<TrackSlot>>,
    pub(crate) track_playlists: HashMap<TrackId, Vec<PlaylistSlot>>,
    pub(crate) freshness: Option<crate::ProviderFreshness>,
    pub(crate) local_files: HashMap<String, LocalFile>,
    pub(crate) path_tracks: HashMap<String, HashSet<TrackSlot>>,
    pub(crate) cue_dependents: HashMap<String, HashSet<String>>,
    pub(crate) directory_children: HashMap<String, HashSet<String>>,
    pub(crate) directory_images: HashMap<String, HashSet<String>>,
    pub(crate) directory_media_counts: HashMap<String, usize>,
    pub(crate) artwork_items: HashMap<String, HashSet<LocalArtworkItemId>>,
    pub(crate) local_access_mapping: Option<LocalAccessMapping>,
    pub(crate) local_access: Vec<LocalAccessFile>,
    pub(crate) local_access_paths: HashSet<PathBuf>,
    pub(crate) local_access_index: HashMap<LocalMatchKey, Vec<usize>>,
    pub(crate) downloaded_files: HashMap<TrackId, PathBuf>,
    pub(crate) download_coverage: crate::download_coverage::DownloadCoverage,
    pub(crate) home_facts: HomeFacts,
    pub(crate) activity: HashMap<TrackId, TrackActivity>,
    pub(crate) recent_plays: Vec<RecentPlay>,
    pub(crate) local_imports: HashMap<TrackSlot, i64>,
    pub(crate) track_loudness: HashMap<TrackId, StoredLoudnessMeasurement>,
    pub(crate) album_loudness: HashMap<AlbumId, StoredLoudnessMeasurement>,
}

/// One accepted source-scoped library.
///
/// The Store lane and Home session owner are bound when this source is loaded,
/// so source-scoped reads and accepted writes cannot be paired with the wrong
/// Store handle. It deliberately contains no selected pointer, source client,
/// session epoch, or route subscription state.
pub struct Library {
    pub(crate) store: crate::store::StoreLane,
    pub(crate) home_sessions: Arc<crate::home::HomeSessions>,
    source_id: SourceId,
    library_id: i64,
    input_digest: [u8; 32],
    state: RwLock<LoadedState>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Library")
            .field("source_id", &self.source_id)
            .field("library_id", &self.library_id)
            .field("input_digest", &self.input_digest)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LibraryCounts {
    pub albums: usize,
    pub tracks: usize,
}

fn search_fields_match<const N: usize>(fields: [&str; N], terms: &[String]) -> bool {
    let fields = fields.map(str::to_lowercase);
    terms
        .iter()
        .all(|term| fields.iter().any(|field| field.contains(term)))
}

fn retain_search_result<T>(
    results: &mut BTreeMap<(String, String), T>,
    title: &str,
    id: &str,
    item: T,
    limit: usize,
) {
    results.insert((title.to_lowercase(), id.to_string()), item);
    if results.len() > limit {
        results.pop_last();
    }
}

impl Library {
    pub(crate) fn build(
        mut input: LibraryInput,
        store: crate::store::StoreLane,
        home_sessions: Arc<crate::home::HomeSessions>,
    ) -> LibraryQueryResult<Arc<Self>> {
        let source_id = input
            .source_id
            .take()
            .ok_or_else(|| LibraryQueryError::MissingItem {
                kind: "source",
                id: String::new(),
            })?;
        let home_facts = input
            .home
            .take()
            .ok_or_else(|| LibraryQueryError::MissingItem {
                kind: "Home facts",
                id: source_id.to_string(),
            })?;

        for album in &mut input.albums {
            album.color_seed = color_seed(album.id.as_str());
        }
        let mut albums = values_by_id(input.albums, |album| album.id.clone());
        let mut tracks = values_by_id(input.tracks, |track| track.id.clone());
        let mut artists = values_by_id(input.artists, |artist| artist.id.clone());
        let mut genres = values_by_id(input.genres, |genre| genre.id.clone());
        let music_folders = values_by_id(input.music_folders, |folder| folder.id.clone());

        let activity = input
            .activity
            .into_iter()
            .filter(|activity| tracks.contains_key(&activity.track_id))
            .map(|activity| (activity.track_id.clone(), activity))
            .collect::<HashMap<_, _>>();
        if home_facts.is_rufin_defined() {
            apply_track_activity(&mut tracks, activity.values());
        }
        let (cue_dependents, directory_children, directory_images) =
            index_local_file_relations(&input.local_files);
        let directory_media_counts =
            index_local_media_directories(tracks.values(), &input.local_files);
        let local_files = values_by_id(input.local_files, |file| file.path.clone());
        let (local_folders, local_folder_children) = index_local_folders(local_files.values());
        let (local_access_paths, local_access_index) =
            index_local_access(&input.local_access, None);
        let local_favorites = input.local_favorites;

        apply_favorite_values(&mut albums, &mut tracks, &mut artists, &local_favorites);

        let playlists = build_playlists(input.playlists);
        let track_playlists = index_track_playlists(&playlists);
        let smart_playlists = build_smart_playlists(input.smart_playlists);
        let mut state = LoadedState {
            albums: LoadedItems::from_values(HashMap::new()),
            tracks: LoadedItems::from_values(HashMap::new()),
            artists: LoadedItems::from_values(HashMap::new()),
            genres: LoadedItems::from_values(HashMap::new()),
            moods: LoadedItems::from_values(HashMap::new()),
            music_folders: LoadedItems::from_values(shared_values(music_folders)),
            playlists,
            smart_playlists,
            music_folder_tracks: HashMap::new(),
            local_folders,
            local_folder_children,
            local_folder_tracks: HashMap::new(),
            track_playlists,
            freshness: input.freshness,
            local_files,
            path_tracks: HashMap::new(),
            cue_dependents,
            directory_children,
            directory_images,
            directory_media_counts,
            artwork_items: HashMap::new(),
            local_access_mapping: None,
            local_access: input.local_access,
            local_access_paths,
            local_access_index,
            downloaded_files: HashMap::new(),
            download_coverage: crate::download_coverage::DownloadCoverage::default(),
            home_facts,
            activity,
            recent_plays: input.recent_plays,
            local_imports: HashMap::new(),
            track_loudness: HashMap::new(),
            album_loudness: HashMap::new(),
        };
        for artist in artists.drain().map(|(_, artist)| artist) {
            state
                .artists
                .insert(artist.id.clone(), LoadedArtist::new(artist, true));
        }
        for genre in genres.drain().map(|(_, genre)| genre) {
            state.genres.insert(
                genre.id.clone(),
                LoadedGenre {
                    genre: Arc::new(genre),
                    source_provided: true,
                    tracks: Vec::new(),
                    albums: Vec::new(),
                },
            );
        }
        for album in albums.drain().map(|(_, album)| album) {
            let id = album.id.clone();
            let album_for_indexes = album.clone();
            state.albums.insert(id, LoadedAlbum::new(album, true));
            add_album_to_indexes(&mut state, &album_for_indexes);
        }
        for mut track in tracks.drain().map(|(_, track)| track) {
            ensure_track_rows(&mut state, &track);
            track.album_artwork = track.album_id.as_ref().and_then(|album_id| {
                state
                    .albums
                    .get(album_id)
                    .map(|album| Arc::new(AlbumArtworkFacts::from(album.album.as_ref())))
            });
            let id = track.id.clone();
            state.tracks.insert(id, track.clone());
            add_track_to_indexes(&mut state, &track);
        }
        let sparse_album_ids = state
            .albums
            .iter()
            .filter(|(_, album)| !album.source_provided)
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        for album_id in &sparse_album_ids {
            if let Some(album) = state
                .albums
                .get(album_id)
                .map(|album| album.album.as_ref().clone())
            {
                remove_album_from_indexes(&mut state, &album);
            }
            refresh_sparse_album(&mut state, album_id);
            if let Some(album) = state
                .albums
                .get(album_id)
                .map(|album| album.album.as_ref().clone())
            {
                add_album_to_indexes(&mut state, &album);
            }
        }
        relink_tracks_to_albums(&mut state, &sparse_album_ids, &mut HashSet::new());
        let artwork_albums = state.albums.iter().map(|(id, _)| id.clone()).collect();
        let artwork_artists = state.artists.iter().map(|(id, _)| id.clone()).collect();
        crate::browse::organize_artwork_bindings(&mut state, &artwork_albums, &artwork_artists);
        for import in input.local_imports {
            if let Some(slot) = state.tracks.slot(&import.track_id) {
                state.local_imports.insert(slot, import.first_seen_at);
            }
        }
        state.artwork_items = index_local_artwork(&state.albums, &state.tracks, &state.artists);
        apply_sparse_favorites(&mut state, local_favorites);
        crate::download_coverage::rebuild_download_coverage(&mut state);
        hydrate_loudness(&mut state, input.loudness);
        Ok(Arc::new(Self {
            store,
            home_sessions,
            source_id,
            library_id: input.library_id,
            input_digest: input.input_digest,
            state: RwLock::new(state),
        }))
    }

    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    pub const fn library_id(&self) -> i64 {
        self.library_id
    }

    pub const fn input_digest(&self) -> &[u8; 32] {
        &self.input_digest
    }

    pub fn provider_freshness(&self) -> LibraryQueryResult<Option<crate::ProviderFreshness>> {
        Ok(self.read()?.freshness.clone())
    }

    pub fn counts(&self) -> LibraryQueryResult<LibraryCounts> {
        let state = self.read()?;
        Ok(LibraryCounts {
            albums: state.albums.len(),
            tracks: state.tracks.len(),
        })
    }

    /// Returns every unique source-owned image the selected library can show.
    ///
    /// The first part interleaves the ordinary route fronts so a large album
    /// collection cannot consume the decoded-cache budget before Artists,
    /// Genres, or Playlists. The remainder stays deterministic.
    pub fn source_artwork(&self) -> LibraryQueryResult<Arc<[SourceArtwork]>> {
        const ROUTE_FRONT: usize = 64;

        let state = self.read()?;
        let mut album_artwork = Vec::new();
        let mut track_artwork = Vec::new();
        let mut artist_artwork = Vec::new();
        let mut genre_artwork = Vec::new();
        let mut playlist_artwork = Vec::new();

        let mut albums = state.albums.values().collect::<Vec<_>>();
        albums.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then_with(|| left.id.cmp(&right.id))
        });
        for album in albums {
            push_raw_source_artwork(
                &mut album_artwork,
                album.local_artwork.clone().map(SourceArtwork::Local),
            );
            push_raw_source_artwork(
                &mut album_artwork,
                album.image_ref.clone().map(SourceArtwork::Native),
            );
        }

        let mut tracks = state.tracks.values().collect::<Vec<_>>();
        tracks.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then_with(|| left.id.cmp(&right.id))
        });
        for track in tracks {
            let resolved_album_image = track
                .album_artwork
                .as_ref()
                .is_some_and(|album| album.local_artwork.is_some() || album.image_ref.is_some());
            if resolved_album_image {
                continue;
            }
            push_raw_source_artwork(
                &mut track_artwork,
                track.local_artwork.clone().map(SourceArtwork::Local),
            );
            push_raw_source_artwork(
                &mut track_artwork,
                track.image_ref.clone().map(SourceArtwork::Native),
            );
        }

        let mut artists = state.artists.values().collect::<Vec<_>>();
        artists.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        for artist in artists {
            push_raw_source_artwork(
                &mut artist_artwork,
                artist.local_artwork.clone().map(SourceArtwork::Local),
            );
            push_raw_source_artwork(
                &mut artist_artwork,
                artist.image_ref.clone().map(SourceArtwork::Native),
            );
        }

        let mut genres = state.genres.values().collect::<Vec<_>>();
        genres.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        for genre in genres {
            push_raw_source_artwork(
                &mut genre_artwork,
                genre.image_ref.clone().map(SourceArtwork::Native),
            );
        }

        let mut playlists = state.playlists.values().collect::<Vec<_>>();
        playlists.sort_by(|left, right| {
            left.playlist
                .name
                .cmp(&right.playlist.name)
                .then_with(|| left.playlist.id.cmp(&right.playlist.id))
        });
        for playlist in playlists {
            push_raw_source_artwork(
                &mut playlist_artwork,
                playlist
                    .playlist
                    .image_ref
                    .clone()
                    .map(SourceArtwork::Native),
            );
        }

        let families = [
            album_artwork,
            track_artwork,
            artist_artwork,
            genre_artwork,
            playlist_artwork,
        ];
        let mut artwork = Vec::new();
        let mut seen = HashSet::new();
        for family in &families {
            for item in family.iter().take(ROUTE_FRONT) {
                push_source_artwork(&mut artwork, &mut seen, Some(item.clone()));
            }
        }
        for family in &families {
            for item in family.iter().skip(ROUTE_FRONT) {
                push_source_artwork(&mut artwork, &mut seen, Some(item.clone()));
            }
        }

        Ok(artwork.into())
    }

    pub fn album(&self, id: &AlbumId) -> LibraryQueryResult<Option<Arc<Album>>> {
        Ok(self
            .read()?
            .albums
            .get(id)
            .map(|album| Arc::clone(&album.album)))
    }

    pub fn track(&self, id: &TrackId) -> LibraryQueryResult<Option<Track>> {
        Ok(self.read()?.tracks.get(id).cloned())
    }

    pub fn loudness_for_track(&self, id: &TrackId) -> LibraryQueryResult<crate::TrackLoudness> {
        let state = self.read()?;
        let Some(track) = state.tracks.get(id) else {
            return Ok(crate::TrackLoudness::default());
        };
        let track_loudness = state
            .track_loudness
            .get(id)
            .map(|stored| stored.measurement);
        let album_loudness = match track.album_id.as_ref() {
            Some(album_id) => state
                .album_loudness
                .get(album_id)
                .map(|stored| stored.measurement),
            None => track_loudness,
        };
        Ok(crate::TrackLoudness {
            track: track_loudness,
            album: album_loudness,
        })
    }

    pub fn loudness_analysis_snapshot(
        &self,
    ) -> LibraryQueryResult<crate::LoudnessAnalysisSnapshot> {
        let state = self.read()?;
        let mut tracks = state.tracks.values().cloned().collect::<Vec<_>>();
        tracks.sort_by(|left, right| {
            left.album_id
                .cmp(&right.album_id)
                .then_with(|| left.disc_number.cmp(&right.disc_number))
                .then_with(|| left.track_number.cmp(&right.track_number))
                .then_with(|| left.id.cmp(&right.id))
        });
        let tracks = tracks
            .into_iter()
            .map(|track| crate::LoudnessTrackInput {
                analysis_key: loudness_track_key(&state, &track),
                current: state
                    .track_loudness
                    .get(&track.id)
                    .map(|stored| stored.measurement),
                track,
            })
            .collect::<Vec<_>>();

        let mut album_ids = state
            .albums
            .iter()
            .filter(|(_, album)| !album.tracks.is_empty())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        album_ids.sort();
        let albums = album_ids
            .into_iter()
            .filter_map(|album_id| {
                let (analysis_key, track_ids) = loudness_album_key(&state, &album_id)?;
                Some(crate::LoudnessAlbumInput {
                    current: state
                        .album_loudness
                        .get(&album_id)
                        .map(|stored| stored.measurement),
                    album_id,
                    analysis_key,
                    track_ids: track_ids.into(),
                })
            })
            .collect::<Vec<_>>();

        Ok(crate::LoudnessAnalysisSnapshot {
            tracks: tracks.into(),
            albums: albums.into(),
        })
    }

    pub fn store_loudness(
        &self,
        writes: Vec<crate::LoudnessMeasurementWrite>,
    ) -> crate::LibraryResult<()> {
        self.store
            .replace_loudness(self.source_id.clone(), self.library_id, writes.clone())?;
        let mut state = self.write()?;
        for write in writes {
            let stored = StoredLoudnessMeasurement {
                analysis_key: write.analysis_key,
                measurement: write.measurement,
            };
            match write.item {
                crate::LoudnessItemId::Track(track_id) => {
                    if state.tracks.get(&track_id).is_some_and(|track| {
                        loudness_track_key(&state, track) == write.analysis_key
                    }) {
                        state.track_loudness.insert(track_id, stored);
                    }
                }
                crate::LoudnessItemId::Album(album_id) => {
                    if loudness_album_key(&state, &album_id)
                        .is_some_and(|(key, _)| key == write.analysis_key)
                    {
                        state.album_loudness.insert(album_id, stored);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn artist(&self, id: &ArtistId) -> LibraryQueryResult<Option<Arc<Artist>>> {
        Ok(self
            .read()?
            .artists
            .get(id)
            .map(|artist| Arc::clone(&artist.artist)))
    }

    pub fn search(
        &self,
        request: &crate::SearchRequest,
    ) -> LibraryQueryResult<crate::SearchResults> {
        let terms = request
            .query()
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| !term.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return Ok(crate::SearchResults::default());
        }
        let limit = request.limit();
        let (artists, albums, tracks) = {
            let state = self.read()?;
            let mut artists = BTreeMap::new();
            for artist in state.artists.values() {
                if search_fields_match([artist.name.as_str()], &terms) {
                    retain_search_result(
                        &mut artists,
                        &artist.name,
                        artist.id.as_str(),
                        artist.artist.as_ref().clone(),
                        limit,
                    );
                }
            }
            let mut albums = BTreeMap::new();
            for album in state.albums.values() {
                if search_fields_match([album.title.as_str(), album.artist.as_str()], &terms) {
                    retain_search_result(
                        &mut albums,
                        &album.title,
                        album.id.as_str(),
                        album.album.as_ref().clone(),
                        limit,
                    );
                }
            }
            let mut tracks = BTreeMap::new();
            for track in state.tracks.values() {
                if search_fields_match(
                    [
                        track.title.as_str(),
                        track.artist.as_str(),
                        track.album.as_str(),
                    ],
                    &terms,
                ) {
                    retain_search_result(
                        &mut tracks,
                        &track.title,
                        track.id.as_str(),
                        track.clone(),
                        limit,
                    );
                }
            }
            (
                artists.into_values().collect(),
                albums.into_values().collect(),
                tracks.into_values().collect(),
            )
        };
        Ok(crate::SearchResults {
            artists,
            albums,
            tracks,
        })
    }

    pub fn genre(&self, id: &GenreId) -> LibraryQueryResult<Option<Arc<Genre>>> {
        Ok(self
            .read()?
            .genres
            .get(id)
            .map(|genre| Arc::clone(&genre.genre)))
    }

    pub fn contains_playlist(&self, id: &PlaylistId) -> LibraryQueryResult<bool> {
        Ok(self.read()?.playlists.contains_key(id))
    }

    pub fn contains_music_folder(&self, id: &MusicFolderId) -> LibraryQueryResult<bool> {
        Ok(self.read()?.music_folders.contains_key(id))
    }

    pub(crate) fn playlist(
        &self,
        id: &PlaylistId,
    ) -> LibraryQueryResult<Option<Arc<LoadedPlaylist>>> {
        Ok(self.read()?.playlists.get(id).cloned())
    }

    pub fn resolve_tracks(
        &self,
        ids: impl IntoIterator<Item = TrackId>,
    ) -> LibraryQueryResult<Vec<Track>> {
        let state = self.read()?;
        Ok(ids
            .into_iter()
            .filter_map(|id| state.tracks.get(&id).cloned())
            .collect())
    }

    pub(crate) fn replace_provider_freshness(
        &self,
        freshness: Option<crate::ProviderFreshness>,
    ) -> LibraryQueryResult<()> {
        self.write()?.freshness = freshness;
        Ok(())
    }

    pub(crate) fn replace_activity_snapshot(
        &self,
        activity: Vec<TrackActivity>,
        recent_plays: Vec<RecentPlay>,
    ) -> LibraryQueryResult<()> {
        let mut state = self.write()?;
        let activity = activity
            .into_iter()
            .filter(|activity| state.tracks.contains_key(&activity.track_id))
            .map(|activity| (activity.track_id.clone(), activity))
            .collect::<HashMap<_, _>>();
        if state.home_facts.is_rufin_defined() {
            for accepted in activity.values() {
                if let Some(track) = state.tracks.get_mut(&accepted.track_id) {
                    apply_track_activity_value(track, accepted);
                }
            }
        }
        state.activity = activity;
        state.recent_plays = recent_plays;
        crate::download_coverage::rebuild_smart_playlist_download_coverage(&mut state);
        Ok(())
    }

    pub(crate) fn replace_favorite(
        &self,
        item_id: &FavoriteItemId,
        favorite: bool,
    ) -> LibraryQueryResult<AcceptedLibraryChange> {
        let mut state = self.write()?;
        let change = match item_id {
            FavoriteItemId::Track(id) => replace_track_favorite(&mut state, id, favorite),
            FavoriteItemId::Album(id) => {
                let current = state
                    .albums
                    .get(id)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "album",
                        id: id.to_string(),
                    })?
                    .clone();
                if current.favorite != favorite {
                    let mut replacement = current.album.as_ref().clone();
                    replacement.favorite = favorite;
                    state
                        .albums
                        .get_mut(id)
                        .expect("favorite Album row still exists")
                        .album = Arc::new(replacement);
                }
                Ok(AcceptedLibraryChange::default())
            }
            FavoriteItemId::Artist(id) => {
                let current = state
                    .artists
                    .get(id)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "artist",
                        id: id.to_string(),
                    })?
                    .clone();
                if current.favorite != favorite {
                    let mut replacement = current.artist.as_ref().clone();
                    replacement.favorite = favorite;
                    state
                        .artists
                        .get_mut(id)
                        .expect("favorite Artist row still exists")
                        .artist = Arc::new(replacement);
                }
                Ok(AcceptedLibraryChange::default())
            }
        }?;
        if matches!(item_id, FavoriteItemId::Track(_)) {
            crate::download_coverage::rebuild_smart_playlist_download_coverage(&mut state);
        }
        Ok(AcceptedLibraryChange {
            favorite: Some(FavoriteAcknowledgement {
                item: item_id.clone(),
                favorite,
            }),
            ..change
        })
    }

    pub(crate) fn replace_rating(
        &self,
        item: &FavoriteItemId,
        rating: Option<u8>,
    ) -> LibraryQueryResult<AcceptedLibraryChange> {
        let mut state = self.write()?;
        let mut replacement = ItemReplacement::default();
        match item {
            FavoriteItemId::Track(id) => {
                let mut track = state
                    .tracks
                    .get(id)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "track",
                        id: id.to_string(),
                    })?
                    .clone();
                track.user_rating = rating;
                replacement.tracks.push(track);
            }
            FavoriteItemId::Album(id) => {
                let mut album = state
                    .albums
                    .get(id)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "album",
                        id: id.to_string(),
                    })?
                    .album
                    .as_ref()
                    .clone();
                album.user_rating = rating;
                replacement.albums.push(album);
            }
            FavoriteItemId::Artist(id) => {
                let mut artist = state
                    .artists
                    .get(id)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "artist",
                        id: id.to_string(),
                    })?
                    .artist
                    .as_ref()
                    .clone();
                artist.user_rating = rating;
                replacement.artists.push(artist);
            }
        }
        let change = apply_item_replacement(&mut state, replacement)?;
        if matches!(item, FavoriteItemId::Track(_)) {
            crate::download_coverage::rebuild_smart_playlist_download_coverage(&mut state);
        }
        Ok(change)
    }

    pub(crate) fn replace_track_activity(
        &self,
        activity: TrackActivity,
        recent_play: Option<RecentPlay>,
    ) -> LibraryQueryResult<Option<AcceptedLibraryChange>> {
        let mut state = self.write()?;
        let track_id = activity.track_id.clone();
        let previous_activity = state.activity.get(&track_id).cloned();
        let activity_changed = previous_activity.as_ref() != Some(&activity);
        let mut recent_changed = false;
        if let Some(recent_play) = recent_play {
            let mut recent_plays = state.recent_plays.clone();
            if !recent_plays
                .iter()
                .any(|accepted| accepted.play_id == recent_play.play_id)
            {
                recent_plays.push(recent_play);
                recent_plays.sort_by(|left, right| {
                    right
                        .played_at
                        .cmp(&left.played_at)
                        .then_with(|| right.play_id.cmp(&left.play_id))
                });
                recent_plays.truncate(100);
                if recent_plays != state.recent_plays {
                    state.recent_plays = recent_plays;
                    recent_changed = true;
                }
            }
        }
        let Some(current) = state.tracks.get(&track_id).cloned() else {
            state.activity.remove(&track_id);
            return Ok(None);
        };
        if !activity_changed && !recent_changed {
            return Ok(None);
        }

        state.activity.insert(track_id.clone(), activity.clone());
        let (affected_smart_playlists, smart_playlist_memberships) =
            crate::smart_playlists::changed_by_activity(
                &state.smart_playlists,
                &current,
                previous_activity.as_ref(),
                &activity,
            );
        let mut replacement = current.clone();
        if state.home_facts.is_rufin_defined() {
            apply_track_activity_value(&mut replacement, &activity);
            state.tracks.insert(track_id.clone(), replacement.clone());
        }
        if activity_changed && !smart_playlist_memberships.is_empty() {
            crate::download_coverage::replace_smart_playlist_download_memberships(
                &mut state,
                &current,
                &smart_playlist_memberships,
            );
        }

        Ok(Some(AcceptedLibraryChange {
            tracks: state
                .home_facts
                .is_rufin_defined()
                .then_some(AcceptedTrackReplacement {
                    id: track_id,
                    track: Some(replacement),
                    activity_only: true,
                })
                .into_iter()
                .collect(),
            smart_playlists: sorted_set(affected_smart_playlists),
            history_changed: recent_changed,
            ..AcceptedLibraryChange::default()
        }))
    }

    pub(crate) fn favorite_value_if_derived(
        &self,
        item_id: &FavoriteItemId,
    ) -> LibraryQueryResult<Option<crate::favorites::FavoriteValue>> {
        let state = self.read()?;
        match item_id {
            FavoriteItemId::Track(id) => {
                state
                    .tracks
                    .contains_key(id)
                    .then_some(None)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "track",
                        id: id.to_string(),
                    })
            }
            FavoriteItemId::Album(id) => {
                let album = state
                    .albums
                    .get(id)
                    .ok_or_else(|| LibraryQueryError::MissingItem {
                        kind: "album",
                        id: id.to_string(),
                    })?;
                Ok((!album.source_provided)
                    .then(|| crate::favorites::FavoriteValue::Album(album.album.as_ref().clone())))
            }
            FavoriteItemId::Artist(id) => {
                let artist =
                    state
                        .artists
                        .get(id)
                        .ok_or_else(|| LibraryQueryError::MissingItem {
                            kind: "artist",
                            id: id.to_string(),
                        })?;
                Ok((!artist.source_provided).then(|| {
                    crate::favorites::FavoriteValue::Artist(artist.artist.as_ref().clone())
                }))
            }
        }
    }

    pub(crate) fn replace_album_release(
        &self,
        id: &AlbumId,
        release_types: Vec<String>,
    ) -> LibraryQueryResult<AcceptedLibraryChange> {
        let mut state = self.write()?;
        let current = state
            .albums
            .get(id)
            .ok_or_else(|| LibraryQueryError::MissingItem {
                kind: "album",
                id: id.to_string(),
            })?
            .clone();
        let mut replacement = current.album.as_ref().clone();
        replacement.release_types = release_types;
        let replacement = Arc::new(replacement);
        state
            .albums
            .get_mut(id)
            .expect("release Album row still exists")
            .album = replacement;
        let artist_releases = sorted_set(album_artist_ids(&state, id));
        Ok(AcceptedLibraryChange {
            album_releases: vec![id.clone()],
            artist_releases,
            ..AcceptedLibraryChange::default()
        })
    }

    pub(crate) fn replace_playlist(
        &self,
        snapshot: PlaylistSnapshot,
    ) -> LibraryQueryResult<Arc<LoadedPlaylist>> {
        let mut state = self.write()?;
        let playlist = replace_playlist_in_state(&mut state, snapshot);
        crate::download_coverage::rebuild_download_coverage(&mut state);
        Ok(playlist)
    }

    pub(crate) fn remove_playlist(
        &self,
        id: &PlaylistId,
    ) -> LibraryQueryResult<Option<Arc<LoadedPlaylist>>> {
        let mut state = self.write()?;
        let playlist = remove_playlist_in_state(&mut state, id);
        if playlist.is_some() {
            crate::download_coverage::rebuild_download_coverage(&mut state);
        }
        Ok(playlist)
    }

    pub(crate) fn replace_source_update(
        &self,
        replacement: ItemReplacement,
        playlists: Vec<PlaylistSnapshot>,
        removed_playlists: Vec<PlaylistId>,
    ) -> LibraryQueryResult<AcceptedLibraryChange> {
        let mut state = self.write()?;
        let mut explicit_playlists = HashSet::new();
        for playlist_id in removed_playlists {
            remove_playlist_in_state(&mut state, &playlist_id);
            explicit_playlists.insert(playlist_id);
        }
        for snapshot in playlists {
            explicit_playlists.insert(snapshot.playlist.id.clone());
            replace_playlist_in_state(&mut state, snapshot);
        }
        let mut accepted = apply_item_replacement(&mut state, replacement)?;
        accepted.playlists.extend(sorted_set(explicit_playlists));
        crate::download_coverage::rebuild_download_coverage(&mut state);
        Ok(accepted)
    }

    pub(crate) fn keep_changed_source_update(
        &self,
        replacement: &mut ItemReplacement,
        playlists: &mut Vec<PlaylistSnapshot>,
        removed_playlists: &mut Vec<PlaylistId>,
    ) -> LibraryQueryResult<()> {
        let state = self.read()?;
        replacement.albums.retain(|album| {
            state
                .albums
                .get(&album.id)
                .is_none_or(|current| !same_source_album(current, album))
        });
        replacement.tracks.retain(|track| {
            state
                .tracks
                .get(&track.id)
                .is_none_or(|current| !same_source_track(current, track))
        });
        replacement.artists.retain(|artist| {
            state
                .artists
                .get(&artist.id)
                .is_none_or(|current| current.artist.as_ref() != artist)
        });
        replacement.genres.retain(|genre| {
            state
                .genres
                .get(&genre.id)
                .is_none_or(|current| current.genre.as_ref() != genre)
        });
        replacement
            .removed_albums
            .retain(|id| state.albums.contains_key(id));
        replacement
            .removed_tracks
            .retain(|id| state.tracks.contains_key(id));
        replacement
            .removed_artists
            .retain(|id| state.artists.contains_key(id));
        replacement
            .removed_genres
            .retain(|id| state.genres.contains_key(id));
        playlists.retain(|snapshot| {
            state
                .playlists
                .get(&snapshot.playlist.id)
                .is_none_or(|current| {
                    current.playlist.as_ref() != &snapshot.playlist
                        || current.entries.as_ref() != snapshot.entries.as_slice()
                })
        });
        removed_playlists.retain(|id| state.playlists.contains_key(id));
        Ok(())
    }

    pub(crate) fn replace_local_component(
        &self,
        files: Vec<LocalFile>,
        removed_paths: Vec<String>,
        mut replacement: ItemReplacement,
        imports: Vec<LocalImport>,
        favorites: Vec<FavoriteItemId>,
        activity: Vec<TrackActivity>,
    ) -> LibraryQueryResult<AcceptedLibraryChange> {
        let mut state = self.write()?;
        apply_track_activity_to_replacement(&mut replacement.tracks, activity);
        apply_favorites_to_replacement(&mut replacement, &favorites);
        let mut affected_local_folders = HashSet::new();
        for path in removed_paths {
            if let Some(file) = state.local_files.remove(&path) {
                if file.kind == LocalFileKind::Media && path_has_source_track(&state, &path) {
                    remove_media_directory_file(&mut state, &file);
                }
                collect_local_folder_effects(&file, &mut affected_local_folders);
                remove_local_file_relations(&mut state, &file);
            }
        }
        for file in files {
            let path = file.path.clone();
            if let Some(previous) = state.local_files.remove(&path) {
                collect_local_folder_effects(&previous, &mut affected_local_folders);
                remove_local_file_relations(&mut state, &previous);
            }
            collect_local_folder_effects(&file, &mut affected_local_folders);
            add_local_file_relations(&mut state, &file);
            state.local_files.insert(path, file);
        }
        let mut accepted = apply_item_replacement(&mut state, replacement)?;
        for import in imports {
            if let Some(slot) = state.tracks.slot(&import.track_id) {
                state.local_imports.insert(slot, import.first_seen_at);
            }
        }
        accepted.local_folders_changed |= !affected_local_folders.is_empty();
        apply_sparse_favorites(&mut state, favorites);
        crate::download_coverage::rebuild_download_coverage(&mut state);
        Ok(accepted)
    }

    pub(crate) fn local_favorite_update(
        &self,
        replacement: &ItemReplacement,
    ) -> LibraryQueryResult<LocalFavoriteUpdate> {
        let state = self.read()?;
        let mut targets = HashSet::new();
        for id in &replacement.removed_tracks {
            targets.insert(FavoriteItemId::Track(id.clone()));
            if let Some(track) = state.tracks.get(id) {
                collect_track_favorite_targets(track, &mut targets);
            }
        }
        for track in &replacement.tracks {
            targets.insert(FavoriteItemId::Track(track.id.clone()));
            if let Some(previous) = state.tracks.get(&track.id) {
                collect_track_favorite_targets(previous, &mut targets);
            }
            collect_track_favorite_targets(track, &mut targets);
        }
        for id in &replacement.removed_albums {
            targets.insert(FavoriteItemId::Album(id.clone()));
            if let Some(album) = state.albums.get(id) {
                collect_album_favorite_targets(album, &mut targets);
            }
        }
        for album in &replacement.albums {
            targets.insert(FavoriteItemId::Album(album.id.clone()));
            if let Some(previous) = state.albums.get(&album.id) {
                collect_album_favorite_targets(previous, &mut targets);
            }
            collect_album_favorite_targets(album, &mut targets);
        }
        for id in &replacement.removed_artists {
            targets.insert(FavoriteItemId::Artist(id.clone()));
        }
        for artist in &replacement.artists {
            targets.insert(FavoriteItemId::Artist(artist.id.clone()));
        }
        let transfers = local_favorite_transfers(&state, replacement);
        targets.extend(
            transfers
                .iter()
                .map(|transfer| transfer.replacement.clone()),
        );
        let mut targets = targets.into_iter().collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            left.kind()
                .as_str()
                .cmp(right.kind().as_str())
                .then_with(|| left.as_str().cmp(right.as_str()))
        });
        Ok(LocalFavoriteUpdate { targets, transfers })
    }

    pub(crate) fn read_state(&self) -> LibraryQueryResult<RwLockReadGuard<'_, LoadedState>> {
        self.read()
    }

    pub(crate) fn write_state(&self) -> LibraryQueryResult<RwLockWriteGuard<'_, LoadedState>> {
        self.write()
    }

    fn read(&self) -> LibraryQueryResult<RwLockReadGuard<'_, LoadedState>> {
        self.state
            .read()
            .map_err(|_| LibraryQueryError::Unavailable)
    }

    fn write(&self) -> LibraryQueryResult<RwLockWriteGuard<'_, LoadedState>> {
        self.state
            .write()
            .map_err(|_| LibraryQueryError::Unavailable)
    }
}

fn push_source_artwork(
    output: &mut Vec<SourceArtwork>,
    seen: &mut HashSet<SourceArtwork>,
    artwork: Option<SourceArtwork>,
) {
    if let Some(artwork) = artwork
        && seen.insert(artwork.clone())
    {
        output.push(artwork);
    }
}

fn push_raw_source_artwork(output: &mut Vec<SourceArtwork>, artwork: Option<SourceArtwork>) {
    if let Some(artwork) = artwork {
        output.push(artwork);
    }
}

fn same_source_album(current: &Album, incoming: &Album) -> bool {
    let mut comparable = incoming.clone();
    comparable.color_seed = current.color_seed;
    if crate::album_release::release_identity(current)
        == crate::album_release::release_identity(incoming)
    {
        if comparable.release_types.is_empty() {
            comparable.release_types.clone_from(&current.release_types);
        }
        if comparable.is_compilation.is_none() {
            comparable.is_compilation = current.is_compilation;
        }
    }
    current == &comparable
}

fn same_source_track(current: &Track, incoming: &Track) -> bool {
    let mut current = current.clone();
    let mut incoming = incoming.clone();
    current.make_mut().album_artwork = None;
    incoming.make_mut().album_artwork = None;
    current == incoming
}

fn replace_track_favorite(
    state: &mut LoadedState,
    track_id: &TrackId,
    favorite: bool,
) -> LibraryQueryResult<AcceptedLibraryChange> {
    let current = state
        .tracks
        .get(track_id)
        .ok_or_else(|| LibraryQueryError::MissingItem {
            kind: "track",
            id: track_id.to_string(),
        })?
        .clone();
    let mut replacement = current.clone();
    replacement.favorite = favorite;
    if replacement == current {
        return Ok(AcceptedLibraryChange {
            tracks: vec![AcceptedTrackReplacement {
                id: track_id.clone(),
                track: Some(current),
                activity_only: false,
            }],
            ..AcceptedLibraryChange::default()
        });
    }

    state.tracks.insert(track_id.clone(), replacement.clone());
    let affected_smart_playlists = crate::smart_playlists::changed_by_favorite(
        &state.smart_playlists,
        &current,
        &replacement,
        &state.activity,
    );

    Ok(AcceptedLibraryChange {
        tracks: vec![AcceptedTrackReplacement {
            id: track_id.clone(),
            track: Some(replacement),
            activity_only: false,
        }],
        smart_playlists: sorted_set(affected_smart_playlists),
        ..AcceptedLibraryChange::default()
    })
}

fn apply_item_replacement(
    state: &mut LoadedState,
    replacement: ItemReplacement,
) -> LibraryQueryResult<AcceptedLibraryChange> {
    let ItemReplacement {
        albums,
        tracks,
        artists,
        genres,
        removed_albums,
        removed_tracks,
        removed_artists,
        removed_genres,
    } = replacement;
    let removed_track_ids = removed_tracks.into_iter().collect::<HashSet<_>>();
    let changed_track_ids = tracks
        .iter()
        .map(|track| track.id.clone())
        .chain(removed_track_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let mut published_track_ids = changed_track_ids.clone();
    let old_tracks = changed_track_ids
        .iter()
        .filter_map(|id| {
            state
                .tracks
                .get(id)
                .cloned()
                .map(|track| (id.clone(), track))
        })
        .collect::<HashMap<_, _>>();
    let incoming_tracks = tracks
        .iter()
        .map(|track| (track.id.clone(), track))
        .collect::<HashMap<_, _>>();
    let media_path_presence = old_tracks
        .values()
        .chain(tracks.iter())
        .filter_map(|track| track.source_path.as_deref())
        .map(|path| (path.to_string(), path_has_source_track(state, path)))
        .collect::<HashMap<_, _>>();
    let directly_affected_albums = albums
        .iter()
        .map(|album| album.id.clone())
        .chain(removed_albums.iter().cloned())
        .collect::<HashSet<_>>();
    let directly_affected_artists = artists
        .iter()
        .map(|artist| artist.id.clone())
        .chain(removed_artists.iter().cloned())
        .collect::<HashSet<_>>();
    let directly_affected_genres = genres
        .iter()
        .map(|genre| genre.id.clone())
        .chain(removed_genres.iter().cloned())
        .collect::<HashSet<_>>();
    let mut affected_albums = directly_affected_albums.clone();
    let mut affected_artists = directly_affected_artists.clone();
    let mut affected_genres = directly_affected_genres.clone();
    let mut affected_moods = HashSet::new();
    let mut affected_local_folders = HashSet::new();
    let mut published_albums = directly_affected_albums;
    let mut published_artists = directly_affected_artists;
    let mut published_genres = directly_affected_genres;
    let mut published_moods = HashSet::new();
    let mut published_local_folders = HashSet::new();
    for track_id in &changed_track_ids {
        let previous = old_tracks.get(track_id);
        let incoming = incoming_tracks.get(track_id).copied();
        if track_relationship_projection_changed(previous, incoming) {
            for track in previous.into_iter().chain(incoming) {
                collect_track_relations(
                    track,
                    &mut published_albums,
                    &mut published_artists,
                    &mut published_genres,
                    &mut published_moods,
                );
                published_local_folders.extend(local_track_folder_id(track));
            }
        }
    }
    for track in old_tracks.values().chain(tracks.iter()) {
        collect_track_relations(
            track,
            &mut affected_albums,
            &mut affected_artists,
            &mut affected_genres,
            &mut affected_moods,
        );
        affected_local_folders.extend(local_track_folder_id(track));
    }
    for album in &albums {
        collect_album_relations(album, &mut affected_artists, &mut affected_genres);
        collect_album_relations(album, &mut published_artists, &mut published_genres);
        if let Some(previous) = state.albums.get(&album.id) {
            collect_album_relations(previous, &mut affected_artists, &mut affected_genres);
            collect_album_relations(previous, &mut published_artists, &mut published_genres);
        }
    }
    for album_id in &removed_albums {
        if let Some(previous) = state.albums.get(album_id) {
            collect_album_relations(previous, &mut affected_artists, &mut affected_genres);
            collect_album_relations(previous, &mut published_artists, &mut published_genres);
        }
    }
    let old_albums = affected_albums
        .iter()
        .filter_map(|id| {
            state
                .albums
                .get(id)
                .map(|album| (id.clone(), album.album.as_ref().clone()))
        })
        .collect::<HashMap<_, _>>();
    for album in old_albums.values() {
        collect_album_relations(album, &mut affected_artists, &mut affected_genres);
        collect_album_relations(album, &mut published_artists, &mut published_genres);
    }
    remove_old_artwork_items(state, &affected_albums, &affected_artists);
    for album in old_albums.values() {
        remove_album_from_indexes(state, album);
    }

    for track in old_tracks.values() {
        remove_track_from_indexes(state, track);
    }
    for track_id in &removed_track_ids {
        state.tracks.remove(track_id);
        state.activity.remove(track_id);
    }
    for album_id in removed_albums {
        if let Some(album) = state.albums.get_mut(&album_id) {
            album.source_provided = false;
        }
    }
    for artist_id in removed_artists {
        if let Some(artist) = state.artists.get_mut(&artist_id) {
            artist.source_provided = false;
        }
    }
    for genre_id in removed_genres {
        if let Some(genre) = state.genres.get_mut(&genre_id) {
            genre.source_provided = false;
        }
    }
    for artist in artists {
        if let Some(row) = state.artists.get_mut(&artist.id) {
            row.replace(artist);
            row.source_provided = true;
        } else {
            state
                .artists
                .insert(artist.id.clone(), LoadedArtist::new(artist, true));
        }
    }
    for genre in genres {
        if let Some(row) = state.genres.get_mut(&genre.id) {
            row.genre = Arc::new(genre);
            row.source_provided = true;
        } else {
            state.genres.insert(
                genre.id.clone(),
                LoadedGenre {
                    genre: Arc::new(genre),
                    source_provided: true,
                    tracks: Vec::new(),
                    albums: Vec::new(),
                },
            );
        }
    }
    for mut album in albums {
        album.color_seed = color_seed(album.id.as_str());
        if let Some(row) = state.albums.get_mut(&album.id) {
            row.replace(album);
            row.source_provided = true;
        } else {
            state
                .albums
                .insert(album.id.clone(), LoadedAlbum::new(album, true));
        }
    }
    let mut album_artwork = HashMap::<AlbumId, Arc<AlbumArtworkFacts>>::new();
    for mut track in tracks {
        ensure_track_rows(state, &track);
        let artwork = track.album_id.as_ref().and_then(|album_id| {
            if let Some(artwork) = album_artwork.get(album_id) {
                return Some(Arc::clone(artwork));
            }
            let artwork = Arc::new(AlbumArtworkFacts::from(
                state.albums.get(album_id)?.album.as_ref(),
            ));
            album_artwork.insert(album_id.clone(), Arc::clone(&artwork));
            Some(artwork)
        });
        track.album_artwork = artwork;
        state.tracks.insert(track.id.clone(), track.clone());
        add_track_to_indexes(state, &track);
    }

    for album_id in affected_albums.clone() {
        if let Some(album) = state
            .albums
            .get(&album_id)
            .map(|album| album.album.as_ref().clone())
        {
            remove_album_from_indexes(state, &album);
        }
        refresh_sparse_album(state, &album_id);
        if let Some(album) = state
            .albums
            .get(&album_id)
            .map(|album| album.album.as_ref().clone())
        {
            collect_album_relations(&album, &mut affected_artists, &mut affected_genres);
            collect_album_relations(&album, &mut published_artists, &mut published_genres);
            add_album_to_indexes(state, &album);
        }
    }

    for artist_id in affected_artists.clone() {
        refresh_sparse_artist(state, &artist_id);
    }
    for genre_id in affected_genres.clone() {
        refresh_sparse_genre(state, &genre_id);
    }
    for mood_id in affected_moods.clone() {
        refresh_mood(state, &mood_id);
    }
    add_current_artwork_items(state, &affected_albums, &affected_artists);
    relink_tracks_to_albums(state, &affected_albums, &mut published_track_ids);
    crate::browse::organize_artwork_bindings(state, &affected_albums, &affected_artists);
    for (path, was_present) in media_path_presence {
        match (was_present, path_has_source_track(state, &path)) {
            (true, false) => remove_media_directory_path(state, &path),
            (false, true) => add_media_directory_path(state, &path),
            _ => {}
        }
    }

    let published_playlists = affected_playlists(state, &changed_track_ids);
    let affected_smart_playlists = crate::smart_playlists::changed_by_tracks(
        &state.smart_playlists,
        &old_tracks,
        &changed_track_ids,
        &state.tracks,
        &state.activity,
    );
    refresh_loudness_validity(state, &changed_track_ids, &affected_albums);
    Ok(AcceptedLibraryChange {
        tracks: accepted_track_replacements(state, published_track_ids),
        albums: sorted_set(published_albums),
        artists: sorted_set(published_artists),
        genres: sorted_set(published_genres),
        moods: sorted_set(published_moods),
        playlists: sorted_set(published_playlists),
        smart_playlists: sorted_set(affected_smart_playlists),
        local_folders_changed: !published_local_folders.is_empty(),
        ..AcceptedLibraryChange::default()
    })
}

fn track_relationship_projection_changed(
    previous: Option<&Track>,
    incoming: Option<&Track>,
) -> bool {
    let (Some(previous), Some(incoming)) = (previous, incoming) else {
        return true;
    };
    previous.album_id != incoming.album_id
        || previous.title != incoming.title
        || previous.album != incoming.album
        || previous.disc_number != incoming.disc_number
        || previous.track_number != incoming.track_number
        || previous.duration_seconds != incoming.duration_seconds
        || previous.image_ref != incoming.image_ref
        || previous.local_artwork != incoming.local_artwork
        || previous.source_path != incoming.source_path
        || previous.relations != incoming.relations
}

fn relink_tracks_to_albums(
    state: &mut LoadedState,
    album_ids: &HashSet<AlbumId>,
    published_track_ids: &mut HashSet<TrackId>,
) {
    for album_id in album_ids {
        let album_artwork = state
            .albums
            .get(album_id)
            .map(|album| Arc::new(AlbumArtworkFacts::from(album.album.as_ref())));
        let track_slots = state
            .albums
            .get(album_id)
            .map_or(&[][..], |relationship| relationship.tracks.as_slice())
            .to_vec();
        for track_slot in track_slots {
            let Some(current) = state.tracks.get_slot(track_slot).cloned() else {
                continue;
            };
            if current.album_artwork.as_deref() == album_artwork.as_deref() {
                continue;
            }
            let mut replacement = current.clone();
            replacement.album_artwork.clone_from(&album_artwork);
            state.tracks.insert(current.id.clone(), replacement);
            published_track_ids.insert(current.id.clone());
        }
    }
}

fn accepted_track_replacements(
    state: &LoadedState,
    track_ids: HashSet<TrackId>,
) -> Vec<AcceptedTrackReplacement> {
    let mut tracks = track_ids
        .into_iter()
        .map(|id| AcceptedTrackReplacement {
            track: state.tracks.get(&id).cloned(),
            id,
            activity_only: false,
        })
        .collect::<Vec<_>>();
    tracks.sort_by(|left, right| left.id.cmp(&right.id));
    tracks
}

fn collect_track_relations(
    track: &Track,
    albums: &mut HashSet<AlbumId>,
    artists: &mut HashSet<ArtistId>,
    genres: &mut HashSet<GenreId>,
    moods: &mut HashSet<MoodId>,
) {
    albums.extend(track.album_id.iter().cloned());
    artists.extend(
        track
            .relations
            .artists
            .iter()
            .chain(track.relations.album_artists.iter())
            .map(|credit| credit.id.clone()),
    );
    genres.extend(
        track
            .relations
            .genres
            .iter()
            .map(|credit| credit.id.clone()),
    );
    moods.extend(track.relations.moods.iter().map(|credit| credit.id.clone()));
}

fn collect_album_relations(
    album: &Album,
    artists: &mut HashSet<ArtistId>,
    genres: &mut HashSet<GenreId>,
) {
    artists.extend(
        album
            .relations
            .album_artists
            .iter()
            .chain(album.relations.artists.iter())
            .map(|credit| credit.id.clone()),
    );
    genres.extend(
        album
            .relations
            .genres
            .iter()
            .map(|credit| credit.id.clone()),
    );
}

fn album_artist_ids(state: &LoadedState, album_id: &AlbumId) -> HashSet<ArtistId> {
    let mut artists = HashSet::new();
    let Some(album) = state.albums.get(album_id) else {
        return artists;
    };
    artists.extend(
        distinct_album_artist_credits(album)
            .into_iter()
            .map(|credit| credit.id.clone()),
    );
    for track in album
        .tracks
        .iter()
        .filter_map(|slot| state.tracks.get_slot(*slot))
    {
        artists.extend(
            distinct_artist_credits(track)
                .into_iter()
                .map(|credit| credit.id.clone()),
        );
    }
    artists
}

fn collect_track_favorite_targets(track: &Track, targets: &mut HashSet<FavoriteItemId>) {
    targets.extend(track.album_id.iter().cloned().map(FavoriteItemId::Album));
    targets.extend(
        track
            .relations
            .artists
            .iter()
            .chain(track.relations.album_artists.iter())
            .map(|credit| FavoriteItemId::Artist(credit.id.clone())),
    );
}

fn collect_album_favorite_targets(album: &Album, targets: &mut HashSet<FavoriteItemId>) {
    targets.extend(
        album
            .relations
            .album_artists
            .iter()
            .chain(album.relations.artists.iter())
            .map(|credit| FavoriteItemId::Artist(credit.id.clone())),
    );
}

fn local_favorite_transfers(
    state: &LoadedState,
    replacement: &ItemReplacement,
) -> Vec<LocalFavoriteTransfer> {
    let incoming_tracks = replacement
        .tracks
        .iter()
        .map(|track| (track.id.clone(), track))
        .collect::<HashMap<_, _>>();
    let incoming_albums = replacement
        .albums
        .iter()
        .map(|album| (album.id.clone(), album))
        .collect::<HashMap<_, _>>();
    let removed_tracks = replacement
        .removed_tracks
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let removed_albums = replacement
        .removed_albums
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let removed_artists = replacement
        .removed_artists
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut candidates = HashMap::<FavoriteItemId, HashSet<FavoriteItemId>>::new();

    for album_id in &replacement.removed_albums {
        let Some(album) = state.albums.get(album_id) else {
            continue;
        };
        let removed = FavoriteItemId::Album(album_id.clone());
        let mut replacements = album
            .tracks
            .iter()
            .filter_map(|slot| state.tracks.get_slot(*slot))
            .filter_map(|track| {
                final_local_track(state, &incoming_tracks, &removed_tracks, &track.id)
            })
            .filter_map(|track| track.album_id.clone())
            .map(FavoriteItemId::Album)
            .collect::<HashSet<_>>();
        if replacements.remove(&removed) || replacements.len() != 1 {
            continue;
        }
        candidates.insert(removed, replacements);
    }

    for artist_id in &replacement.removed_artists {
        let Some(artist) = state.artists.get(artist_id) else {
            continue;
        };
        let removed = FavoriteItemId::Artist(artist_id.clone());
        let mut track_ids = artist
            .tracks
            .iter()
            .filter_map(|slot| state.tracks.get_slot(*slot))
            .map(|track| track.id.clone())
            .collect::<HashSet<_>>();
        for album in artist
            .albums
            .iter()
            .filter_map(|slot| state.albums.get_slot(*slot))
        {
            track_ids.extend(
                album
                    .tracks
                    .iter()
                    .filter_map(|slot| state.tracks.get_slot(*slot))
                    .map(|track| track.id.clone()),
            );
        }
        let mut stable_tracks = HashSet::new();
        let mut coverage = HashMap::<ArtistId, HashSet<TrackId>>::new();
        let mut retained = false;
        for track_id in track_ids {
            let Some(previous) = state.tracks.get(&track_id) else {
                continue;
            };
            let Some(final_track) =
                final_local_track(state, &incoming_tracks, &removed_tracks, &track_id)
            else {
                continue;
            };
            let previous_artists = loaded_track_artist_ids(state, previous);
            let final_artists =
                final_track_artist_ids(state, &incoming_albums, &removed_albums, &final_track);
            if final_artists.contains(artist_id) {
                retained = true;
                break;
            }
            stable_tracks.insert(track_id.clone());
            for candidate in final_artists.difference(&previous_artists) {
                if !removed_artists.contains(candidate) {
                    coverage
                        .entry(candidate.clone())
                        .or_default()
                        .insert(track_id.clone());
                }
            }
        }
        if retained || stable_tracks.is_empty() {
            continue;
        }
        let replacements = if coverage.len() == 1 {
            coverage
                .into_iter()
                .filter(|(_, tracks)| tracks == &stable_tracks)
                .map(|(artist_id, _)| FavoriteItemId::Artist(artist_id))
                .collect()
        } else {
            HashSet::new()
        };
        if !replacements.is_empty() {
            candidates.insert(removed, replacements);
        }
    }

    let mut transfers = candidates
        .into_iter()
        .filter_map(|(removed, replacements)| {
            if replacements.len() != 1 {
                return None;
            }
            let replacement = replacements.into_iter().next()?;
            Some(LocalFavoriteTransfer {
                removed,
                replacement,
            })
        })
        .collect::<Vec<_>>();
    transfers.sort_by(|left, right| {
        left.removed
            .kind()
            .as_str()
            .cmp(right.removed.kind().as_str())
            .then_with(|| left.removed.as_str().cmp(right.removed.as_str()))
    });
    transfers
}

fn final_local_track(
    state: &LoadedState,
    incoming: &HashMap<TrackId, &Track>,
    removed: &HashSet<TrackId>,
    track_id: &TrackId,
) -> Option<Track> {
    incoming
        .get(track_id)
        .map(|track| (*track).clone())
        .or_else(|| {
            (!removed.contains(track_id))
                .then(|| state.tracks.get(track_id).cloned())
                .flatten()
        })
}

fn final_track_artist_ids(
    state: &LoadedState,
    incoming_albums: &HashMap<AlbumId, &Album>,
    removed_albums: &HashSet<AlbumId>,
    track: &Track,
) -> HashSet<ArtistId> {
    let album = track.album_id.as_ref().and_then(|album_id| {
        incoming_albums.get(album_id).copied().or_else(|| {
            (!removed_albums.contains(album_id))
                .then(|| state.albums.get(album_id).map(|album| album.album.as_ref()))
                .flatten()
        })
    });
    track_artist_ids(track, album)
}

fn loaded_track_artist_ids(state: &LoadedState, track: &Track) -> HashSet<ArtistId> {
    let album = track
        .album_id
        .as_ref()
        .and_then(|album_id| state.albums.get(album_id))
        .map(|album| album.album.as_ref());
    track_artist_ids(track, album)
}

fn track_artist_ids(track: &Track, album: Option<&Album>) -> HashSet<ArtistId> {
    distinct_artist_credits(track)
        .into_iter()
        .chain(album.into_iter().flat_map(distinct_album_artist_credits))
        .map(|credit| credit.id.clone())
        .collect()
}

fn apply_track_activity_to_replacement(tracks: &mut [Track], activity: Vec<TrackActivity>) {
    let activity = activity
        .into_iter()
        .map(|value| (value.track_id.clone(), value))
        .collect::<HashMap<_, _>>();
    for track in tracks {
        if let Some(value) = activity.get(&track.id) {
            apply_track_activity_value(track, value);
        }
    }
}

fn apply_favorites_to_replacement(replacement: &mut ItemReplacement, favorites: &[FavoriteItemId]) {
    let favorites = favorites.iter().collect::<HashSet<_>>();
    for album in &mut replacement.albums {
        album.favorite = favorites.contains(&FavoriteItemId::Album(album.id.clone()));
    }
    for track in &mut replacement.tracks {
        track.favorite = favorites.contains(&FavoriteItemId::Track(track.id.clone()));
    }
    for artist in &mut replacement.artists {
        artist.favorite = favorites.contains(&FavoriteItemId::Artist(artist.id.clone()));
    }
}

fn apply_sparse_favorites(state: &mut LoadedState, favorites: Vec<FavoriteItemId>) {
    for favorite in favorites {
        match favorite {
            FavoriteItemId::Album(id) => {
                if let Some(current) = state.albums.get_mut(&id)
                    && !current.source_provided
                {
                    let mut album = current.album.as_ref().clone();
                    album.favorite = true;
                    current.album = Arc::new(album);
                }
            }
            FavoriteItemId::Artist(id) => {
                if let Some(current) = state.artists.get_mut(&id)
                    && !current.source_provided
                {
                    let mut artist = current.artist.as_ref().clone();
                    artist.favorite = true;
                    current.artist = Arc::new(artist);
                }
            }
            FavoriteItemId::Track(_) => {}
        }
    }
}

fn collect_local_folder_effects(file: &LocalFile, affected: &mut HashSet<FolderId>) {
    if file.kind != LocalFileKind::Directory {
        return;
    }
    affected.insert(local_folder_id(&file.path));
    affected.extend(local_folder_parent_id(file));
}

fn remove_local_folder(state: &mut LoadedState, file: &LocalFile) {
    let id = local_folder_id(&file.path);
    let parent_id = state
        .local_folders
        .remove(&id)
        .and_then(|folder| folder.parent_id);
    let removed_tracks = state.local_folder_tracks.remove(&Some(id.clone()));
    if parent_id.is_none()
        && let Some(removed_tracks) = removed_tracks
        && let Some(root_tracks) = state.local_folder_tracks.get_mut(&None)
    {
        let removed_tracks = removed_tracks.into_iter().collect::<HashSet<_>>();
        root_tracks.retain(|slot| !removed_tracks.contains(slot));
        if root_tracks.is_empty() {
            state.local_folder_tracks.remove(&None);
        }
    }
    remove_id_relation(&mut state.local_folder_children, &parent_id, &id);
}

fn add_local_folder(state: &mut LoadedState, file: &LocalFile) {
    let (id, folder) = loaded_local_folder(file);
    let parent_id = folder.parent_id.clone();
    state.local_folders.insert(id.clone(), folder);
    let children = state
        .local_folder_children
        .entry(parent_id.clone())
        .or_default();
    if !children.contains(&id) {
        children.push(id.clone());
    }
    children.sort_by(|left, right| {
        compare_local_folders(
            &state.local_folders[left].folder,
            &state.local_folders[right].folder,
        )
    });

    let track_ids = local_folder_track_ids(state, &file.path);
    if track_ids.is_empty() {
        state.local_folder_tracks.remove(&Some(id));
    } else {
        state
            .local_folder_tracks
            .insert(Some(id.clone()), track_ids.clone());
        if parent_id.is_none() {
            for slot in track_ids {
                let track = state
                    .tracks
                    .get_slot(slot)
                    .expect("Local folder Track slot must resolve")
                    .clone();
                insert_sorted_track(
                    &mut state.local_folder_tracks,
                    None,
                    &track,
                    &state.tracks,
                    compare_album_tracks,
                );
            }
        }
    }
}

fn remove_id_relation<Id>(index: &mut HashMap<Option<Id>, Vec<Id>>, key: &Option<Id>, value: &Id)
where
    Id: Clone + Eq + std::hash::Hash,
{
    let remove = if let Some(values) = index.get_mut(key) {
        values.retain(|candidate| candidate != value);
        values.is_empty()
    } else {
        false
    };
    if remove {
        index.remove(key);
    }
}

fn local_folder_track_ids(state: &LoadedState, directory: &str) -> Vec<TrackSlot> {
    let directory = std::path::Path::new(directory);
    let mut ids = state
        .directory_children
        .get(directory.to_string_lossy().as_ref())
        .into_iter()
        .flatten()
        .filter_map(|path| state.path_tracks.get(path))
        .flatten()
        .filter(|slot| {
            state
                .tracks
                .get_slot(**slot)
                .and_then(|track| track.source_path.as_deref())
                .and_then(|path| std::path::Path::new(path).parent())
                == Some(directory)
        })
        .copied()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| {
        compare_album_tracks(
            state
                .tracks
                .get_slot(*left)
                .expect("Local folder Track slot must resolve"),
            state
                .tracks
                .get_slot(*right)
                .expect("Local folder Track slot must resolve"),
        )
    });
    ids
}

fn remove_local_file_relations(state: &mut LoadedState, file: &LocalFile) {
    if file.kind == LocalFileKind::Directory {
        remove_local_folder(state, file);
    }
    if file.kind == LocalFileKind::Cue {
        for dependency in &file.dependencies {
            remove_string_relation(&mut state.cue_dependents, dependency, &file.path);
        }
    }
    if let Some(parent) = std::path::Path::new(&file.path).parent() {
        if file.kind == LocalFileKind::Image {
            remove_string_relation(
                &mut state.directory_images,
                parent.to_string_lossy().as_ref(),
                &file.path,
            );
        }
        remove_string_relation(
            &mut state.directory_children,
            parent.to_string_lossy().as_ref(),
            &file.path,
        );
    }
}

fn add_local_file_relations(state: &mut LoadedState, file: &LocalFile) {
    if file.kind == LocalFileKind::Cue {
        for dependency in &file.dependencies {
            state
                .cue_dependents
                .entry(dependency.clone())
                .or_default()
                .insert(file.path.clone());
        }
    }
    if let Some(parent) = std::path::Path::new(&file.path).parent() {
        if file.kind == LocalFileKind::Image {
            state
                .directory_images
                .entry(parent.to_string_lossy().into_owned())
                .or_default()
                .insert(file.path.clone());
        }
        state
            .directory_children
            .entry(parent.to_string_lossy().into_owned())
            .or_default()
            .insert(file.path.clone());
    }
    if file.kind == LocalFileKind::Directory {
        add_local_folder(state, file);
    }
}

fn remove_string_relation(index: &mut HashMap<String, HashSet<String>>, key: &str, value: &str) {
    let remove = if let Some(values) = index.get_mut(key) {
        values.remove(value);
        values.is_empty()
    } else {
        false
    };
    if remove {
        index.remove(key);
    }
}

fn remove_track_from_indexes(state: &mut LoadedState, track: &Track) {
    let track_slot = state
        .tracks
        .slot(&track.id)
        .expect("removed Track slot must resolve");
    if let Some(album_id) = &track.album_id {
        if let Some(album) = state.albums.get_mut(album_id) {
            remove_slot(&mut album.tracks, track_slot);
        }
    }
    for credit in distinct_artist_credits(track) {
        if let Some(artist) = state.artists.get_mut(&credit.id) {
            remove_slot(&mut artist.tracks, track_slot);
        }
    }
    for credit in distinct_by_id(&track.relations.genres, |credit| &credit.id) {
        if let Some(genre) = state.genres.get_mut(&credit.id) {
            remove_slot(&mut genre.tracks, track_slot);
        }
    }
    for credit in distinct_by_id(&track.relations.moods, |credit| &credit.id) {
        if let Some(mood) = state.moods.get_mut(&credit.id) {
            remove_slot(&mut mood.tracks, track_slot);
        }
    }
    for folder_id in track.relations.music_folders.iter().collect::<HashSet<_>>() {
        remove_sorted_track(
            &mut state.music_folder_tracks,
            folder_id,
            track,
            &state.tracks,
            compare_tracks_by_title,
        );
    }
    if let Some(folder_id) = local_track_folder_id(track) {
        remove_sorted_track(
            &mut state.local_folder_tracks,
            &Some(folder_id.clone()),
            track,
            &state.tracks,
            compare_album_tracks,
        );
        if state
            .local_folders
            .get(&folder_id)
            .is_some_and(|folder| folder.parent_id.is_none())
        {
            remove_sorted_track(
                &mut state.local_folder_tracks,
                &None,
                track,
                &state.tracks,
                compare_album_tracks,
            );
        }
    }
    for path in track_paths(track) {
        remove_path_track(&mut state.path_tracks, path, track_slot);
    }
    if let Some(artwork) = &track.local_artwork {
        remove_artwork_item(
            &mut state.artwork_items,
            artwork.path(),
            &LocalArtworkItemId::Track(track_slot),
        );
    }
}

fn add_track_to_indexes(state: &mut LoadedState, track: &Track) {
    ensure_track_rows(state, track);
    let track_slot = state
        .tracks
        .slot(&track.id)
        .expect("inserted Track slot must resolve");
    if let Some(album_id) = &track.album_id {
        push_slot(
            &mut state
                .albums
                .get_mut(album_id)
                .expect("relationship Album must exist before inserting a Track")
                .tracks,
            track_slot,
        );
    }
    for credit in distinct_artist_credits(track) {
        push_slot(
            &mut state
                .artists
                .get_mut(&credit.id)
                .expect("relationship Artist must exist before inserting a Track")
                .tracks,
            track_slot,
        );
    }
    for credit in distinct_by_id(&track.relations.genres, |credit| &credit.id) {
        push_slot(
            &mut state
                .genres
                .get_mut(&credit.id)
                .expect("relationship Genre must exist before inserting a Track")
                .tracks,
            track_slot,
        );
    }
    for credit in distinct_by_id(&track.relations.moods, |credit| &credit.id) {
        push_slot(
            &mut state
                .moods
                .get_mut(&credit.id)
                .expect("relationship Mood must exist before inserting a Track")
                .tracks,
            track_slot,
        );
    }
    for folder_id in track.relations.music_folders.iter().collect::<HashSet<_>>() {
        insert_sorted_track(
            &mut state.music_folder_tracks,
            folder_id.clone(),
            track,
            &state.tracks,
            compare_tracks_by_title,
        );
    }
    if let Some(folder_id) = local_track_folder_id(track)
        && state.local_folders.contains_key(&folder_id)
    {
        insert_sorted_track(
            &mut state.local_folder_tracks,
            Some(folder_id.clone()),
            track,
            &state.tracks,
            compare_album_tracks,
        );
        if state
            .local_folders
            .get(&folder_id)
            .is_some_and(|folder| folder.parent_id.is_none())
        {
            insert_sorted_track(
                &mut state.local_folder_tracks,
                None,
                track,
                &state.tracks,
                compare_album_tracks,
            );
        }
    }
    for path in track_paths(track) {
        state
            .path_tracks
            .entry(path.to_string())
            .or_default()
            .insert(track_slot);
    }
    if let Some(artwork) = &track.local_artwork {
        state
            .artwork_items
            .entry(artwork.path().to_string())
            .or_default()
            .insert(LocalArtworkItemId::Track(track_slot));
    }
}

fn ensure_track_rows(state: &mut LoadedState, track: &Track) {
    if let Some(album_id) = &track.album_id
        && !state.albums.contains_key(album_id)
    {
        let album = album_from_track(album_id, track);
        state
            .albums
            .insert(album_id.clone(), LoadedAlbum::new(album.clone(), false));
        add_album_to_indexes(state, &album);
    }
    for credit in distinct_artist_credits(track) {
        if !state.artists.contains_key(&credit.id) {
            state.artists.insert(
                credit.id.clone(),
                LoadedArtist::new(artist_from_credit(credit), false),
            );
        }
    }
    for credit in distinct_by_id(&track.relations.genres, |credit| &credit.id) {
        if !state.genres.contains_key(&credit.id) {
            state.genres.insert(
                credit.id.clone(),
                LoadedGenre {
                    genre: Arc::new(genre_from_credit(credit)),
                    source_provided: false,
                    tracks: Vec::new(),
                    albums: Vec::new(),
                },
            );
        }
    }
    for credit in distinct_by_id(&track.relations.moods, |credit| &credit.id) {
        if !state.moods.contains_key(&credit.id) {
            state.moods.insert(
                credit.id.clone(),
                LoadedMood {
                    mood: Arc::new(Mood {
                        id: credit.id.clone(),
                        name: credit.name.clone(),
                    }),
                    tracks: Vec::new(),
                },
            );
        }
    }
}

fn add_album_to_indexes(state: &mut LoadedState, album: &Album) {
    for credit in distinct_album_artist_credits(album) {
        if !state.artists.contains_key(&credit.id) {
            state.artists.insert(
                credit.id.clone(),
                LoadedArtist::new(artist_from_credit(credit), false),
            );
        }
    }
    for credit in distinct_by_id(&album.relations.genres, |credit| &credit.id) {
        if !state.genres.contains_key(&credit.id) {
            state.genres.insert(
                credit.id.clone(),
                LoadedGenre {
                    genre: Arc::new(genre_from_credit(credit)),
                    source_provided: false,
                    tracks: Vec::new(),
                    albums: Vec::new(),
                },
            );
        }
    }

    let album_slot = state
        .albums
        .slot(&album.id)
        .expect("inserted Album slot must resolve");
    for credit in distinct_album_artist_credits(album) {
        push_slot(
            &mut state
                .artists
                .get_mut(&credit.id)
                .expect("relationship Artist must exist before inserting an Album")
                .albums,
            album_slot,
        );
    }
    for credit in distinct_by_id(&album.relations.genres, |credit| &credit.id) {
        push_slot(
            &mut state
                .genres
                .get_mut(&credit.id)
                .expect("relationship Genre must exist before inserting an Album")
                .albums,
            album_slot,
        );
    }
}

fn remove_album_from_indexes(state: &mut LoadedState, album: &Album) {
    let Some(album_slot) = state.albums.slot(&album.id) else {
        return;
    };
    for credit in distinct_album_artist_credits(album) {
        if let Some(artist) = state.artists.get_mut(&credit.id) {
            remove_slot(&mut artist.albums, album_slot);
        }
    }
    for credit in distinct_by_id(&album.relations.genres, |credit| &credit.id) {
        if let Some(genre) = state.genres.get_mut(&credit.id) {
            remove_slot(&mut genre.albums, album_slot);
        }
    }
}

fn remove_slot<Id>(slots: &mut Vec<ItemSlot<Id>>, removed: ItemSlot<Id>) {
    if let Some(position) = slots.iter().position(|slot| slot.index == removed.index) {
        slots.swap_remove(position);
    }
}

fn push_slot<Id>(slots: &mut Vec<ItemSlot<Id>>, slot: ItemSlot<Id>) {
    if !slots.iter().any(|candidate| candidate.index == slot.index) {
        slots.push(slot);
    }
}

fn remove_sorted_track<Id>(
    index: &mut HashMap<Id, Vec<TrackSlot>>,
    id: &Id,
    track: &Track,
    tracks: &LoadedItems<TrackId, Track>,
    compare: fn(&Track, &Track) -> std::cmp::Ordering,
) where
    Id: Clone + Eq + std::hash::Hash,
{
    let Some(track_slot) = tracks.slot(&track.id) else {
        return;
    };
    let remove = if let Some(ids) = index.get_mut(id) {
        if let Ok(position) = ids.binary_search_by(|candidate| {
            if candidate == &track_slot {
                std::cmp::Ordering::Equal
            } else {
                compare(
                    tracks
                        .get_slot(*candidate)
                        .expect("relationship Track slot must resolve"),
                    track,
                )
            }
        }) {
            ids.remove(position);
        }
        ids.is_empty()
    } else {
        false
    };
    if remove {
        index.remove(id);
    }
}

fn insert_sorted_track<Id>(
    index: &mut HashMap<Id, Vec<TrackSlot>>,
    id: Id,
    track: &Track,
    tracks: &LoadedItems<TrackId, Track>,
    compare: fn(&Track, &Track) -> std::cmp::Ordering,
) where
    Id: Eq + std::hash::Hash,
{
    let track_slot = tracks
        .slot(&track.id)
        .expect("inserted Track slot must resolve");
    let ids = index.entry(id).or_default();
    match ids.binary_search_by(|candidate| {
        if candidate == &track_slot {
            std::cmp::Ordering::Equal
        } else {
            compare(
                tracks
                    .get_slot(*candidate)
                    .expect("relationship Track slot must resolve"),
                track,
            )
        }
    }) {
        Ok(_) => {}
        Err(position) => ids.insert(position, track_slot),
    }
}

fn remove_path_track(
    index: &mut HashMap<String, HashSet<TrackSlot>>,
    path: &str,
    track_slot: TrackSlot,
) {
    let remove = if let Some(track_ids) = index.get_mut(path) {
        track_ids.remove(&track_slot);
        track_ids.is_empty()
    } else {
        false
    };
    if remove {
        index.remove(path);
    }
}

fn remove_artwork_item(
    index: &mut HashMap<String, HashSet<LocalArtworkItemId>>,
    path: &str,
    item: &LocalArtworkItemId,
) {
    let remove = if let Some(items) = index.get_mut(path) {
        items.remove(item);
        items.is_empty()
    } else {
        false
    };
    if remove {
        index.remove(path);
    }
}

fn refresh_sparse_album(state: &mut LoadedState, id: &AlbumId) {
    let Some(row) = state.albums.get(id) else {
        return;
    };
    if row.source_provided {
        return;
    }
    let album = row
        .tracks
        .iter()
        .filter_map(|slot| state.tracks.get_slot(*slot))
        .min_by(|left, right| compare_album_tracks(left, right))
        .map(|track| album_from_track(id, track));
    if let Some(album) = album {
        state
            .albums
            .get_mut(id)
            .expect("derived Album row still exists")
            .album = Arc::new(album);
    } else {
        state.albums.remove(id);
    }
}

fn album_from_track(id: &AlbumId, track: &Track) -> Album {
    Album {
        id: id.clone(),
        title: nonempty_or(&track.album, "Unknown Album"),
        artist: track.artist.clone(),
        year: track.year,
        release_date: track.release_date.clone(),
        date_added: track.date_added.clone(),
        last_played: track.last_played.clone(),
        play_count: track.play_count,
        user_rating: None,
        favorite: false,
        color_seed: color_seed(id.as_str()),
        image_ref: track.image_ref.clone(),
        local_artwork: track.local_artwork.clone(),
        release_types: Vec::new(),
        is_compilation: None,
        musicbrainz_album_id: None,
        musicbrainz_release_group_id: None,
        relations: AlbumRelations {
            album_artists: track.relations.album_artists.clone(),
            artists: track.relations.artists.clone(),
            genres: track.relations.genres.clone(),
        },
    }
}

fn remove_old_artwork_items(
    state: &mut LoadedState,
    album_ids: &HashSet<AlbumId>,
    artist_ids: &HashSet<ArtistId>,
) {
    for album_id in album_ids {
        if let Some(album_slot) = state.albums.slot(album_id)
            && let Some(album) = state.albums.get_slot(album_slot)
            && let Some(artwork) = &album.local_artwork
        {
            remove_artwork_item(
                &mut state.artwork_items,
                artwork.path(),
                &LocalArtworkItemId::Album(album_slot),
            );
        }
    }
    for artist_id in artist_ids {
        if let Some(artist_slot) = state.artists.slot(artist_id)
            && let Some(artist) = state.artists.get_slot(artist_slot)
            && let Some(artwork) = &artist.local_artwork
        {
            remove_artwork_item(
                &mut state.artwork_items,
                artwork.path(),
                &LocalArtworkItemId::Artist(artist_slot),
            );
        }
    }
}

fn add_current_artwork_items(
    state: &mut LoadedState,
    album_ids: &HashSet<AlbumId>,
    artist_ids: &HashSet<ArtistId>,
) {
    for album_id in album_ids {
        if let Some(album_slot) = state.albums.slot(album_id)
            && let Some(album) = state.albums.get_slot(album_slot)
            && let Some(artwork) = &album.local_artwork
        {
            state
                .artwork_items
                .entry(artwork.path().to_string())
                .or_default()
                .insert(LocalArtworkItemId::Album(album_slot));
        }
    }
    for artist_id in artist_ids {
        if let Some(artist_slot) = state.artists.slot(artist_id)
            && let Some(artist) = state.artists.get_slot(artist_slot)
            && let Some(artwork) = &artist.local_artwork
        {
            state
                .artwork_items
                .entry(artwork.path().to_string())
                .or_default()
                .insert(LocalArtworkItemId::Artist(artist_slot));
        }
    }
}

fn refresh_sparse_artist(state: &mut LoadedState, id: &ArtistId) {
    let Some(row) = state.artists.get(id) else {
        return;
    };
    if row.source_provided {
        return;
    }
    let artist = find_artist_credit(state, id).map(|credit| artist_from_credit(&credit));
    if let Some(artist) = artist {
        state
            .artists
            .get_mut(id)
            .expect("derived Artist row still exists")
            .artist = Arc::new(artist);
    } else {
        state.artists.remove(id);
    }
}

fn find_artist_credit(state: &LoadedState, id: &ArtistId) -> Option<ArtistCredit> {
    let album_credit = state
        .artists
        .get(id)
        .map(|relationship| relationship.albums.as_slice())
        .into_iter()
        .flatten()
        .filter_map(|slot| state.albums.get_slot(*slot))
        .filter_map(|album| {
            let credit = album
                .relations
                .album_artists
                .iter()
                .chain(album.relations.artists.iter())
                .find(|credit| &credit.id == id)?;
            Some((&album.id, credit))
        })
        .min_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, credit)| credit.clone());
    album_credit.or_else(|| {
        state
            .artists
            .get(id)
            .map(|relationship| relationship.tracks.as_slice())
            .into_iter()
            .flatten()
            .filter_map(|slot| state.tracks.get_slot(*slot))
            .filter_map(|track| {
                let credit = track
                    .relations
                    .artists
                    .iter()
                    .chain(track.relations.album_artists.iter())
                    .find(|credit| &credit.id == id)?;
                Some((&track.id, credit))
            })
            .min_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(_, credit)| credit.clone())
    })
}

fn refresh_sparse_genre(state: &mut LoadedState, id: &GenreId) {
    let Some(row) = state.genres.get(id) else {
        return;
    };
    if row.source_provided {
        return;
    }
    let genre = find_genre_credit(state, id).map(|credit| genre_from_credit(&credit));
    if let Some(genre) = genre {
        state
            .genres
            .get_mut(id)
            .expect("derived Genre row still exists")
            .genre = Arc::new(genre);
    } else {
        state.genres.remove(id);
    }
}

fn find_genre_credit(state: &LoadedState, id: &GenreId) -> Option<GenreCredit> {
    let album_credit = state
        .genres
        .get(id)
        .map(|relationship| relationship.albums.as_slice())
        .into_iter()
        .flatten()
        .filter_map(|slot| state.albums.get_slot(*slot))
        .filter_map(|album| {
            let credit = album
                .relations
                .genres
                .iter()
                .find(|credit| &credit.id == id)?;
            Some((&album.id, credit))
        })
        .min_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, credit)| credit.clone());
    album_credit.or_else(|| {
        state
            .genres
            .get(id)
            .map(|relationship| relationship.tracks.as_slice())
            .into_iter()
            .flatten()
            .filter_map(|slot| state.tracks.get_slot(*slot))
            .filter_map(|track| {
                let credit = track
                    .relations
                    .genres
                    .iter()
                    .find(|credit| &credit.id == id)?;
                Some((&track.id, credit))
            })
            .min_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(_, credit)| credit.clone())
    })
}

fn refresh_mood(state: &mut LoadedState, id: &MoodId) {
    let Some(credit) = state
        .moods
        .get(id)
        .map(|relationship| relationship.tracks.as_slice())
        .into_iter()
        .flatten()
        .filter_map(|slot| state.tracks.get_slot(*slot))
        .filter_map(|track| {
            let credit = track
                .relations
                .moods
                .iter()
                .find(|credit| &credit.id == id)?;
            Some((&track.id, credit))
        })
        .min_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, credit)| credit)
    else {
        state.moods.remove(id);
        return;
    };
    let mood = Arc::new(Mood {
        id: id.clone(),
        name: credit.name.clone(),
    });
    if let Some(row) = state.moods.get_mut(id) {
        row.mood = mood;
    }
}

fn affected_playlists(
    state: &LoadedState,
    changed_track_ids: &HashSet<TrackId>,
) -> HashSet<PlaylistId> {
    let playlist_slots = changed_track_ids
        .iter()
        .filter_map(|id| state.track_playlists.get(id))
        .flatten()
        .copied()
        .collect::<HashSet<_>>();
    playlist_slots
        .into_iter()
        .filter_map(|slot| state.playlists.get_slot(slot))
        .map(|playlist| playlist.playlist.id.clone())
        .collect()
}

fn sorted_set<Value: Ord>(values: HashSet<Value>) -> Vec<Value> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort();
    values
}

fn values_by_id<Id, Value>(values: Vec<Value>, id: impl Fn(&Value) -> Id) -> HashMap<Id, Value>
where
    Id: Eq + std::hash::Hash,
{
    values
        .into_iter()
        .map(|value| (id(&value), value))
        .collect()
}

fn shared_values<Id, Value>(values: HashMap<Id, Value>) -> HashMap<Id, Arc<Value>>
where
    Id: Eq + std::hash::Hash,
{
    values
        .into_iter()
        .map(|(id, value)| (id, Arc::new(value)))
        .collect()
}

fn artist_from_credit(credit: &ArtistCredit) -> Artist {
    Artist {
        id: credit.id.clone(),
        name: nonempty_or(&credit.name, "Unknown Artist"),
        favorite: false,
        last_played: None,
        play_count: None,
        user_rating: None,
        musicbrainz_artist_id: credit.musicbrainz_artist_id.clone(),
        image_ref: None,
        local_artwork: None,
    }
}

fn genre_from_credit(credit: &GenreCredit) -> Genre {
    Genre {
        id: credit.id.clone(),
        name: nonempty_or(&credit.name, "Unknown Genre"),
        image_ref: None,
    }
}

fn index_local_folders<'a>(
    files: impl IntoIterator<Item = &'a LocalFile>,
) -> (
    HashMap<FolderId, LoadedLocalFolder>,
    HashMap<Option<FolderId>, Vec<FolderId>>,
) {
    let mut folders = HashMap::new();
    for file in files {
        if file.kind != LocalFileKind::Directory {
            continue;
        }
        let (id, folder) = loaded_local_folder(file);
        folders.insert(id, folder);
    }
    let mut children = HashMap::<Option<FolderId>, Vec<FolderId>>::new();
    for (id, folder) in &folders {
        children
            .entry(folder.parent_id.clone())
            .or_default()
            .push(id.clone());
    }
    for ids in children.values_mut() {
        ids.sort_by(|left, right| {
            compare_local_folders(&folders[left].folder, &folders[right].folder)
        });
    }
    (folders, children)
}

fn loaded_local_folder(file: &LocalFile) -> (FolderId, LoadedLocalFolder) {
    let path = std::path::Path::new(&file.path);
    let id = local_folder_id(&file.path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&file.path)
        .to_string();
    (
        id.clone(),
        LoadedLocalFolder {
            folder: Folder { id, name },
            parent_id: local_folder_parent_id(file),
        },
    )
}

fn local_folder_parent_id(file: &LocalFile) -> Option<FolderId> {
    if file.path == file.root {
        return None;
    }
    std::path::Path::new(&file.path)
        .parent()
        .filter(|parent| parent.starts_with(std::path::Path::new(&file.root)))
        .map(|parent| local_folder_id(parent.to_string_lossy().as_ref()))
}

fn local_track_folder_id(track: &Track) -> Option<FolderId> {
    track
        .source_path
        .as_deref()
        .and_then(|path| std::path::Path::new(path).parent())
        .map(|parent| local_folder_id(parent.to_string_lossy().as_ref()))
}

fn local_folder_id(path: &str) -> FolderId {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    FolderId::new(format!("local:folder:{hash:016x}"))
}

fn compare_local_folders(left: &Folder, right: &Folder) -> std::cmp::Ordering {
    left.name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then(left.id.cmp(&right.id))
}

fn track_paths(track: &Track) -> impl Iterator<Item = &str> {
    [
        track.source_path.as_deref(),
        track.cue.as_ref().map(|cue| cue.cue_path.as_str()),
    ]
    .into_iter()
    .flatten()
}

fn index_local_file_relations(
    files: &[LocalFile],
) -> (
    HashMap<String, HashSet<String>>,
    HashMap<String, HashSet<String>>,
    HashMap<String, HashSet<String>>,
) {
    let mut cue_dependents = HashMap::<String, HashSet<String>>::new();
    let mut directory_children = HashMap::<String, HashSet<String>>::new();
    let mut directory_images = HashMap::<String, HashSet<String>>::new();
    for file in files {
        if file.kind == LocalFileKind::Cue {
            for dependency in &file.dependencies {
                cue_dependents
                    .entry(dependency.clone())
                    .or_default()
                    .insert(file.path.clone());
            }
        }
        if let Some(parent) = std::path::Path::new(&file.path).parent() {
            if file.kind == LocalFileKind::Image {
                directory_images
                    .entry(parent.to_string_lossy().into_owned())
                    .or_default()
                    .insert(file.path.clone());
            }
            directory_children
                .entry(parent.to_string_lossy().into_owned())
                .or_default()
                .insert(file.path.clone());
        }
    }
    (cue_dependents, directory_children, directory_images)
}

fn index_local_media_directories<'a>(
    tracks: impl IntoIterator<Item = &'a Track>,
    files: &[LocalFile],
) -> HashMap<String, usize> {
    let files = files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<HashMap<_, _>>();
    let mut counts = HashMap::new();
    let mut paths = HashSet::new();
    for path in tracks
        .into_iter()
        .filter_map(|track| track.source_path.as_deref())
        .filter(|path| paths.insert(*path))
    {
        let Some(file) = files
            .get(path)
            .filter(|file| file.kind == LocalFileKind::Media)
        else {
            continue;
        };
        for directory in file.directories_to_root() {
            *counts
                .entry(directory.to_string_lossy().into_owned())
                .or_default() += 1;
        }
    }
    counts
}

fn path_has_source_track(state: &LoadedState, path: &str) -> bool {
    state
        .path_tracks
        .get(path)
        .into_iter()
        .flatten()
        .filter_map(|slot| state.tracks.get_slot(*slot))
        .any(|track| track.source_path.as_deref() == Some(path))
}

fn add_media_directory_path(state: &mut LoadedState, path: &str) {
    let Some(file) = state
        .local_files
        .get(path)
        .filter(|file| file.kind == LocalFileKind::Media)
    else {
        return;
    };
    let directories = file
        .directories_to_root()
        .map(|directory| directory.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for directory in directories {
        *state.directory_media_counts.entry(directory).or_default() += 1;
    }
}

fn remove_media_directory_path(state: &mut LoadedState, path: &str) {
    let Some(file) = state.local_files.get(path) else {
        return;
    };
    let file = file.clone();
    remove_media_directory_file(state, &file);
}

fn remove_media_directory_file(state: &mut LoadedState, file: &LocalFile) {
    let directories = file
        .directories_to_root()
        .map(|directory| directory.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for directory in directories {
        let remove = if let Some(count) = state.directory_media_counts.get_mut(&directory) {
            *count = count.saturating_sub(1);
            *count == 0
        } else {
            false
        };
        if remove {
            state.directory_media_counts.remove(&directory);
        }
    }
}

fn index_local_artwork(
    albums: &LoadedItems<AlbumId, LoadedAlbum>,
    tracks: &LoadedItems<TrackId, Track>,
    artists: &LoadedItems<ArtistId, LoadedArtist>,
) -> HashMap<String, HashSet<LocalArtworkItemId>> {
    let mut index = HashMap::<String, HashSet<LocalArtworkItemId>>::new();
    for slot in albums.live_slots() {
        let album = albums.get_slot(slot).expect("live Album slot must resolve");
        if let Some(artwork) = &album.local_artwork {
            index
                .entry(artwork.path().to_string())
                .or_default()
                .insert(LocalArtworkItemId::Album(slot));
        }
    }
    for slot in tracks.live_slots() {
        let track = tracks.get_slot(slot).expect("live Track slot must resolve");
        if let Some(artwork) = &track.local_artwork {
            index
                .entry(artwork.path().to_string())
                .or_default()
                .insert(LocalArtworkItemId::Track(slot));
        }
    }
    for slot in artists.live_slots() {
        let artist = artists
            .get_slot(slot)
            .expect("live Artist slot must resolve");
        if let Some(artwork) = &artist.local_artwork {
            index
                .entry(artwork.path().to_string())
                .or_default()
                .insert(LocalArtworkItemId::Artist(slot));
        }
    }
    index
}

fn build_playlists(
    snapshots: Vec<PlaylistSnapshot>,
) -> LoadedItems<PlaylistId, Arc<LoadedPlaylist>> {
    let mut playlists = HashMap::new();
    for snapshot in snapshots {
        let id = snapshot.playlist.id.clone();
        playlists.insert(
            id.clone(),
            Arc::new(LoadedPlaylist {
                playlist: Arc::new(snapshot.playlist),
                entries: snapshot.entries.into(),
            }),
        );
    }
    LoadedItems::from_values(playlists)
}

fn replace_playlist_in_state(
    state: &mut LoadedState,
    snapshot: PlaylistSnapshot,
) -> Arc<LoadedPlaylist> {
    let id = snapshot.playlist.id.clone();
    let playlist = Arc::new(LoadedPlaylist {
        playlist: Arc::new(snapshot.playlist),
        entries: snapshot.entries.into(),
    });
    if let Some(previous) = state.playlists.get(&id).cloned() {
        let slot = state
            .playlists
            .slot(&id)
            .expect("loaded Playlist ID must have a slot");
        remove_playlist_membership(&mut state.track_playlists, slot, &previous.entries);
    }
    state.playlists.insert(id.clone(), Arc::clone(&playlist));
    let slot = state
        .playlists
        .slot(&id)
        .expect("inserted Playlist slot must resolve");
    add_playlist_membership(&mut state.track_playlists, slot, &playlist.entries);
    playlist
}

fn remove_playlist_in_state(
    state: &mut LoadedState,
    id: &PlaylistId,
) -> Option<Arc<LoadedPlaylist>> {
    let slot = state.playlists.slot(id)?;
    let removed = state.playlists.remove(id);
    if let Some(playlist) = &removed {
        remove_playlist_membership(&mut state.track_playlists, slot, &playlist.entries);
    }
    removed
}

fn index_track_playlists(
    playlists: &LoadedItems<PlaylistId, Arc<LoadedPlaylist>>,
) -> HashMap<TrackId, Vec<PlaylistSlot>> {
    let mut index = HashMap::new();
    for slot in playlists.live_slots() {
        let playlist = playlists
            .get_slot(slot)
            .expect("live Playlist slot must resolve");
        add_playlist_membership(&mut index, slot, &playlist.entries);
    }
    index
}

fn add_playlist_membership(
    index: &mut HashMap<TrackId, Vec<PlaylistSlot>>,
    playlist_slot: PlaylistSlot,
    entries: &[PlaylistEntry],
) {
    let mut seen = HashSet::new();
    for track_id in entries
        .iter()
        .map(|entry| &entry.track_id)
        .filter(|track_id| seen.insert((*track_id).clone()))
    {
        let playlists = index.entry(track_id.clone()).or_default();
        if !playlists.contains(&playlist_slot) {
            playlists.push(playlist_slot);
        }
    }
}

fn hydrate_loudness(state: &mut LoadedState, writes: Vec<crate::LoudnessMeasurementWrite>) {
    for write in writes {
        let stored = StoredLoudnessMeasurement {
            analysis_key: write.analysis_key,
            measurement: write.measurement,
        };
        match write.item {
            crate::LoudnessItemId::Track(track_id) => {
                if state
                    .tracks
                    .get(&track_id)
                    .is_some_and(|track| loudness_track_key(state, track) == write.analysis_key)
                {
                    state.track_loudness.insert(track_id, stored);
                }
            }
            crate::LoudnessItemId::Album(album_id) => {
                if loudness_album_key(state, &album_id)
                    .is_some_and(|(key, _)| key == write.analysis_key)
                {
                    state.album_loudness.insert(album_id, stored);
                }
            }
        }
    }
}

fn refresh_loudness_validity(
    state: &mut LoadedState,
    track_ids: &HashSet<TrackId>,
    album_ids: &HashSet<AlbumId>,
) {
    for track_id in track_ids {
        let current_key = state
            .tracks
            .get(track_id)
            .map(|track| loudness_track_key(state, track));
        if state
            .track_loudness
            .get(track_id)
            .is_some_and(|stored| Some(stored.analysis_key) != current_key)
        {
            state.track_loudness.remove(track_id);
        }
    }
    for album_id in album_ids {
        let current_key = loudness_album_key(state, album_id).map(|(key, _)| key);
        if state
            .album_loudness
            .get(album_id)
            .is_some_and(|stored| Some(stored.analysis_key) != current_key)
        {
            state.album_loudness.remove(album_id);
        }
    }
}

fn loudness_track_key(state: &LoadedState, track: &Track) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    hash_loudness_bytes(&mut digest, b"rufin-loudness-track");
    hash_loudness_bytes(&mut digest, &crate::LOUDNESS_ANALYSIS_VERSION.to_le_bytes());
    hash_loudness_bytes(&mut digest, track.id.as_str().as_bytes());
    hash_loudness_bytes(&mut digest, &track.duration_seconds.to_le_bytes());
    hash_loudness_optional(&mut digest, track.source_format.as_deref());
    hash_loudness_optional(&mut digest, track.source_path.as_deref());
    match &track.cue {
        Some(cue) => {
            digest.update(&[1]);
            hash_loudness_bytes(&mut digest, cue.cue_path.as_bytes());
            hash_loudness_bytes(&mut digest, &cue.start_millis.to_le_bytes());
            hash_loudness_bytes(&mut digest, &cue.end_millis.to_le_bytes());
        }
        None => {
            digest.update(&[0]);
        }
    }
    if let Some(file) = track
        .source_path
        .as_ref()
        .and_then(|path| state.local_files.get(path))
    {
        digest.update(&[1]);
        hash_loudness_bytes(
            &mut digest,
            &file.size_bytes.unwrap_or_default().to_le_bytes(),
        );
        hash_loudness_bytes(&mut digest, &file.mtime_ns.to_le_bytes());
        hash_loudness_bytes(
            &mut digest,
            &file.device_id.unwrap_or_default().to_le_bytes(),
        );
        hash_loudness_bytes(&mut digest, &file.inode.unwrap_or_default().to_le_bytes());
    } else {
        digest.update(&[0]);
    }
    *digest.finalize().as_bytes()
}

fn loudness_album_key(state: &LoadedState, album_id: &AlbumId) -> Option<([u8; 32], Vec<TrackId>)> {
    let album = state.albums.get(album_id)?;
    let mut tracks = album
        .tracks
        .iter()
        .filter_map(|slot| state.tracks.get_slot(*slot))
        .collect::<Vec<_>>();
    if tracks.is_empty() {
        return None;
    }
    tracks.sort_by(|left, right| compare_album_tracks(left, right));
    let mut digest = blake3::Hasher::new();
    hash_loudness_bytes(&mut digest, b"rufin-loudness-album");
    hash_loudness_bytes(&mut digest, &crate::LOUDNESS_ANALYSIS_VERSION.to_le_bytes());
    hash_loudness_bytes(&mut digest, album_id.as_str().as_bytes());
    let mut track_ids = Vec::with_capacity(tracks.len());
    for track in tracks {
        hash_loudness_bytes(&mut digest, track.id.as_str().as_bytes());
        hash_loudness_bytes(&mut digest, &loudness_track_key(state, track));
        track_ids.push(track.id.clone());
    }
    Some((*digest.finalize().as_bytes(), track_ids))
}

fn hash_loudness_optional(digest: &mut blake3::Hasher, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update(&[1]);
            hash_loudness_bytes(digest, value.as_bytes());
        }
        None => {
            digest.update(&[0]);
        }
    }
}

fn hash_loudness_bytes(digest: &mut blake3::Hasher, value: &[u8]) {
    digest.update(&(value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn remove_playlist_membership(
    index: &mut HashMap<TrackId, Vec<PlaylistSlot>>,
    playlist_slot: PlaylistSlot,
    entries: &[PlaylistEntry],
) {
    let track_ids = entries
        .iter()
        .map(|entry| entry.track_id.clone())
        .collect::<HashSet<_>>();
    for track_id in track_ids {
        let remove = if let Some(playlists) = index.get_mut(&track_id) {
            playlists.retain(|slot| slot != &playlist_slot);
            playlists.is_empty()
        } else {
            false
        };
        if remove {
            index.remove(&track_id);
        }
    }
}

pub(crate) fn build_smart_playlists(
    records: Vec<SmartPlaylistRecord>,
) -> HashMap<SmartPlaylistId, Arc<crate::SmartPlaylist>> {
    records
        .into_iter()
        .map(|record| {
            let id = record.id.clone();
            (
                id,
                Arc::new(crate::SmartPlaylist {
                    id: record.id,
                    name: record.name,
                    position: record.position,
                    builtin: record.builtin,
                    definition: record.definition,
                }),
            )
        })
        .collect()
}

fn apply_track_activity<'a>(
    tracks: &mut HashMap<TrackId, Track>,
    activity: impl IntoIterator<Item = &'a TrackActivity>,
) {
    for activity in activity {
        let Some(track) = tracks.get_mut(&activity.track_id) else {
            continue;
        };
        apply_track_activity_value(track, &activity);
    }
}

fn apply_favorite_values(
    albums: &mut HashMap<AlbumId, Album>,
    tracks: &mut HashMap<TrackId, Track>,
    artists: &mut HashMap<ArtistId, Artist>,
    favorites: &[FavoriteItemId],
) {
    for favorite in favorites {
        match favorite {
            FavoriteItemId::Album(id) => {
                if let Some(album) = albums.get_mut(id) {
                    album.favorite = true;
                }
            }
            FavoriteItemId::Track(id) => {
                if let Some(track) = tracks.get_mut(id) {
                    track.favorite = true;
                }
            }
            FavoriteItemId::Artist(id) => {
                if let Some(artist) = artists.get_mut(id) {
                    artist.favorite = true;
                }
            }
        }
    }
}

fn distinct_artist_credits(track: &Track) -> Vec<&ArtistCredit> {
    distinct_by_id(
        track
            .relations
            .artists
            .iter()
            .chain(track.relations.album_artists.iter()),
        |credit| &credit.id,
    )
}

fn distinct_album_artist_credits(album: &Album) -> Vec<&ArtistCredit> {
    distinct_by_id(
        album
            .relations
            .artists
            .iter()
            .chain(album.relations.album_artists.iter()),
        |credit| &credit.id,
    )
}

fn distinct_by_id<'a, Item, Id>(
    values: impl IntoIterator<Item = &'a Item>,
    id: impl Fn(&Item) -> &Id,
) -> Vec<&'a Item>
where
    Item: 'a,
    Id: Clone + Eq + std::hash::Hash + 'a,
{
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(id(value).clone()))
        .collect()
}

fn compare_tracks_by_title(left: &Track, right: &Track) -> std::cmp::Ordering {
    text_cmp(&left.title, &right.title)
        .then_with(|| text_cmp(&left.album, &right.album))
        .then(left.disc_number.cmp(&right.disc_number))
        .then(left.track_number.cmp(&right.track_number))
        .then(left.id.cmp(&right.id))
}

fn compare_album_tracks(left: &Track, right: &Track) -> std::cmp::Ordering {
    left.disc_number
        .cmp(&right.disc_number)
        .then(left.track_number.cmp(&right.track_number))
        .then_with(|| text_cmp(&left.title, &right.title))
        .then(left.id.cmp(&right.id))
}

fn text_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
}

fn nonempty_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, mem::size_of, sync::Arc};

    use crate::{
        HomeFacts, SourceId, Track, TrackData, TrackId, TrackRelations, activity::TrackActivity,
        home::HomeSessions, store::StoreLane,
    };

    use super::{ItemReplacement, Library, LibraryInput, LoadedItems, TrackSlot};

    fn track(id: TrackId) -> Track {
        Track::new(TrackData {
            id,
            album_id: None,
            title: "Test track".to_string(),
            artist: "Test artist".to_string(),
            album: "Test album".to_string(),
            album_artwork: None,
            year: 2026,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds: 180,
            favorite: false,
            disc_number: 1,
            track_number: 1,
            image_ref: None,
            local_artwork: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            source_path: None,
            cue: None,
            source_format: None,
            comment: None,
            skip_count: None,
            bpm: None,
            relations: TrackRelations::default(),
        })
    }

    fn activity(track_id: &TrackId) -> TrackActivity {
        TrackActivity {
            track_id: track_id.clone(),
            play_count: 3,
            skip_count: 1,
            last_played: Some("2026-08-09T10:00:00Z".to_string()),
        }
    }

    fn library_with(tracks: Vec<Track>, activity: Vec<TrackActivity>) -> Arc<Library> {
        let mut input = LibraryInput::new(SourceId::new("test:loaded"), 1, [1; 32], None);
        input.home = Some(HomeFacts::RufinDefined);
        input.tracks = tracks;
        input.activity = activity;
        Library::build(
            input,
            StoreLane::memory().expect("open test Store"),
            Arc::new(HomeSessions::new()),
        )
        .expect("build test Library")
    }

    #[test]
    fn loaded_track_slot_is_one_compact_index() {
        assert_eq!(size_of::<TrackSlot>(), size_of::<u32>());
    }

    #[test]
    fn removed_item_slots_stay_tombstoned_when_the_id_is_reinserted() {
        let id = "item".to_string();
        let mut items = LoadedItems::<String, usize>::from_values(HashMap::new());
        items.insert(id.clone(), 1);
        let removed_slot = items.slot(&id).expect("inserted item slot");

        assert_eq!(items.remove(&id), Some(1));
        assert!(items.slot(&id).is_none());
        assert!(items.get_slot(removed_slot).is_none());

        items.insert(id.clone(), 2);
        let replacement_slot = items.slot(&id).expect("replacement item slot");
        assert_ne!(replacement_slot, removed_slot);
        assert!(items.get_slot(removed_slot).is_none());
        assert_eq!(items.get_slot(replacement_slot), Some(&2));
    }

    #[test]
    fn activity_snapshots_contain_only_current_tracks() {
        let current_id = TrackId::new("test:track:current");
        let missing_id = TrackId::new("test:track:missing");
        let library = library_with(
            vec![track(current_id.clone())],
            vec![activity(&current_id), activity(&missing_id)],
        );

        {
            let state = library.read_state().expect("read built Library");
            assert_eq!(state.activity.len(), 1);
            assert!(state.activity.contains_key(&current_id));
            assert!(!state.activity.contains_key(&missing_id));
        }

        library
            .replace_activity_snapshot(vec![activity(&missing_id)], Vec::new())
            .expect("replace activity snapshot");
        assert!(
            library
                .read_state()
                .expect("read replaced activity snapshot")
                .activity
                .is_empty()
        );
    }

    #[test]
    fn replacing_activity_does_not_cache_a_missing_track() {
        let current_id = TrackId::new("test:track:current");
        let missing_id = TrackId::new("test:track:missing");
        let library = library_with(vec![track(current_id)], Vec::new());

        assert!(
            library
                .replace_track_activity(activity(&missing_id), None)
                .expect("replace missing Track activity")
                .is_none()
        );
        assert!(
            library
                .read_state()
                .expect("read missing Track activity")
                .activity
                .is_empty()
        );
    }

    #[test]
    fn activity_sort_changes_do_not_rebuild_smart_playlist_download_coverage() {
        let track_id = TrackId::new("test:track:activity-sort");
        let library = library_with(vec![track(track_id.clone())], Vec::new());
        library
            .initialize_smart_playlists()
            .expect("initialize smart playlists");
        library
            .replace_track_activity(activity(&track_id), None)
            .expect("apply initial activity")
            .expect("initial activity changes smart playlist membership");
        let initial_writes = library
            .read_state()
            .expect("read initial coverage")
            .download_coverage
            .smart_playlist_membership_writes();
        assert!(initial_writes > 0);

        let mut reordered = activity(&track_id);
        reordered.skip_count += 1;
        let change = library
            .replace_track_activity(reordered, None)
            .expect("apply changed skip count")
            .expect("skip count changes Most Skipped order");
        assert!(!change.smart_playlists.is_empty());
        assert_eq!(
            library
                .read_state()
                .expect("read reordered coverage")
                .download_coverage
                .smart_playlist_membership_writes(),
            initial_writes
        );
    }

    #[test]
    fn removing_a_track_removes_its_current_activity() {
        let track_id = TrackId::new("test:track:removed");
        let library = library_with(vec![track(track_id.clone())], vec![activity(&track_id)]);

        library
            .replace_source_update(
                ItemReplacement {
                    removed_tracks: vec![track_id.clone()],
                    ..ItemReplacement::default()
                },
                Vec::new(),
                Vec::new(),
            )
            .expect("remove Track");
        let state = library.read_state().expect("read removed Track");
        assert!(!state.tracks.contains_key(&track_id));
        assert!(!state.activity.contains_key(&track_id));
    }
}
