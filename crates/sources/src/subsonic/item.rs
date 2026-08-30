use super::*;
use crate::policy::normalized_date;

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
            .map(|value| (album.id.as_str(), value.as_str()))
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
        .album_id
        .is_none()
        .then_some(track.image_ref.as_ref())
        .flatten()
        .map(serde_json::to_vec)
        .transpose()?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"rufin-subsonic-audio-v1\0");
    hash.update(track.id.as_bytes());
    hash.update(track.source_path.as_deref().unwrap_or_default().as_bytes());
    hash.update(
        &track
            .source_format
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hash.update(&track.duration_seconds.to_le_bytes());
    let normalized = format!(
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
        &normalized,
        &track.album,
        &track.artist,
        &track.title.to_lowercase(),
        i64::from(track.duration_seconds) * 1000,
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
        *hash.finalize().as_bytes(),
    )
    .await?;
    if track.replay_gain_track_db.is_some() {
        scan.write_track_source_loudness(
            &track.id,
            None,
            None,
            track.replay_gain_track_db,
            track.replay_gain_track_peak,
        )
        .await?;
    }
    if let Some(album_id) = track.album_id.as_deref()
        && track.replay_gain_album_db.is_some()
    {
        scan.write_album_source_loudness(
            album_id,
            None,
            None,
            track.replay_gain_album_db,
            track.replay_gain_album_peak,
        )
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
    for mood in &track.relations.moods {
        scan.write_mood(
            &mood.id,
            &mood.name,
            &mood.name.to_lowercase(),
            &mood.name.to_lowercase(),
        )
        .await?;
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
        &track
            .relations
            .moods
            .iter()
            .map(|mood| (track.id.as_str(), mood.id.as_str()))
            .collect::<Vec<_>>(),
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

pub(super) fn image_ref(
    source: &SubsonicSource,
    cover_art: Option<SubsonicId>,
) -> Option<ImageRef> {
    cover_art.map(|id| ImageRef::new(source.id("cover", &id.0), None))
}
pub(super) fn folder_from_artist(source: &SubsonicSource, artist: SubsonicArtist) -> Folder {
    Folder {
        id: String::from(source.id("folder", artist.id.0.as_str())),
        name: artist.name.unwrap_or_else(|| "Untitled Folder".to_string()),
    }
}
pub(super) fn folder_from_child(source: &SubsonicSource, child: SubsonicSong) -> Folder {
    Folder {
        id: String::from(source.id("folder", child.id.0.as_str())),
        name: child.title.unwrap_or_else(|| "Untitled Folder".to_string()),
    }
}
pub(super) fn genres_from_item(genre: Option<String>, genres: Vec<GenreName>) -> Vec<String> {
    let mut values = Vec::new();
    if let Some(genre) = genre.filter(|genre| !genre.trim().is_empty()) {
        values.push(genre);
    }
    for genre in genres {
        if !genre.name.trim().is_empty() && !values.iter().any(|value| value == &genre.name) {
            values.push(genre.name);
        }
    }
    values
}
fn genre_credits_from_item(
    source: &SubsonicSource,
    genre: Option<String>,
    genres: Vec<GenreName>,
) -> Vec<GenreCredit> {
    genres_from_item(genre, genres)
        .into_iter()
        .map(|name| GenreCredit {
            id: String::from(source.id("genre", &name)),
            name,
        })
        .collect()
}
pub(super) fn moods_from_item(source: &SubsonicSource, moods: Vec<String>) -> Vec<MoodCredit> {
    let mut values = Vec::new();
    for mood in moods {
        let mood = mood.trim();
        if !mood.is_empty()
            && !values
                .iter()
                .any(|value: &MoodCredit| value.name.eq_ignore_ascii_case(mood))
        {
            values.push(MoodCredit {
                id: String::from(source.id("mood", mood)),
                name: mood.to_string(),
            });
        }
    }
    values
}
fn artist_credit(
    source: &SubsonicSource,
    id: Option<&SubsonicId>,
    name: &str,
) -> Option<ArtistCredit> {
    let id = id?;
    (!id.0.trim().is_empty()).then(|| ArtistCredit {
        id: String::from(source.id("artist", &id.0)),
        name: name.to_string(),
        musicbrainz_artist_id: None,
    })
}

fn artist_credits_from_refs(
    source: &SubsonicSource,
    artists: Vec<SubsonicArtistRef>,
) -> Vec<ArtistCredit> {
    artists
        .into_iter()
        .filter(|artist| !artist.id.0.trim().is_empty())
        .map(|artist| ArtistCredit {
            id: String::from(source.id("artist", &artist.id.0)),
            name: artist.name,
            musicbrainz_artist_id: None,
        })
        .collect()
}

pub(super) fn joined_artist_names(artists: &[ArtistCredit]) -> Option<String> {
    let names = artists
        .iter()
        .map(|artist| artist.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}

fn structured_release_date(date: Option<SubsonicItemDate>) -> Option<String> {
    let date = date?;
    let year = u16::try_from(date.year).ok().filter(|year| *year > 0)?;
    match (
        u8::try_from(date.month)
            .ok()
            .filter(|month| (1..=12).contains(month)),
        u8::try_from(date.day)
            .ok()
            .filter(|day| (1..=31).contains(day)),
    ) {
        (Some(month), Some(day)) => Some(format!("{year:04}-{month:02}-{day:02}")),
        (Some(month), None) => Some(format!("{year:04}-{month:02}")),
        _ => Some(format!("{year:04}")),
    }
}
fn bpm_from_u32(value: u32) -> Option<u16> {
    if value == 0 || value > u32::from(u16::MAX) {
        None
    } else {
        Some(value as u16)
    }
}
fn clean_optional(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}
pub(super) fn album_from_dto(source: &SubsonicSource, album: SubsonicAlbum) -> Album {
    let raw_id = raw_id_string(&album.id);
    let structured_artists = artist_credits_from_refs(source, album.artists);
    let artist = clean_optional(album.display_artist)
        .or_else(|| clean_optional(album.artist.clone()))
        .or_else(|| joined_artist_names(&structured_artists))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album_artists = if structured_artists.is_empty() {
        artist_credit(source, album.artist_id.as_ref(), &artist)
            .into_iter()
            .collect()
    } else {
        structured_artists
    };
    let genres = genre_credits_from_item(source, album.genre, album.genres);
    let release_date = structured_release_date(album.release_date);
    let year = {
        let scalar = u16_from_option(album.year);
        if scalar > 0 {
            scalar
        } else {
            release_date
                .as_deref()
                .and_then(|date| date.get(..4))
                .and_then(|year| year.parse().ok())
                .unwrap_or_default()
        }
    };
    Album {
        id: String::from(source.id("album", &raw_id)),
        title: album
            .title
            .or(album.name)
            .or(album.album)
            .unwrap_or_else(|| "Untitled Album".to_string()),
        artist,
        year,
        release_date,
        date_added: normalized_date(album.created),
        last_played: normalized_timestamp(album.played),
        play_count: album
            .play_count
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        user_rating: album
            .user_rating
            .filter(|value| *value > 0)
            .map(|value| value.min(5).saturating_mul(2) as u8),
        favorite: favorite(&album.starred),
        color_seed: color_seed(&raw_id),
        image_ref: image_ref(source, album.cover_art),
        local_artwork: None,
        release_types: normalize_release_types(album.release_types),
        is_compilation: album.is_compilation,
        musicbrainz_album_id: clean_optional(album.musicbrainz_album_id),
        musicbrainz_release_group_id: None,
        relations: AlbumRelations {
            album_artists,
            artists: Vec::new(),
            genres,
        },
    }
}
pub(super) fn track_from_dto(source: &SubsonicSource, song: SubsonicSong) -> Track {
    let replay_gain = song.replay_gain.unwrap_or_default();
    let raw_id = raw_id_string(&song.id);
    let album_id = song
        .album_id
        .as_ref()
        .map(raw_id_string)
        .map(|id| String::from(source.id("album", &id)));
    let structured_artists = artist_credits_from_refs(source, song.artists);
    let album_artist_credits = artist_credits_from_refs(source, song.album_artists);
    let artist = clean_optional(song.display_artist)
        .or_else(|| clean_optional(song.artist.clone()))
        .or_else(|| joined_artist_names(&structured_artists))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let artist_credits = if structured_artists.is_empty() {
        artist_credit(source, song.artist_id.as_ref(), &artist)
            .into_iter()
            .collect()
    } else {
        structured_artists
    };
    let genres = genre_credits_from_item(source, song.genre, song.genres);
    let moods = moods_from_item(source, song.moods);
    let source_format = source_format_from_song(
        song.suffix.as_deref(),
        song.content_type.as_deref(),
        song.path.as_deref(),
    );
    Track {
        id: String::from(source.id("track", &raw_id)),
        album_id,
        title: song.title.unwrap_or_else(|| "Untitled Track".to_string()),
        artist,
        album: song.album.unwrap_or_else(|| "Unknown Album".to_string()),
        year: u16_from_option(song.year),
        release_date: None,
        date_added: normalized_date(song.created),
        last_played: crate::policy::unix_seconds(song.played),
        play_count: song
            .play_count
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        user_rating: song
            .user_rating
            .filter(|value| *value > 0)
            .map(|value| value.min(5).saturating_mul(2) as u8),
        duration_seconds: song.duration.unwrap_or_default(),
        favorite: favorite(&song.starred),
        disc_number: u16_from_option(song.disc_number),
        track_number: u16_from_option(song.track),
        image_ref: image_ref(source, song.cover_art),
        local_artwork: None,
        musicbrainz_recording_id: clean_optional(song.musicbrainz_recording_id),
        musicbrainz_release_track_id: None,
        source_path: song.path,
        cue: None,
        source_format,
        comment: song.comment.filter(|value| !value.trim().is_empty()),
        skip_count: None,
        bpm: song.bpm.and_then(bpm_from_u32),
        replay_gain_track_db: finite(replay_gain.track_gain),
        replay_gain_track_peak: positive_finite(replay_gain.track_peak),
        replay_gain_album_db: finite(replay_gain.album_gain),
        replay_gain_album_peak: positive_finite(replay_gain.album_peak),
        relations: TrackRelations {
            artists: artist_credits,
            album_artists: album_artist_credits,
            genres,
            moods,
            music_folders: Vec::new(),
        },
    }
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}

fn positive_finite(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

pub(super) fn source_format_from_song(
    suffix: Option<&str>,
    content_type: Option<&str>,
    path: Option<&str>,
) -> Option<String> {
    suffix
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            content_type
                .and_then(|value| value.rsplit('/').next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .or_else(|| {
            let raw_path = path?;
            let path = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
            std::path::Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
}
pub(super) fn artist_from_dto(source: &SubsonicSource, artist: SubsonicArtist) -> Artist {
    let raw_id = raw_id_string(&artist.id);
    Artist {
        id: String::from(source.id("artist", &raw_id)),
        name: artist.name.unwrap_or_else(|| "Unknown Artist".to_string()),
        favorite: favorite(&artist.starred),
        last_played: normalized_timestamp(artist.played),
        play_count: artist
            .play_count
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        user_rating: artist
            .user_rating
            .filter(|value| *value > 0)
            .map(|value| value.min(5).saturating_mul(2) as u8),
        musicbrainz_artist_id: clean_optional(artist.musicbrainz_artist_id),
        image_ref: image_ref(source, artist.cover_art),
        local_artwork: None,
    }
}
pub(super) fn genre_from_dto(source: &SubsonicSource, genre: SubsonicGenre) -> Genre {
    Genre {
        id: String::from(source.id("genre", &genre.value)),
        name: genre.value,
        image_ref: None,
        local_artwork: None,
    }
}
fn normalized_timestamp(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn playlist_from_dto(source: &SubsonicSource, playlist: SubsonicPlaylist) -> Playlist {
    let raw_id = raw_id_string(&playlist.id);
    let track_count = playlist.entry.as_ref().map_or(0, Vec::len);
    Playlist {
        id: String::from(source.id("playlist", &raw_id)),
        name: playlist
            .name
            .unwrap_or_else(|| "Untitled Playlist".to_string()),
        image_ref: image_ref(source, playlist.cover_art),
        duration_seconds: 0,
        track_count,
    }
}
