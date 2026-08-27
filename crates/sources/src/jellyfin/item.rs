use std::collections::HashMap;
use std::path::Path;

pub(super) use crate::NativeImageRef as ImageRef;
use serde::Deserialize;

use crate::policy::{normalized_date, u16_from_option};

use super::{jellyfin_id, stable_hash};

pub(super) async fn stage_album(
    scan: &mut library::Scan,
    album: Album,
) -> library::LibraryResult<()> {
    let artwork = album
        .image_ref
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    scan.write_album(
        &album.id,
        &album.title,
        &album.title.to_lowercase(),
        &album.artist,
        &album.title.to_lowercase(),
        Some(i64::from(album.year)).filter(|year| *year > 0),
        album.release_date.as_deref(),
        album.date_added.as_deref(),
        album.musicbrainz_album_id.as_deref(),
        album.musicbrainz_release_group_id.as_deref(),
        album.is_compilation,
        artwork.as_deref(),
        album.favorite,
        album.user_rating.map(i64::from),
        None,
    )
    .await?;
    let effective_album_artists = if album.relations.album_artists.is_empty() {
        &album.relations.artists
    } else {
        &album.relations.album_artists
    };
    for artist in effective_album_artists {
        stage_artist_credit(scan, artist).await?;
    }
    for genre in &album.relations.genres {
        stage_genre_credit(scan, genre).await?;
    }
    scan.write_album_relations(
        &effective_album_artists
            .iter()
            .map(|artist| (album.id.as_str(), artist.id.as_str()))
            .collect::<Vec<_>>(),
        &album
            .relations
            .genres
            .iter()
            .map(|genre| (album.id.as_str(), genre.id.as_str()))
            .collect::<Vec<_>>(),
        &album
            .release_types
            .iter()
            .map(|release_type| (album.id.as_str(), release_type.as_str()))
            .collect::<Vec<_>>(),
    )
    .await?;
    Ok(())
}

pub(super) async fn stage_track(
    scan: &mut library::Scan,
    track: Track,
) -> library::LibraryResult<()> {
    let artwork = track
        .image_ref
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    let mut audio = blake3::Hasher::new();
    audio.update(b"rufin-jellyfin-audio-v1\0");
    for value in [
        track.id.as_str(),
        track.source_path.as_deref().unwrap_or_default(),
        track.source_format.as_deref().unwrap_or_default(),
    ] {
        audio.update(&(value.len() as u64).to_le_bytes());
        audio.update(value.as_bytes());
    }
    audio.update(&track.duration_seconds.to_le_bytes());
    let key = *audio.finalize().as_bytes();
    let normalized_search = format!(
        "{} {} {} {}",
        track.title,
        track.album,
        track.artist,
        track.comment.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    scan.write_track(
        &track.id,
        track.album_id.as_deref(),
        &track.title,
        &normalized_search,
        &track.album,
        &track.artist,
        &track.title.to_lowercase(),
        i64::from(track.duration_seconds) * 1_000,
        i64::from(track.disc_number),
        i64::from(track.track_number),
        Some(i64::from(track.year)).filter(|year| *year > 0),
        track.release_date.as_deref(),
        track.date_added.as_deref(),
        None,
        track.source_format.as_deref(),
        track.comment.as_deref(),
        track.bpm.map(i64::from),
        track.musicbrainz_recording_id.as_deref(),
        track.musicbrainz_release_track_id.as_deref(),
        None,
        None,
        None,
        artwork.as_deref(),
        track.favorite,
        track.user_rating.map(i64::from),
        None,
        track.play_count.map(i64::from),
        track.skip_count.map(i64::from),
        track.last_played,
        track.source_path.as_deref(),
        key,
    )
    .await?;
    if track.replay_gain_track_db.is_some() {
        scan.write_track_source_loudness(&track.id, None, None, track.replay_gain_track_db, None)
            .await?;
    }
    if let Some(album_id) = track.album_id.as_deref()
        && track.replay_gain_album_db.is_some()
    {
        scan.write_album_source_loudness(album_id, None, None, track.replay_gain_album_db, None)
            .await?;
    }
    for artist in &track.relations.artists {
        stage_artist_credit(scan, artist).await?;
    }
    for artist in &track.relations.album_artists {
        stage_artist_credit(scan, artist).await?;
    }
    for genre in &track.relations.genres {
        stage_genre_credit(scan, genre).await?;
    }
    scan.write_track_relations(
        &track
            .relations
            .artists
            .iter()
            .map(|artist| (track.id.as_str(), artist.id.as_str()))
            .collect::<Vec<_>>(),
        &track
            .relations
            .genres
            .iter()
            .map(|genre| (track.id.as_str(), genre.id.as_str()))
            .collect::<Vec<_>>(),
        &[],
    )
    .await?;
    Ok(())
}

pub(super) async fn stage_artist(
    scan: &mut library::Scan,
    artist: Artist,
) -> library::LibraryResult<()> {
    let artwork = artist
        .image_ref
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    scan.write_artist(
        &artist.id,
        &artist.name,
        &artist.name.to_lowercase(),
        &artist.name.to_lowercase(),
        artist.musicbrainz_artist_id.as_deref(),
        artwork.as_deref(),
        artist.favorite,
        artist.user_rating.map(i64::from),
    )
    .await
}

pub(super) async fn stage_genre(
    scan: &mut library::Scan,
    genre: Genre,
) -> library::LibraryResult<()> {
    let artwork = genre
        .image_ref
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    scan.write_genre(
        &genre.id,
        &genre.name,
        &genre.name.to_lowercase(),
        &genre.name.to_lowercase(),
        artwork.as_deref(),
    )
    .await
}

async fn stage_artist_credit(
    scan: &mut library::Scan,
    artist: &ArtistCredit,
) -> library::LibraryResult<()> {
    scan.write_artist(
        &artist.id,
        &artist.name,
        &artist.name.to_lowercase(),
        &artist.name.to_lowercase(),
        artist.musicbrainz_artist_id.as_deref(),
        None,
        false,
        None,
    )
    .await
}

async fn stage_genre_credit(
    scan: &mut library::Scan,
    genre: &GenreCredit,
) -> library::LibraryResult<()> {
    scan.write_genre(
        &genre.id,
        &genre.name,
        &genre.name.to_lowercase(),
        &genre.name.to_lowercase(),
        None,
    )
    .await
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ArtistCredit {
    pub id: String,
    pub name: String,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GenreCredit {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct AlbumRelations {
    pub album_artists: Vec<ArtistCredit>,
    pub artists: Vec<ArtistCredit>,
    pub genres: Vec<GenreCredit>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TrackRelations {
    pub artists: Vec<ArtistCredit>,
    pub album_artists: Vec<ArtistCredit>,
    pub genres: Vec<GenreCredit>,
    pub moods: Vec<String>,
    pub music_folders: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Album {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub year: u16,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub last_played: Option<String>,
    pub play_count: Option<u32>,
    pub user_rating: Option<u8>,
    pub favorite: bool,
    pub color_seed: u32,
    pub image_ref: Option<ImageRef>,
    pub local_artwork: Option<()>,
    pub release_types: Vec<String>,
    pub is_compilation: Option<bool>,
    pub musicbrainz_album_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub relations: AlbumRelations,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Track {
    pub id: String,
    pub album_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artwork: Option<()>,
    pub year: u16,
    pub release_date: Option<String>,
    pub date_added: Option<String>,
    pub last_played: Option<i64>,
    pub play_count: Option<u32>,
    pub user_rating: Option<u8>,
    pub duration_seconds: u32,
    pub favorite: bool,
    pub disc_number: u16,
    pub track_number: u16,
    pub image_ref: Option<ImageRef>,
    pub local_artwork: Option<()>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub source_path: Option<String>,
    pub cue: Option<()>,
    pub source_format: Option<String>,
    pub comment: Option<String>,
    pub skip_count: Option<u32>,
    pub bpm: Option<u16>,
    pub replay_gain_track_db: Option<f64>,
    pub replay_gain_album_db: Option<f64>,
    pub relations: TrackRelations,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Artist {
    pub id: String,
    pub name: String,
    pub favorite: bool,
    pub last_played: Option<String>,
    pub play_count: Option<u32>,
    pub user_rating: Option<u8>,
    pub image_ref: Option<ImageRef>,
    pub local_artwork: Option<()>,
    pub musicbrainz_artist_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Genre {
    pub id: String,
    pub name: String,
    pub image_ref: Option<ImageRef>,
    pub local_artwork: Option<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Playlist {
    pub id: String,
    pub name: String,
    pub image_ref: Option<ImageRef>,
    pub duration_seconds: u32,
    pub track_count: usize,
}

pub(super) const ALBUM_FIELDS: &str = "Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,ChildCount";
pub(super) const TRACK_FIELDS: &str = "Path,Overview,Container,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,AlbumId,AlbumPrimaryImageTag,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,NormalizationGain,AlbumNormalizationGain";
pub(super) const PLAYLIST_FIELDS: &str = "RunTimeTicks,ImageTags,ChildCount";
pub(super) const MIXED_ITEM_FIELDS: &str = "Path,Overview,Container,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,ParentId,AlbumId,AlbumPrimaryImageTag,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,ChildCount,AlbumCount,SongCount,NormalizationGain,AlbumNormalizationGain";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ItemQueryResult {
    #[serde(default)]
    pub(super) items: Vec<JellyfinItem>,
    pub(super) total_record_count: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct JellyfinItem {
    pub(super) id: String,
    pub(super) name: Option<String>,
    overview: Option<String>,
    #[serde(rename = "Type")]
    pub(super) item_type: Option<String>,
    pub(super) collection_type: Option<String>,
    album_artist: Option<String>,
    album_artists: Option<Vec<NameIdPair>>,
    artists: Option<Vec<String>>,
    genre_items: Option<Vec<NameIdPair>>,
    artist_items: Option<Vec<NameIdPair>>,
    provider_ids: Option<HashMap<String, String>>,
    album: Option<String>,
    pub(super) album_id: Option<String>,
    album_primary_image_tag: Option<String>,
    path: Option<String>,
    container: Option<String>,
    production_year: Option<i32>,
    date_created: Option<String>,
    premiere_date: Option<String>,
    run_time_ticks: Option<i64>,
    index_number: Option<i32>,
    parent_index_number: Option<i32>,
    child_count: Option<i32>,
    user_data: Option<UserData>,
    pub(super) image_tags: Option<HashMap<String, String>>,
    backdrop_image_tags: Option<Vec<String>>,
    parent_backdrop_item_id: Option<String>,
    parent_backdrop_image_tags: Option<Vec<String>>,
    pub(super) playlist_item_id: Option<String>,
    normalization_gain: Option<f64>,
    album_normalization_gain: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct NameIdPair {
    name: Option<String>,
    id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UserData {
    is_favorite: Option<bool>,
    play_count: Option<i32>,
    last_played_date: Option<String>,
    rating: Option<f64>,
}

pub(super) fn album_from_item(item: JellyfinItem) -> Album {
    let item_id = item.id.clone();
    let image_ref = primary_image_ref("album", &item.id, &item.image_tags)
        .or_else(|| backdrop_image_ref(&item));
    let album_artist_credits = artist_credits_from_pairs(item.album_artists.as_deref());
    let artist_credits = artist_credits_from_pairs(item.artist_items.as_deref());
    let genres = genre_credits_from_pairs(item.genre_items.as_deref());
    let artist = item
        .album_artist
        .clone()
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| joined_credit_names(&album_artist_credits))
        .or_else(|| {
            item.artists
                .as_ref()
                .and_then(|artists| joined_artist_names(Some(artists)))
        })
        .unwrap_or_else(|| "Unknown Artist".to_string());
    Album {
        id: String::from(jellyfin_id("album", &item.id)),
        title: item.name.unwrap_or_else(|| "Untitled Album".to_string()),
        artist,
        year: u16_from_option(item.production_year),
        release_date: normalized_date(item.premiere_date),
        date_added: normalized_date(item.date_created),
        last_played: normalized_timestamp(
            item.user_data
                .as_ref()
                .and_then(|data| data.last_played_date.clone()),
        ),
        play_count: play_count(&item.user_data),
        user_rating: user_rating(&item.user_data),
        favorite: favorite(&item.user_data),
        color_seed: color_seed(&item_id),
        image_ref,
        local_artwork: None,
        release_types: Vec::new(),
        is_compilation: None,
        musicbrainz_album_id: source_id(&item.provider_ids, "MusicBrainzAlbum"),
        musicbrainz_release_group_id: source_id(&item.provider_ids, "MusicBrainzReleaseGroup"),
        relations: AlbumRelations {
            album_artists: album_artist_credits,
            artists: artist_credits,
            genres,
        },
    }
}

pub(super) fn track_from_item(item: JellyfinItem) -> Track {
    let image_ref = album_image_ref(&item)
        .or_else(|| primary_image_ref("track", &item.id, &item.image_tags))
        .or_else(|| backdrop_image_ref(&item));
    let artist_credits = artist_credits_from_pairs(item.artist_items.as_deref());
    let album_artist_credits = artist_credits_from_pairs(item.album_artists.as_deref());
    let genres = genre_credits_from_pairs(item.genre_items.as_deref());
    let album_id = item
        .album_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| String::from(jellyfin_id("album", id)));
    let source_format = source_format_from_item(item.container.as_deref(), item.path.as_deref());
    Track {
        id: String::from(jellyfin_id("track", &item.id)),
        album_id,
        title: item.name.unwrap_or_else(|| "Untitled Track".to_string()),
        artist: item
            .artists
            .as_ref()
            .and_then(|artists| joined_artist_names(Some(artists)))
            .or_else(|| joined_credit_names(&artist_credits))
            .unwrap_or_else(|| {
                item.album_artist
                    .unwrap_or_else(|| "Unknown Artist".to_string())
            }),
        album: item.album.unwrap_or_else(|| "Unknown Album".to_string()),
        album_artwork: None,
        year: u16_from_option(item.production_year),
        release_date: normalized_date(item.premiere_date),
        date_added: normalized_date(item.date_created),
        last_played: crate::policy::unix_seconds(
            item.user_data
                .as_ref()
                .and_then(|data| data.last_played_date.clone()),
        ),
        play_count: play_count(&item.user_data),
        user_rating: user_rating(&item.user_data),
        duration_seconds: duration_seconds(item.run_time_ticks),
        favorite: favorite(&item.user_data),
        disc_number: u16_from_option(item.parent_index_number),
        track_number: u16_from_option(item.index_number),
        image_ref,
        local_artwork: None,
        musicbrainz_recording_id: source_id(&item.provider_ids, "MusicBrainzRecording"),
        musicbrainz_release_track_id: source_id(&item.provider_ids, "MusicBrainzTrack"),
        source_path: item.path,
        cue: None,
        source_format,
        comment: item.overview.filter(|value| !value.trim().is_empty()),
        skip_count: None,
        bpm: None,
        replay_gain_track_db: item.normalization_gain.filter(|value| value.is_finite()),
        replay_gain_album_db: item
            .album_normalization_gain
            .filter(|value| value.is_finite()),
        relations: TrackRelations {
            artists: artist_credits,
            album_artists: album_artist_credits,
            genres,
            moods: Vec::new(),
            music_folders: Vec::new(),
        },
    }
}

fn source_format_from_item(container: Option<&str>, path: Option<&str>) -> Option<String> {
    container
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            let raw_path = path?;
            let path = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}

pub(super) fn is_audio_item(item: &JellyfinItem) -> bool {
    item.item_type
        .as_deref()
        .is_some_and(|item_type| item_type.eq_ignore_ascii_case("Audio"))
}

pub(super) fn artist_from_item(item: JellyfinItem) -> Artist {
    Artist {
        id: String::from(jellyfin_id("artist", &item.id)),
        name: item.name.unwrap_or_else(|| "Unknown Artist".to_string()),
        favorite: favorite(&item.user_data),
        last_played: normalized_timestamp(
            item.user_data
                .as_ref()
                .and_then(|data| data.last_played_date.clone()),
        ),
        play_count: play_count(&item.user_data),
        user_rating: user_rating(&item.user_data),
        musicbrainz_artist_id: source_id(&item.provider_ids, "MusicBrainzArtist"),
        image_ref: primary_image_ref("artist", &item.id, &item.image_tags),
        local_artwork: None,
    }
}

pub(super) fn genre_from_item(item: JellyfinItem) -> Genre {
    Genre {
        id: String::from(jellyfin_id("genre", &item.id)),
        name: item.name.unwrap_or_else(|| "Unknown Genre".to_string()),
        image_ref: primary_image_ref("genre", &item.id, &item.image_tags),
        local_artwork: None,
    }
}

pub(super) fn playlist_from_item(item: JellyfinItem) -> Playlist {
    Playlist {
        id: String::from(jellyfin_id("playlist", &item.id)),
        name: item.name.unwrap_or_else(|| "Untitled Playlist".to_string()),
        image_ref: primary_image_ref("playlist", &item.id, &item.image_tags),
        duration_seconds: duration_seconds(item.run_time_ticks),
        track_count: item
            .child_count
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
    }
}

fn artist_credits_from_pairs(pairs: Option<&[NameIdPair]>) -> Vec<ArtistCredit> {
    pairs
        .unwrap_or_default()
        .iter()
        .filter(|pair| !pair.id.trim().is_empty())
        .map(|pair| ArtistCredit {
            id: String::from(jellyfin_id("artist", &pair.id)),
            name: pair
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Unknown Artist")
                .to_string(),
            musicbrainz_artist_id: None,
        })
        .collect()
}

fn genre_credits_from_pairs(pairs: Option<&[NameIdPair]>) -> Vec<GenreCredit> {
    pairs
        .unwrap_or_default()
        .iter()
        .filter(|pair| !pair.id.trim().is_empty())
        .map(|pair| GenreCredit {
            id: String::from(jellyfin_id("genre", &pair.id)),
            name: pair
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("Unknown Genre")
                .to_string(),
        })
        .collect()
}

fn joined_credit_names(credits: &[ArtistCredit]) -> Option<String> {
    let names = credits
        .iter()
        .map(|credit| credit.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

fn joined_artist_names(artists: Option<&[String]>) -> Option<String> {
    let names = artists
        .unwrap_or_default()
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

fn source_id(ids: &Option<HashMap<String, String>>, key: &str) -> Option<String> {
    ids.as_ref()
        .and_then(|ids| ids.get(key))
        .filter(|value| !value.trim().is_empty())
        .cloned()
}

fn color_seed(id: &str) -> u32 {
    (stable_hash(id) & 0xffff_ffff) as u32
}

fn duration_seconds(ticks: Option<i64>) -> u32 {
    ticks
        .map(|value| (value.max(0) / 10_000_000) as u32)
        .unwrap_or(0)
}

fn favorite(user_data: &Option<UserData>) -> bool {
    user_data
        .as_ref()
        .and_then(|data| data.is_favorite)
        .unwrap_or(false)
}

fn play_count(user_data: &Option<UserData>) -> Option<u32> {
    user_data
        .as_ref()
        .and_then(|data| data.play_count)
        .map(|value| value.max(0) as u32)
}

fn user_rating(user_data: &Option<UserData>) -> Option<u8> {
    user_data
        .as_ref()
        .and_then(|data| data.rating)
        .filter(|rating| rating.is_finite() && (0.0..=10.0).contains(rating))
        .map(|rating| rating.round() as u8)
}

fn normalized_timestamp(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn primary_image_ref(
    kind: &str,
    item_id: &str,
    image_tags: &Option<HashMap<String, String>>,
) -> Option<ImageRef> {
    image_tags
        .as_ref()
        .and_then(|tags| tags.get("Primary"))
        .filter(|tag| !tag.is_empty())
        .map(|tag| ImageRef {
            item_id: jellyfin_id(kind, item_id),
            tag: Some(tag.clone()),
        })
}

fn album_image_ref(item: &JellyfinItem) -> Option<ImageRef> {
    let album_id = item.album_id.as_deref()?.trim();
    let tag = item.album_primary_image_tag.as_deref()?.trim();
    if album_id.is_empty() || tag.is_empty() {
        return None;
    }
    Some(ImageRef {
        item_id: jellyfin_id("album", album_id),
        tag: Some(tag.to_string()),
    })
}

fn backdrop_image_ref(item: &JellyfinItem) -> Option<ImageRef> {
    let item_tag = first_image_tag(item.backdrop_image_tags.as_deref());
    if let Some(tag) = item_tag {
        return Some(ImageRef {
            item_id: jellyfin_id("backdrop", &item.id),
            tag: Some(tag),
        });
    }

    let parent_id = item.parent_backdrop_item_id.as_deref()?.trim();
    let tag = first_image_tag(item.parent_backdrop_image_tags.as_deref())?;
    if parent_id.is_empty() {
        return None;
    }
    Some(ImageRef {
        item_id: jellyfin_id("backdrop", parent_id),
        tag: Some(tag),
    })
}

fn first_image_tag(tags: Option<&[String]>) -> Option<String> {
    tags.unwrap_or_default()
        .iter()
        .map(|tag| tag.trim())
        .find(|tag| !tag.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jellyfin_item_reads_normalization_gains() {
        let item = serde_json::from_value::<JellyfinItem>(serde_json::json!({
            "Id": "track-one",
            "NormalizationGain": -4.25,
            "AlbumNormalizationGain": -3.5
        }))
        .expect("Jellyfin item");

        assert_eq!(item.normalization_gain, Some(-4.25));
        assert_eq!(item.album_normalization_gain, Some(-3.5));
    }
}
