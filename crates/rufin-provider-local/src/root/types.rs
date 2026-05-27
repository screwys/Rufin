use async_trait::async_trait;
use lofty::file::TaggedFileExt;
use lofty::picture::{Picture, PictureType};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use rufin_core::{
    Album, AlbumId, Artist, ArtistCredit, ArtistId, Folder, FolderId, Genre, GenreId,
    HOME_SECTION_ITEM_LIMIT, HomeSection, HomeSectionKind, ImageRef, Playlist, PlaylistId,
    ServerId, ServerIdentity, Track, TrackId,
};
use rufin_provider::{
    AlbumDetail, FolderDetail, GenreDetail, ImageBytes, ImageKind, ImageMetadata, ImageRequest,
    MusicProvider, PagedRequest, PagedResponse, PlayedFilter, PlaylistDetail, ProviderCapabilities,
    ProviderError, ProviderIdentity, ProviderResult, RandomTrackRequest, SearchResults,
    StreamDescriptor,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;
use walkdir::WalkDir;
pub const LOCAL_PROVIDER_ID: &str = "local";
#[derive(Clone, Debug)]
pub struct LocalProvider {
    identity: ProviderIdentity,
    capabilities: ProviderCapabilities,
    library: LocalLibrary,
}
#[derive(Clone, Debug, Default)]
struct LocalLibrary {
    roots: Vec<LocalFolderEntry>,
    folders: HashMap<FolderId, LocalFolderEntry>,
    albums: Vec<Album>,
    tracks: Vec<Track>,
    artists: Vec<Artist>,
    album_artists: Vec<Artist>,
    genres: Vec<Genre>,
    covers: HashMap<String, LocalCover>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalFolderEntry {
    folder: Folder,
    path: PathBuf,
    parent_id: Option<FolderId>,
}
#[derive(Clone, Debug)]
enum LocalCover {
    File(PathBuf),
    Embedded {
        path: PathBuf,
        content_type: Option<String>,
    },
}
#[derive(Clone, Debug)]
struct ScannedTrack {
    track: Track,
    album_artist: String,
    cover: Option<LocalCover>,
}
#[derive(Clone, Debug)]
struct AlbumAccumulator {
    album: Album,
    album_artist_keys: BTreeSet<String>,
    artist_keys: BTreeSet<String>,
}
#[derive(Clone, Debug, Default)]
struct ArtistAccumulator {
    name: String,
    albums: BTreeSet<AlbumId>,
    tracks: BTreeSet<TrackId>,
}
#[derive(Clone, Debug, Default)]
struct GenreAccumulator {
    name: String,
    albums: BTreeSet<AlbumId>,
    tracks: BTreeSet<TrackId>,
}
impl LocalProvider {
    pub fn from_root(root: PathBuf) -> ProviderResult<Self> {
        let root = normalize_root(root)?;
        let server = identity_for_root(&root);
        Self::from_roots_with_identity(vec![root], server)
    }

    pub fn from_roots(roots: Vec<PathBuf>) -> ProviderResult<Self> {
        let roots = normalize_roots(roots)?;
        let server = identity_for_roots(&roots);
        Self::from_normalized_roots_with_identity(roots, server)
    }

    pub fn from_roots_with_identity(
        roots: Vec<PathBuf>,
        server: ServerIdentity,
    ) -> ProviderResult<Self> {
        let roots = normalize_roots(roots)?;
        Self::from_normalized_roots_with_identity(roots, server)
    }

    fn from_normalized_roots_with_identity(
        roots: Vec<PathBuf>,
        server: ServerIdentity,
    ) -> ProviderResult<Self> {
        Ok(Self {
            identity: ProviderIdentity { server },
            capabilities: local_capabilities(),
            library: scan_library(&roots),
        })
    }

    pub fn from_server(server: ServerIdentity) -> ProviderResult<Self> {
        let root = normalize_root(PathBuf::from(&server.base_url))?;
        Ok(Self {
            identity: ProviderIdentity { server },
            capabilities: local_capabilities(),
            library: scan_library(&[root]),
        })
    }

    pub fn identity_for_root(root: impl AsRef<Path>) -> ProviderResult<ServerIdentity> {
        let root = normalize_root(root.as_ref().to_path_buf())?;
        Ok(identity_for_root(&root))
    }
}
#[async_trait(?Send)]
impl MusicProvider for LocalProvider {
    fn identity(&self) -> &ProviderIdentity {
        &self.identity
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    async fn home_sections(&self) -> ProviderResult<Vec<HomeSection>> {
        let albums = self
            .library
            .albums
            .iter()
            .take(HOME_SECTION_ITEM_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        let tracks = self
            .library
            .tracks
            .iter()
            .take(HOME_SECTION_ITEM_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        Ok(vec![
            HomeSection {
                kind: HomeSectionKind::Explore,
                albums: albums.clone(),
                tracks: Vec::new(),
            },
            HomeSection {
                kind: HomeSectionKind::NewlyAdded,
                albums: albums.clone(),
                tracks: Vec::new(),
            },
            HomeSection {
                kind: HomeSectionKind::RecentlyReleased,
                albums,
                tracks: Vec::new(),
            },
            HomeSection {
                kind: HomeSectionKind::MostPlayed,
                albums: Vec::new(),
                tracks: tracks.clone(),
            },
            HomeSection {
                kind: HomeSectionKind::RecentlyPlayed,
                albums: Vec::new(),
                tracks,
            },
        ])
    }

    async fn albums(&self, request: PagedRequest) -> ProviderResult<PagedResponse<Album>> {
        Ok(page(&self.library.albums, request))
    }

    async fn album_detail(&self, album_id: &AlbumId) -> ProviderResult<AlbumDetail> {
        let album = self
            .library
            .albums
            .iter()
            .find(|album| album.id == *album_id)
            .cloned()
            .ok_or(ProviderError::NotFound)?;
        let tracks = self
            .library
            .tracks
            .iter()
            .filter(|track| track.album_id == *album_id)
            .cloned()
            .collect();
        Ok(AlbumDetail { album, tracks })
    }

    async fn tracks(&self, request: PagedRequest) -> ProviderResult<PagedResponse<Track>> {
        Ok(page(&self.library.tracks, request))
    }

    async fn folder(
        &self,
        folder_id: Option<&FolderId>,
        _music_folder_id: Option<&rufin_core::MusicFolderId>,
    ) -> ProviderResult<FolderDetail> {
        let Some(folder_id) = folder_id else {
            return Ok(FolderDetail {
                folder: Folder {
                    id: FolderId::new("local:folder:root"),
                    name: "Folders".to_string(),
                },
                parent_id: None,
                folders: self
                    .library
                    .roots
                    .iter()
                    .map(|entry| entry.folder.clone())
                    .collect(),
                tracks: Vec::new(),
            });
        };

        let entry = self
            .library
            .folders
            .get(folder_id)
            .ok_or(ProviderError::NotFound)?;
        let mut folders = self
            .library
            .folders
            .values()
            .filter(|candidate| candidate.parent_id.as_ref() == Some(folder_id))
            .map(|candidate| candidate.folder.clone())
            .collect::<Vec<_>>();
        folders.sort_by(folder_sort);
        let mut tracks = self
            .library
            .tracks
            .iter()
            .filter(|track| {
                track
                    .local_path
                    .as_deref()
                    .map(Path::new)
                    .and_then(Path::parent)
                    .is_some_and(|parent| parent == entry.path)
            })
            .cloned()
            .collect::<Vec<_>>();
        tracks.sort_by(|left, right| {
            left.disc_number
                .cmp(&right.disc_number)
                .then_with(|| left.track_number.cmp(&right.track_number))
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(FolderDetail {
            folder: entry.folder.clone(),
            parent_id: entry.parent_id.clone(),
            folders,
            tracks,
        })
    }

    async fn random_tracks(&self, request: RandomTrackRequest) -> ProviderResult<Vec<Track>> {
        if request.played_filter != PlayedFilter::All {
            return Err(ProviderError::Unsupported("random played filter"));
        }
        if let (Some(min_year), Some(max_year)) = (request.min_year, request.max_year)
            && min_year > max_year
        {
            return Err(ProviderError::Other(
                "minimum year cannot be greater than maximum year".to_string(),
            ));
        }

        let genre_id = request.genre_id.as_ref();
        let genre_name = request.genre_name.as_deref();
        let seed = stable_hash(&format!(
            "{}:{}:{}",
            request.min_year.unwrap_or_default(),
            request.max_year.unwrap_or_default(),
            genre_name.unwrap_or_default()
        ));
        let mut tracks = self
            .library
            .tracks
            .iter()
            .filter(|track| {
                request.min_year.is_none_or(|year| track.year >= year)
                    && request.max_year.is_none_or(|year| track.year <= year)
                    && genre_name.is_none_or(|name| {
                        track.genres.iter().any(|track_genre| track_genre == name)
                    })
                    && genre_id.is_none_or(|id| {
                        track
                            .genres
                            .iter()
                            .any(|track_genre| local_id::<GenreId>("genre", track_genre) == *id)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        tracks.sort_by_key(|track| stable_hash(&format!("{}:{seed}", track.id.as_str())));
        Ok(tracks
            .into_iter()
            .take(request.limit.clamp(1, 500))
            .collect())
    }

    async fn artists(&self, request: PagedRequest) -> ProviderResult<PagedResponse<Artist>> {
        Ok(page(&self.library.artists, request))
    }

    async fn album_artists(&self, request: PagedRequest) -> ProviderResult<PagedResponse<Artist>> {
        Ok(page(&self.library.album_artists, request))
    }

    async fn genres(&self, request: PagedRequest) -> ProviderResult<PagedResponse<Genre>> {
        Ok(page(&self.library.genres, request))
    }

    async fn playlists(&self, request: PagedRequest) -> ProviderResult<PagedResponse<Playlist>> {
        let _unused = request;
        Ok(PagedResponse::new(Vec::new(), 0))
    }

    async fn playlist_detail(&self, playlist_id: &PlaylistId) -> ProviderResult<PlaylistDetail> {
        let _unused = playlist_id;
        Err(ProviderError::NotFound)
    }

    async fn genre_detail(&self, genre_id: &GenreId) -> ProviderResult<GenreDetail> {
        let genre = self
            .library
            .genres
            .iter()
            .find(|genre| genre.id == *genre_id)
            .cloned()
            .ok_or(ProviderError::NotFound)?;
        let tracks = self
            .library
            .tracks
            .iter()
            .filter(|track| {
                track
                    .genres
                    .iter()
                    .any(|name| local_id::<GenreId>("genre", name) == genre.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let album_ids = tracks
            .iter()
            .map(|track| track.album_id.clone())
            .collect::<BTreeSet<_>>();
        let albums = self
            .library
            .albums
            .iter()
            .filter(|album| album_ids.contains(&album.id))
            .cloned()
            .collect();
        Ok(GenreDetail {
            genre,
            albums,
            tracks,
        })
    }

    async fn track(&self, track_id: &TrackId) -> ProviderResult<Track> {
        self.library
            .tracks
            .iter()
            .find(|track| track.id == *track_id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }

    async fn stream(&self, track_id: &TrackId) -> ProviderResult<StreamDescriptor> {
        let track = self.track(track_id).await?;
        let Some(local_path) = track.local_path else {
            return Err(ProviderError::NotFound);
        };
        let url = Url::from_file_path(local_path).map_err(|()| {
            ProviderError::Other("could not turn local track path into a file URI".to_string())
        })?;
        Ok(StreamDescriptor::new(url.to_string()))
    }

    async fn search(&self, query: &str) -> ProviderResult<SearchResults> {
        let query = normalize_search(query);
        if query.is_empty() {
            return Ok(SearchResults::default());
        }
        Ok(SearchResults {
            albums: self
                .library
                .albums
                .iter()
                .filter(|album| {
                    searchable_matches(&query, [&album.title, &album.artist].into_iter())
                })
                .take(50)
                .cloned()
                .collect(),
            tracks: self
                .library
                .tracks
                .iter()
                .filter(|track| {
                    searchable_matches(
                        &query,
                        [&track.title, &track.artist, &track.album].into_iter(),
                    )
                })
                .take(50)
                .cloned()
                .collect(),
            artists: self
                .library
                .artists
                .iter()
                .filter(|artist| searchable_matches(&query, [&artist.name].into_iter()))
                .take(50)
                .cloned()
                .collect(),
            playlists: Vec::new(),
        })
    }

    async fn image_metadata(
        &self,
        item_id: &str,
        kind: ImageKind,
    ) -> ProviderResult<ImageMetadata> {
        let _unused = kind;
        let cover = self
            .library
            .covers
            .get(item_id)
            .ok_or(ProviderError::NotFound)?;
        Ok(ImageMetadata {
            item_id: item_id.to_string(),
            kind: ImageKind::Primary,
            tag: None,
            url: cover_url(cover)?,
        })
    }

    async fn image_bytes(&self, request: ImageRequest) -> ProviderResult<ImageBytes> {
        let cover = self
            .library
            .covers
            .get(&request.item_id)
            .ok_or(ProviderError::NotFound)?;
        match cover {
            LocalCover::File(path) => Ok(ImageBytes {
                bytes: fs::read(path).map_err(|error| ProviderError::Other(error.to_string()))?,
                content_type: content_type_from_path(path),
            }),
            LocalCover::Embedded { path, content_type } => {
                let tagged = Probe::open(path)
                    .and_then(|probe| probe.read())
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
                let picture = tagged
                    .primary_tag()
                    .or_else(|| tagged.first_tag())
                    .and_then(|tag| select_best_picture(tag.pictures()))
                    .or_else(|| select_best_picture_from_tags(tagged.tags()))
                    .ok_or(ProviderError::NotFound)?;
                Ok(ImageBytes {
                    bytes: picture.data().to_vec(),
                    content_type: content_type.clone(),
                })
            }
        }
    }
}
fn normalize_root(root: PathBuf) -> ProviderResult<PathBuf> {
    let expanded = if root.as_os_str().is_empty() {
        std::env::current_dir().map_err(|error| ProviderError::Other(error.to_string()))?
    } else {
        root
    };
    Ok(expanded.canonicalize().unwrap_or(expanded))
}
fn normalize_roots(roots: Vec<PathBuf>) -> ProviderResult<Vec<PathBuf>> {
    let mut normalized = Vec::new();
    for root in roots {
        let root = normalize_root(root)?;
        if !normalized.iter().any(|candidate| candidate == &root) {
            normalized.push(root);
        }
    }
    Ok(normalized)
}
fn identity_for_root(root: &Path) -> ServerIdentity {
    let root_text = root.to_string_lossy().into_owned();
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Local")
        .to_string();
    ServerIdentity {
        id: ServerId::new(format!("local:server:{:016x}", stable_hash(&root_text))),
        provider: LOCAL_PROVIDER_ID.to_string(),
        name,
        base_url: root_text,
    }
}
fn identity_for_roots(roots: &[PathBuf]) -> ServerIdentity {
    if roots.len() == 1 {
        return identity_for_root(&roots[0]);
    }
    let joined = roots
        .iter()
        .map(|root| root.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    ServerIdentity {
        id: ServerId::new(format!("local:server:{:016x}", stable_hash(&joined))),
        provider: LOCAL_PROVIDER_ID.to_string(),
        name: "Local".to_string(),
        base_url: joined,
    }
}
fn local_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        favorites: false,
        lyrics: false,
        playback_reporting: false,
        playlist_mutations: false,
        favorite_mutations: false,
        auto_dj: false,
        playlists: false,
        random_tracks: true,
        folder_browsing: true,
        ..ProviderCapabilities::default()
    }
}
fn scan_library(roots: &[PathBuf]) -> LocalLibrary {
    let mut scanned = roots
        .iter()
        .flat_map(|root| WalkDir::new(root).follow_links(true).into_iter())
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| is_audio_file(path))
        .filter_map(read_track)
        .collect::<Vec<_>>();
    scanned.sort_by(|left, right| {
        left.track
            .album
            .to_lowercase()
            .cmp(&right.track.album.to_lowercase())
            .then(left.track.disc_number.cmp(&right.track.disc_number))
            .then(left.track.track_number.cmp(&right.track.track_number))
            .then(
                left.track
                    .title
                    .to_lowercase()
                    .cmp(&right.track.title.to_lowercase()),
            )
    });
    let (root_entries, folders) = scan_folders(roots);
    build_library(scanned, root_entries, folders)
}
fn scan_folders(roots: &[PathBuf]) -> (Vec<LocalFolderEntry>, HashMap<FolderId, LocalFolderEntry>) {
    let mut entries = HashMap::<FolderId, LocalFolderEntry>::new();
    let mut root_entries = Vec::new();
    for root in roots {
        for entry in WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_dir())
        {
            let path = entry.path().to_path_buf();
            let folder = folder_for_path(&path);
            let parent_id = if path == *root {
                None
            } else {
                path.parent()
                    .filter(|parent| parent.starts_with(root))
                    .map(|parent| folder_for_path(parent).id)
            };
            let local_entry = LocalFolderEntry {
                folder: folder.clone(),
                path,
                parent_id,
            };
            if entry.path() == root {
                root_entries.push(local_entry.clone());
            }
            entries.insert(folder.id.clone(), local_entry);
        }
    }
    root_entries.sort_by(|left, right| folder_sort(&left.folder, &right.folder));
    (root_entries, entries)
}
fn folder_for_path(path: &Path) -> Folder {
    let path_text = path.to_string_lossy();
    Folder {
        id: local_id("folder", &path_text),
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| path_text.as_ref())
            .to_string(),
    }
}
fn folder_sort(left: &Folder, right: &Folder) -> std::cmp::Ordering {
    left.name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then_with(|| left.id.cmp(&right.id))
}
fn read_track(path: PathBuf) -> Option<ScannedTrack> {
    let tagged_file = Probe::open(&path).and_then(|probe| probe.read()).ok();
    let tag = tagged_file
        .as_ref()
        .and_then(|file| file.primary_tag().or_else(|| file.first_tag()));
    let properties = tagged_file.as_ref().map(|file| file.properties());

    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Title")
        .to_string();
    let parent_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("Unknown Album")
        .to_string();

    let title =
        tag_string(tag, |tag| tag.title().map(|value| value.to_string())).unwrap_or(fallback_title);
    let artist = tag_string(tag, |tag| tag.artist().map(|value| value.to_string()))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album =
        tag_string(tag, |tag| tag.album().map(|value| value.to_string())).unwrap_or(parent_name);
    let album_artist = tag
        .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
        .map(ToString::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| artist.clone());
    let artist_names = artist_names(tag, &artist);
    let artist_credits = artist_names
        .iter()
        .map(|name| ArtistCredit {
            id: local_id("artist", name),
            name: name.clone(),
        })
        .collect::<Vec<_>>();
    let album_artist_credits = split_credit_names(&album_artist)
        .into_iter()
        .map(|name| ArtistCredit {
            id: local_id("artist", &name),
            name,
        })
        .collect::<Vec<_>>();
    let artist_id = artist_credits
        .first()
        .or_else(|| album_artist_credits.first())
        .map(|artist| artist.id.clone());
    let path_text = path.to_string_lossy().into_owned();
    let album_id = local_id(
        "album",
        &format!("{}:{}:{}", album_artist, album, album_grouping_path(&path)),
    );
    let genres = tag
        .and_then(|tag| tag.genre().map(|genre| split_credit_names(&genre)))
        .unwrap_or_default();
    let cover = embedded_cover(&path, tagged_file.as_ref(), tag)
        .or_else(|| path.parent().and_then(folder_cover).map(LocalCover::File));
    let year = tag
        .and_then(|tag| tag.date())
        .map(|date| date.year)
        .unwrap_or_default();
    let duration_seconds = properties
        .map(|properties| properties.duration().as_secs().min(u64::from(u32::MAX)) as u32)
        .unwrap_or_default();

    Some(ScannedTrack {
        track: Track {
            id: local_id("track", &path_text),
            album_id,
            title,
            artist,
            artist_id,
            artist_credits,
            album_artist_credits,
            album,
            year,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds,
            favorite: false,
            disc_number: tag
                .and_then(|tag| tag.disk())
                .unwrap_or(1)
                .min(u32::from(u16::MAX)) as u16,
            track_number: tag
                .and_then(|tag| tag.track())
                .unwrap_or_default()
                .min(u32::from(u16::MAX)) as u16,
            image_ref: None,
            genres,
            local_path: Some(path_text),
            source_format: path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
        },
        album_artist,
        cover,
    })
}
