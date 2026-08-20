//! Native-first radio composition with one common Library fallback.
//!
//! Sources acquire native recommendations. Library admits them, deduplicates
//! every stage, and underfills from genre, artist, then the complete selected
//! source. Rufin owns orchestration and Playback owns queue mutation.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    AlbumId, ArtistId, GenreId, Library, LibraryQueryError, MusicFolderId, PlaylistId, Track,
    TrackId,
    loaded::{AlbumSlot, LoadedItems, LoadedState, TrackSlot},
    local_playback::playable_file_for,
};

const RADIO_CANDIDATE_MULTIPLIER: usize = 8;
const MAX_RADIO_CANDIDATES: usize = 500;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RadioSeed {
    Track(TrackId),
    Album(AlbumId),
    Artist(ArtistId),
    Genre { id: GenreId, name: String },
    Playlist(PlaylistId),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlayedFilter {
    #[default]
    All,
    Unplayed,
    Played,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RandomCriteria {
    pub limit: usize,
    pub min_year: Option<u16>,
    pub max_year: Option<u16>,
    pub genre_id: Option<GenreId>,
    pub genre_name: Option<String>,
    pub played_filter: PlayedFilter,
}

#[derive(Clone, Debug)]
pub struct RadioComposition {
    pub seed: RadioSeed,
    pub native: Option<Vec<Track>>,
    pub excluded_track_ids: Vec<TrackId>,
    pub limit: usize,
    pub include_seed_track: bool,
    pub require_local_playback: bool,
    pub variation: u64,
}

#[derive(Clone, Debug)]
pub struct RandomComposition {
    pub native: Vec<Track>,
    pub criteria: RandomCriteria,
    pub music_folder_id: Option<MusicFolderId>,
    pub variation: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RadioUnavailable {
    #[error("the radio seed is no longer available")]
    MissingSeed,
    #[error("no matching radio tracks were found")]
    Empty,
    #[error(transparent)]
    Query(#[from] LibraryQueryError),
}

impl Library {
    pub fn compose_random(
        &self,
        request: RandomComposition,
    ) -> Result<Vec<Track>, LibraryQueryError> {
        let criteria = request.criteria;
        let limit = criteria.limit.clamp(1, 500);
        let state = self.read_state()?;
        let mut seen = HashSet::new();
        let mut tracks = Vec::with_capacity(limit);
        for track in request.native {
            if tracks.len() == limit {
                break;
            }
            if !seen.insert(track.id.clone()) {
                continue;
            }
            tracks.push(state.tracks.get(&track.id).cloned().unwrap_or(track));
        }

        let capacity = state.tracks.slot_capacity();
        let start = usize::try_from(request.variation % capacity.max(1) as u64)
            .expect("the random Track start fits usize");
        for offset in 0..capacity {
            if tracks.len() == limit {
                break;
            }
            let Some(track) = state.tracks.get_index((start + offset) % capacity) else {
                continue;
            };
            if !seen.insert(track.id.clone()) {
                continue;
            }
            if !request
                .music_folder_id
                .as_ref()
                .is_none_or(|folder| track.relations.music_folders.contains(folder))
            {
                continue;
            }
            if !criteria.min_year.is_none_or(|year| track.year >= year)
                || !criteria.max_year.is_none_or(|year| track.year <= year)
            {
                continue;
            }
            if !criteria
                .genre_id
                .as_ref()
                .is_none_or(|genre_id| track_has_genre_id(&state, track, genre_id))
            {
                continue;
            }
            if !criteria
                .genre_name
                .as_deref()
                .is_none_or(|genre_name| track_has_genre_name(&state, track, genre_name))
            {
                continue;
            }
            if !match criteria.played_filter {
                PlayedFilter::All => true,
                PlayedFilter::Unplayed => track.play_count.unwrap_or_default() == 0,
                PlayedFilter::Played => track.play_count.unwrap_or_default() > 0,
            } {
                continue;
            }
            tracks.push(track.clone());
        }
        Ok(tracks)
    }

    pub fn compose_radio(&self, request: RadioComposition) -> Result<Vec<Track>, RadioUnavailable> {
        let limit = request.limit.clamp(1, 500);
        let candidate_limit = limit
            .saturating_mul(RADIO_CANDIDATE_MULTIPLIER)
            .clamp(limit, MAX_RADIO_CANDIDATES);
        let selection_limit = if request.require_local_playback {
            candidate_limit
        } else {
            limit
        };
        let context = radio_context(self, &request.seed)?;
        let seed_key = request.seed.key();
        let mut excluded = request
            .excluded_track_ids
            .into_iter()
            .collect::<HashSet<_>>();
        if let Some(seed_track) = &context.seed_track {
            excluded.insert(seed_track.id.clone());
        }
        let mut seen = excluded.clone();
        let mut selected = Vec::with_capacity(selection_limit);

        if let Some(candidates) = request.native {
            let state = self.read_state()?;
            let mut admitted = Vec::with_capacity(candidate_limit);
            for track in candidates {
                if admitted.len() == candidate_limit {
                    break;
                }
                if !seen.insert(track.id.clone()) {
                    continue;
                }
                admitted.push(state.tracks.get(&track.id).cloned().unwrap_or(track));
            }
            drop(state);
            append_stage(
                &seed_key,
                request.variation,
                admitted,
                selection_limit,
                &mut selected,
            );
        }

        if selected.len() < selection_limit {
            let state = self.read_state()?;
            let mut genre = Vec::new();
            for (index, id) in context.genre_ids.iter().enumerate() {
                let Some(loaded_genre) = state.genres.get(id) else {
                    continue;
                };
                append_relationship_tracks(
                    &state,
                    &loaded_genre.tracks,
                    &loaded_genre.albums,
                    context.excluded_album.as_ref(),
                    &mut seen,
                    request
                        .variation
                        .wrapping_add(u64::try_from(index).expect("a Genre index fits u64")),
                    candidate_limit,
                    &mut genre,
                );
                if genre.len() == candidate_limit {
                    break;
                }
            }
            append_stage(
                &seed_key,
                request.variation.wrapping_add(1),
                genre,
                selection_limit,
                &mut selected,
            );

            let mut artist = Vec::new();
            if selected.len() < selection_limit {
                for (index, id) in context.artist_ids.iter().enumerate() {
                    let Some(loaded_artist) = state.artists.get(id) else {
                        continue;
                    };
                    append_relationship_tracks(
                        &state,
                        &loaded_artist.tracks,
                        &loaded_artist.albums,
                        context.excluded_album.as_ref(),
                        &mut seen,
                        request
                            .variation
                            .wrapping_add(u64::try_from(index).expect("an Artist index fits u64")),
                        candidate_limit,
                        &mut artist,
                    );
                    if artist.len() == candidate_limit {
                        break;
                    }
                }
                append_stage(
                    &seed_key,
                    request.variation.wrapping_add(2),
                    artist,
                    selection_limit,
                    &mut selected,
                );
            }

            if selected.len() < selection_limit {
                let mut random = Vec::new();
                append_source_tracks(
                    &state.tracks,
                    context.excluded_album.as_ref(),
                    &mut seen,
                    request.variation.wrapping_add(3),
                    candidate_limit,
                    &mut random,
                );
                append_stage(
                    &seed_key,
                    request.variation.wrapping_add(3),
                    random,
                    selection_limit,
                    &mut selected,
                );
            }
        }

        if request.include_seed_track
            && let Some(seed) = context.seed_track
        {
            selected.insert(0, seed);
        }
        if request.require_local_playback {
            let state = self.read_state()?;
            selected.retain(|track| locally_available(&state, track));
            selected.truncate(limit);
        }
        if selected.is_empty() {
            return Err(RadioUnavailable::Empty);
        }
        Ok(selected)
    }
}

fn locally_available(state: &LoadedState, track: &Track) -> bool {
    state
        .downloaded_files
        .get(&track.id)
        .is_some_and(|path| path.is_file())
        || playable_file_for(
            track,
            &state.local_files,
            state.local_access_mapping.as_ref(),
            &state.local_access,
            &state.local_access_index,
        )
        .is_some_and(|file| file.path().is_file())
}

struct RadioContext {
    seed_track: Option<Track>,
    genre_ids: Vec<GenreId>,
    artist_ids: Vec<ArtistId>,
    excluded_album: Option<AlbumId>,
}

fn radio_context(loaded: &Library, seed: &RadioSeed) -> Result<RadioContext, RadioUnavailable> {
    let state = loaded.read_state()?;
    let context = match seed {
        RadioSeed::Track(track_id) => {
            let track = state
                .tracks
                .get(track_id)
                .ok_or(RadioUnavailable::MissingSeed)?;
            context_from_track(track, None)
        }
        RadioSeed::Album(album_id) => {
            let album = state
                .albums
                .get(album_id)
                .ok_or(RadioUnavailable::MissingSeed)?;
            RadioContext {
                seed_track: None,
                genre_ids: distinct(
                    album
                        .relations
                        .genres
                        .iter()
                        .map(|credit| credit.id.clone()),
                ),
                artist_ids: distinct(
                    album
                        .relations
                        .album_artists
                        .iter()
                        .chain(album.relations.artists.iter())
                        .map(|credit| credit.id.clone()),
                ),
                excluded_album: Some(album.id.clone()),
            }
        }
        RadioSeed::Artist(artist_id) => {
            state
                .artists
                .get(artist_id)
                .ok_or(RadioUnavailable::MissingSeed)?;
            RadioContext {
                seed_track: None,
                genre_ids: Vec::new(),
                artist_ids: vec![artist_id.clone()],
                excluded_album: None,
            }
        }
        RadioSeed::Genre {
            id: genre_id,
            name: _,
        } => {
            state
                .genres
                .get(genre_id)
                .ok_or(RadioUnavailable::MissingSeed)?;
            RadioContext {
                seed_track: None,
                genre_ids: vec![genre_id.clone()],
                artist_ids: Vec::new(),
                excluded_album: None,
            }
        }
        RadioSeed::Playlist(playlist_id) => {
            let loaded_playlist = state
                .playlists
                .get(playlist_id)
                .ok_or(RadioUnavailable::MissingSeed)?;
            let first = loaded_playlist
                .entries
                .first()
                .ok_or(RadioUnavailable::MissingSeed)?;
            let seed = state
                .tracks
                .get(&first.track_id)
                .ok_or(RadioUnavailable::MissingSeed)?;
            context_from_track(seed, None)
        }
    };
    Ok(context)
}

fn context_from_track(track: &Track, excluded_album: Option<AlbumId>) -> RadioContext {
    RadioContext {
        seed_track: Some(track.clone()),
        genre_ids: distinct(
            track
                .relations
                .genres
                .iter()
                .map(|credit| credit.id.clone()),
        ),
        artist_ids: distinct(
            track
                .relations
                .artists
                .iter()
                .chain(track.relations.album_artists.iter())
                .map(|credit| credit.id.clone()),
        ),
        excluded_album,
    }
}

fn append_relationship_tracks(
    state: &LoadedState,
    direct_tracks: &[TrackSlot],
    albums: &[AlbumSlot],
    excluded_album: Option<&AlbumId>,
    seen: &mut HashSet<TrackId>,
    variation: u64,
    candidate_limit: usize,
    output: &mut Vec<Track>,
) {
    append_index_tracks(
        cyclic_slots(direct_tracks, variation),
        &state.tracks,
        excluded_album,
        seen,
        candidate_limit,
        output,
    );
    for (index, album_slot) in cyclic_slots(albums, variation.rotate_left(17)).enumerate() {
        if output.len() == candidate_limit {
            break;
        }
        let Some(album) = state.albums.get_slot(album_slot) else {
            continue;
        };
        append_index_tracks(
            cyclic_slots(
                &album.tracks,
                variation
                    .rotate_right(11)
                    .wrapping_add(u64::try_from(index).expect("an Album index fits u64")),
            ),
            &state.tracks,
            excluded_album,
            seen,
            candidate_limit,
            output,
        );
    }
}

fn append_index_tracks(
    ids: impl IntoIterator<Item = TrackSlot>,
    tracks: &LoadedItems<TrackId, Track>,
    excluded_album: Option<&AlbumId>,
    seen: &mut HashSet<TrackId>,
    candidate_limit: usize,
    output: &mut Vec<Track>,
) {
    for slot in ids {
        if output.len() == candidate_limit {
            break;
        }
        let Some(track) = tracks.get_slot(slot) else {
            continue;
        };
        if excluded_album.is_some_and(|album_id| track.album_id.as_ref() == Some(album_id))
            || !seen.insert(track.id.clone())
        {
            continue;
        }
        output.push(track.clone());
    }
}

fn append_source_tracks(
    tracks: &LoadedItems<TrackId, Track>,
    excluded_album: Option<&AlbumId>,
    seen: &mut HashSet<TrackId>,
    variation: u64,
    candidate_limit: usize,
    output: &mut Vec<Track>,
) {
    let capacity = tracks.slot_capacity();
    let start = variation_offset(variation, capacity);
    for offset in 0..capacity {
        if output.len() == candidate_limit {
            break;
        }
        let Some(track) = tracks.get_index((start + offset) % capacity) else {
            continue;
        };
        if excluded_album.is_some_and(|album_id| track.album_id.as_ref() == Some(album_id))
            || !seen.insert(track.id.clone())
        {
            continue;
        }
        output.push(track.clone());
    }
}

fn cyclic_slots<Id>(
    slots: &[crate::loaded::ItemSlot<Id>],
    variation: u64,
) -> impl Iterator<Item = crate::loaded::ItemSlot<Id>> + '_ {
    let start = variation_offset(variation, slots.len());
    slots[start..].iter().chain(slots[..start].iter()).copied()
}

fn variation_offset(variation: u64, len: usize) -> usize {
    usize::try_from(variation % len.max(1) as u64).expect("a loaded item offset fits usize")
}

fn track_has_genre_id(state: &LoadedState, track: &Track, genre_id: &GenreId) -> bool {
    track
        .relations
        .genres
        .iter()
        .any(|genre| &genre.id == genre_id)
        || track.album_id.as_ref().is_some_and(|album_id| {
            state.albums.get(album_id).is_some_and(|album| {
                album
                    .relations
                    .genres
                    .iter()
                    .any(|genre| &genre.id == genre_id)
            })
        })
}

fn track_has_genre_name(state: &LoadedState, track: &Track, genre_name: &str) -> bool {
    track
        .genre_names()
        .any(|name| name.eq_ignore_ascii_case(genre_name))
        || track.album_id.as_ref().is_some_and(|album_id| {
            state.albums.get(album_id).is_some_and(|album| {
                album
                    .relations
                    .genres
                    .iter()
                    .any(|genre| genre.name.eq_ignore_ascii_case(genre_name))
            })
        })
}

fn append_stage(
    seed_key: &str,
    variation: u64,
    candidates: Vec<Track>,
    limit: usize,
    selected: &mut Vec<Track>,
) {
    let remaining = limit.saturating_sub(selected.len());
    if remaining == 0 {
        return;
    }
    let mut album_order = Vec::<String>::new();
    let mut albums = HashMap::<String, Vec<Track>>::new();
    for track in candidates {
        let album_key = track
            .album_id
            .as_ref()
            .map_or_else(|| format!("track:{}", track.id), |id| format!("album:{id}"));
        if !albums.contains_key(&album_key) {
            album_order.push(album_key.clone());
        }
        albums.entry(album_key).or_default().push(track);
    }
    album_order.sort_by_key(|album| radio_hash(seed_key, variation, album));
    let first_pass = if album_order.len() <= 1 {
        remaining
    } else {
        remaining
            .div_ceil(album_order.len())
            .saturating_add(1)
            .clamp(2, 3)
    };
    for tracks in albums.values_mut() {
        tracks.sort_by_key(|track| radio_hash(seed_key, variation, track.id.as_str()));
    }
    for index in 0..first_pass {
        for album in &album_order {
            if selected.len() == limit {
                return;
            }
            if let Some(track) = albums.get(album).and_then(|tracks| tracks.get(index)) {
                selected.push(track.clone());
            }
        }
    }
    let mut deferred = album_order
        .into_iter()
        .flat_map(|album| {
            albums
                .remove(&album)
                .unwrap_or_default()
                .into_iter()
                .skip(first_pass)
        })
        .collect::<Vec<_>>();
    deferred.sort_by_key(|track| radio_hash(seed_key, variation, track.id.as_str()));
    selected.extend(
        deferred
            .into_iter()
            .take(limit.saturating_sub(selected.len())),
    );
}

fn radio_hash(seed: &str, variation: u64, value: &str) -> u64 {
    const OFFSET: u64 = 14_695_981_039_346_656_037;
    const PRIME: u64 = 1_099_511_628_211;
    seed.bytes()
        .chain(variation.to_le_bytes())
        .chain([0xff])
        .chain(value.bytes())
        .fold(OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(PRIME)
        })
}

fn distinct<T>(values: impl IntoIterator<Item = T>) -> Vec<T>
where
    T: Clone + Eq + std::hash::Hash,
{
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

impl RadioSeed {
    fn key(&self) -> String {
        match self {
            Self::Track(id) => format!("track:{id}"),
            Self::Album(id) => format!("album:{id}"),
            Self::Artist(id) => format!("artist:{id}"),
            Self::Genre { id, name: _ } => format!("genre:{id}"),
            Self::Playlist(id) => format!("playlist:{id}"),
        }
    }
}
