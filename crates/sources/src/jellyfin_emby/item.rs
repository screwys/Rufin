use std::path::Path;

pub(super) use crate::NativeImageRef as ImageRef;
use crate::remote_json::{boolean, field, id, items, strings};
use serde_json::Value;

use crate::policy::{normalized_date, u16_from_option};

use super::{ServerKind, stable_hash};

pub(super) async fn stage_album(
    scan: &mut library::Scan,
    album: Album,
) -> library::LibraryResult<()> {
    let artwork = album
        .image_ref
        .as_ref()
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
        .transpose()?;
    scan.write_album(
        &album.id,
        &album.title,
        &album.title.to_lowercase(),
        &album.artist,
        &album
            .sort_name
            .as_deref()
            .unwrap_or(&album.title)
            .to_lowercase(),
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
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
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
        &track
            .sort_name
            .as_deref()
            .unwrap_or(&track.title)
            .to_lowercase(),
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
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
        .transpose()?;
    scan.write_artist(
        &artist.id,
        &artist.name,
        &artist.name.to_lowercase(),
        Some(
            &artist
                .sort_name
                .as_deref()
                .unwrap_or(&artist.name)
                .to_lowercase(),
        ),
        artist.musicbrainz_artist_id.as_deref(),
        artwork.as_deref(),
        Some(artist.favorite),
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
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
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
        None,
        artist.musicbrainz_artist_id.as_deref(),
        None,
        None,
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
    pub sort_name: Option<String>,
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
    pub sort_name: Option<String>,
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
    pub sort_name: Option<String>,
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

pub(super) const ALBUM_FIELDS: &str = "SortName,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,ChildCount";
pub(super) const TRACK_FIELDS: &str = "SortName,Path,Overview,Container,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,AlbumId,AlbumPrimaryImageTag,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,NormalizationGain,AlbumNormalizationGain";
pub(super) const PLAYLIST_FIELDS: &str = "RunTimeTicks,ImageTags,ChildCount";
pub(super) const MIXED_ITEM_FIELDS: &str = "SortName,Path,Overview,Container,Genres,DateCreated,PremiereDate,ProductionYear,RunTimeTicks,ParentId,AlbumId,AlbumPrimaryImageTag,AlbumArtists,ArtistItems,ProviderIds,UserData,ImageTags,BackdropImageTags,ParentBackdropItemId,ParentBackdropImageTags,ChildCount,AlbumCount,SongCount,NormalizationGain,AlbumNormalizationGain";

pub(super) fn album_from_item(server: ServerKind, item: Value) -> Option<Album> {
    let item_id = id(&item["Id"])?;

    let image_ref = primary_image_ref(server, "album", &item_id, &item["ImageTags"])
        .or_else(|| alternate_primary_image_ref(server, &item))
        .or_else(|| backdrop_image_ref(server, &item));
    let album_artist_credits = artist_credits_from_pairs(server, &item["AlbumArtists"]);
    let artist_credits = artist_credits_from_pairs(server, &item["ArtistItems"]);
    let genres = genre_credits_from_pairs(server, &item["GenreItems"]);
    let artist = field::<String>(&item, "AlbumArtist")
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| joined_credit_names(&album_artist_credits))
        .or_else(|| joined_artist_names(Some(&strings(&item["Artists"]))))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    Some(Album {
        id: String::from(server.object_id("album", &item_id)),
        title: field(&item, "Name").unwrap_or_else(|| "Untitled Album".to_string()),
        sort_name: sort_name(&item),
        artist,
        year: u16_from_option(field(&item, "ProductionYear")),
        release_date: normalized_date(field(&item, "PremiereDate")),
        date_added: normalized_date(field(&item, "DateCreated")),
        last_played: normalized_timestamp(field(&item["UserData"], "LastPlayedDate")),
        play_count: play_count(&item["UserData"]),
        user_rating: user_rating(&item["UserData"]),
        favorite: favorite(&item["UserData"]),
        color_seed: color_seed(&item_id),
        image_ref,
        local_artwork: None,
        release_types: Vec::new(),
        is_compilation: None,
        musicbrainz_album_id: source_id(&item["ProviderIds"], "MusicBrainzAlbum"),
        musicbrainz_release_group_id: source_id(&item["ProviderIds"], "MusicBrainzReleaseGroup"),
        relations: AlbumRelations {
            album_artists: album_artist_credits,
            artists: artist_credits,
            genres,
        },
    })
}

pub(super) fn track_from_item(server: ServerKind, item: Value) -> Option<Track> {
    let item_id = id(&item["Id"])?;
    let image_ref = album_image_ref(server, &item)
        .or_else(|| primary_image_ref(server, "track", &item_id, &item["ImageTags"]))
        .or_else(|| alternate_primary_image_ref(server, &item))
        .or_else(|| backdrop_image_ref(server, &item));
    let artist_credits = artist_credits_from_pairs(server, &item["ArtistItems"]);
    let album_artist_credits = artist_credits_from_pairs(server, &item["AlbumArtists"]);
    let genres = genre_credits_from_pairs(server, &item["GenreItems"]);
    let album_id = id(&item["AlbumId"])
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| String::from(server.object_id("album", id)));
    let source_format = source_format_from_item(
        field::<String>(&item, "Container").as_deref(),
        field::<String>(&item, "Path").as_deref(),
    );
    Some(Track {
        id: String::from(server.object_id("track", &item_id)),
        album_id,
        title: field(&item, "Name").unwrap_or_else(|| "Untitled Track".to_string()),
        sort_name: sort_name(&item),
        artist: joined_artist_names(Some(&strings(&item["Artists"])))
            .or_else(|| joined_credit_names(&artist_credits))
            .unwrap_or_else(|| {
                field(&item, "AlbumArtist").unwrap_or_else(|| "Unknown Artist".to_string())
            }),
        album: field(&item, "Album").unwrap_or_else(|| "Unknown Album".to_string()),
        album_artwork: None,
        year: u16_from_option(field(&item, "ProductionYear")),
        release_date: normalized_date(field(&item, "PremiereDate")),
        date_added: normalized_date(field(&item, "DateCreated")),
        last_played: crate::policy::unix_seconds(field(&item["UserData"], "LastPlayedDate")),
        play_count: play_count(&item["UserData"]),
        user_rating: user_rating(&item["UserData"]),
        duration_seconds: duration_seconds(field(&item, "RunTimeTicks")),
        favorite: favorite(&item["UserData"]),
        disc_number: u16_from_option(field(&item, "ParentIndexNumber")),
        track_number: u16_from_option(field(&item, "IndexNumber")),
        image_ref,
        local_artwork: None,
        musicbrainz_recording_id: source_id(&item["ProviderIds"], "MusicBrainzRecording"),
        musicbrainz_release_track_id: source_id(&item["ProviderIds"], "MusicBrainzTrack"),
        source_path: field::<String>(&item, "Path"),
        cue: None,
        source_format,
        comment: field::<String>(&item, "Overview").filter(|value| !value.trim().is_empty()),
        skip_count: None,
        bpm: None,
        replay_gain_track_db: field::<f64>(&item, "NormalizationGain")
            .filter(|value| value.is_finite()),
        replay_gain_album_db: field::<f64>(&item, "AlbumNormalizationGain")
            .filter(|value| value.is_finite()),
        relations: TrackRelations {
            artists: artist_credits,
            album_artists: album_artist_credits,
            genres,
            moods: Vec::new(),
            music_folders: Vec::new(),
        },
    })
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

pub(super) fn is_audio_item(item: &Value) -> bool {
    item["Type"]
        .as_str()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("Audio"))
}

pub(super) fn artist_from_item(server: ServerKind, item: Value) -> Option<Artist> {
    let item_id = id(&item["Id"])?;
    Some(Artist {
        id: String::from(server.object_id("artist", &item_id)),
        name: field(&item, "Name").unwrap_or_else(|| "Unknown Artist".to_string()),
        sort_name: sort_name(&item),
        favorite: favorite(&item["UserData"]),
        last_played: normalized_timestamp(field(&item["UserData"], "LastPlayedDate")),
        play_count: play_count(&item["UserData"]),
        user_rating: user_rating(&item["UserData"]),
        musicbrainz_artist_id: source_id(&item["ProviderIds"], "MusicBrainzArtist"),
        image_ref: primary_image_ref(server, "artist", &item_id, &item["ImageTags"]),
        local_artwork: None,
    })
}

pub(super) fn genre_from_item(server: ServerKind, item: Value) -> Option<Genre> {
    let item_id = id(&item["Id"])?;
    Some(Genre {
        id: String::from(server.object_id("genre", &item_id)),
        name: field(&item, "Name").unwrap_or_else(|| "Unknown Genre".to_string()),
        image_ref: primary_image_ref(server, "genre", &item_id, &item["ImageTags"]),
        local_artwork: None,
    })
}

pub(super) fn playlist_from_item(server: ServerKind, item: Value) -> Option<Playlist> {
    let item_id = id(&item["Id"])?;
    Some(Playlist {
        id: String::from(server.object_id("playlist", &item_id)),
        name: field(&item, "Name").unwrap_or_else(|| "Untitled Playlist".to_string()),
        image_ref: primary_image_ref(server, "playlist", &item_id, &item["ImageTags"]),
        duration_seconds: duration_seconds(field(&item, "RunTimeTicks")),
        track_count: field::<i32>(&item, "ChildCount")
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
    })
}

fn artist_credits_from_pairs(server: ServerKind, pairs: &Value) -> Vec<ArtistCredit> {
    items(pairs)
        .iter()
        .filter_map(|pair| {
            Some(ArtistCredit {
                id: server.object_id("artist", &id(&pair["Id"])?),
                name: field::<String>(pair, "Name")
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Unknown Artist".into()),
                musicbrainz_artist_id: None,
            })
        })
        .collect()
}
fn genre_credits_from_pairs(server: ServerKind, pairs: &Value) -> Vec<GenreCredit> {
    items(pairs)
        .iter()
        .filter_map(|pair| {
            Some(GenreCredit {
                id: server.object_id("genre", &id(&pair["Id"])?),
                name: field::<String>(pair, "Name")
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| "Unknown Genre".into()),
            })
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

fn source_id(ids: &Value, key: &str) -> Option<String> {
    field::<String>(ids, key).filter(|value| !value.trim().is_empty())
}

fn color_seed(id: &str) -> u32 {
    (stable_hash(id) & 0xffff_ffff) as u32
}

fn duration_seconds(ticks: Option<i64>) -> u32 {
    ticks
        .map(|value| (value.max(0) / 10_000_000) as u32)
        .unwrap_or(0)
}

fn favorite(data: &Value) -> bool {
    boolean(&data["IsFavorite"]).unwrap_or(false)
}
fn play_count(data: &Value) -> Option<u32> {
    field::<i32>(data, "PlayCount").map(|v| v.max(0) as u32)
}
fn user_rating(data: &Value) -> Option<u8> {
    field::<f64>(data, "Rating")
        .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
        .map(|v| v.round() as u8)
}

fn normalized_timestamp(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn primary_image_ref(
    server: ServerKind,
    kind: &str,
    item_id: &str,
    tags: &Value,
) -> Option<ImageRef> {
    let tag = field::<String>(tags, "Primary").filter(|tag| !tag.is_empty())?;
    Some(ImageRef {
        item_id: server.object_id(kind, item_id),
        tag: Some(tag),
    })
}
fn alternate_primary_image_ref(server: ServerKind, item: &Value) -> Option<ImageRef> {
    let tag = field::<String>(item, "PrimaryImageTag").filter(|tag| !tag.trim().is_empty())?;
    let owner = id(&item["PrimaryImageItemId"]).or_else(|| id(&item["Id"]))?;
    Some(ImageRef {
        item_id: server.object_id("track", &owner),
        tag: Some(tag),
    })
}

fn sort_name(item: &Value) -> Option<String> {
    field::<String>(item, "SortName")
        .filter(|name| !name.trim().is_empty())
        .or_else(|| field::<String>(item, "ForcedSortName").filter(|name| !name.trim().is_empty()))
}
fn album_image_ref(server: ServerKind, item: &Value) -> Option<ImageRef> {
    let album_id = id(&item["AlbumId"])?;
    let tag = field::<String>(item, "AlbumPrimaryImageTag").filter(|tag| !tag.trim().is_empty())?;
    Some(ImageRef {
        item_id: server.object_id("album", &album_id),
        tag: Some(tag.trim().to_string()),
    })
}
fn backdrop_image_ref(server: ServerKind, item: &Value) -> Option<ImageRef> {
    let (item_id, tags) = if first_image_tag(&item["BackdropImageTags"]).is_some() {
        (id(&item["Id"])?, &item["BackdropImageTags"])
    } else {
        (
            id(&item["ParentBackdropItemId"])?,
            &item["ParentBackdropImageTags"],
        )
    };
    Some(ImageRef {
        item_id: server.object_id("backdrop", &item_id),
        tag: first_image_tag(tags),
    })
}
fn first_image_tag(tags: &Value) -> Option<String> {
    strings(tags)
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .find(|tag| !tag.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn emby_import_preserves_distinct_alternate_album_covers() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut scan = library::Scan::begin(&database, "emby", "Emby", "emby", None)
            .await
            .unwrap();
        let albums: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/alternate-album-covers.json")).unwrap();
        for album in &albums {
            let mut mapped = album_from_item(ServerKind::Emby, album.clone()).unwrap();
            mapped.image_ref = Some(ImageRef::new(
                "emby:backdrop:3259",
                Some("c9c414f53f4439cf148b7d4bca58e300".into()),
            ));
            stage_album(&mut scan, mapped).await.unwrap();
        }
        scan.finish().await.unwrap();
        let mut scan = library::Scan::begin_items(&database, "emby").await.unwrap();
        for album in &albums {
            stage_album(
                &mut scan,
                album_from_item(ServerKind::Emby, album.clone()).unwrap(),
            )
            .await
            .unwrap();
        }
        scan.finish().await.unwrap();
        for album in albums {
            let object = format!("emby:album:{}", album["Id"].as_str().unwrap());
            let uri = library::source_entity_uri(&crate::SourceId::new("emby"), "album", &object);
            let row = database
                .album_row_by_media_uri(&uri, &library::ReadCancellation::new())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.title, album["Name"].as_str().unwrap());
            let binding: crate::NativeArtworkBinding =
                serde_json::from_slice(row.artwork_binding.as_deref().unwrap()).unwrap();
            assert_eq!(
                binding.image.item_id,
                format!(
                    "emby:track:{}",
                    album["PrimaryImageItemId"].as_str().unwrap()
                )
            );
            assert_eq!(
                binding.image.tag.as_deref(),
                album["PrimaryImageTag"].as_str()
            );
        }
    }

    #[test]
    fn effective_sort_names_and_existing_image_precedence_are_preserved() {
        for server in [ServerKind::Jellyfin, ServerKind::Emby] {
            let item = json!({"Id":"1", "Name":"The Name", "SortName":"effective override", "ForcedSortName":"Raw override"});
            assert_eq!(
                album_from_item(server, item.clone())
                    .unwrap()
                    .sort_name
                    .as_deref(),
                Some("effective override")
            );
            assert_eq!(
                artist_from_item(server, item.clone())
                    .unwrap()
                    .sort_name
                    .as_deref(),
                Some("effective override")
            );
            assert_eq!(
                track_from_item(server, item).unwrap().sort_name.as_deref(),
                Some("effective override")
            );
            let forced = album_from_item(
                server,
                json!({"Id":"1", "Name":"Name", "ForcedSortName":"Override"}),
            )
            .unwrap();
            assert_eq!(forced.sort_name.as_deref(), Some("Override"));
            let plain = album_from_item(
                server,
                json!({"Id":"1", "Name":"Name", "SortName":"", "ForcedSortName":null}),
            )
            .unwrap();
            assert_eq!(plain.sort_name, None);
            assert_eq!(plain.title, "Name");
            let item = json!({"Id":"1", "ImageTags":{"Primary":"own"}, "PrimaryImageItemId":"2", "PrimaryImageTag":"alternate", "AlbumId":"3", "AlbumPrimaryImageTag":"album"});
            assert_eq!(
                album_from_item(server, item.clone())
                    .unwrap()
                    .image_ref
                    .unwrap()
                    .item_id,
                server.object_id("album", "1")
            );
            assert_eq!(
                track_from_item(server, item)
                    .unwrap()
                    .image_ref
                    .unwrap()
                    .item_id,
                server.object_id("album", "3")
            );
        }
    }
}
