use super::*;
use crate::policy::normalized_date;
use crate::remote_json as json;
use serde_json::Value;

pub(super) async fn stage_album(
    scan: &mut library::Scan,
    album: Album,
) -> library::LibraryResult<()> {
    let artwork = album
        .image_ref
        .as_ref()
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
        .transpose()?;
    let sort_text = album
        .sort_name
        .as_deref()
        .unwrap_or(&album.title)
        .to_lowercase();
    let input = library::AlbumInput {
        object_id: album.id,
        title: album.title,
        display_artist: album.artist,
        sort_text,
        year: Some(i64::from(album.year)).filter(|year| *year > 0),
        release_date: album.release_date,
        date_added: album.date_added,
        musicbrainz_release_id: album.musicbrainz_album_id,
        musicbrainz_release_group_id: album.musicbrainz_release_group_id,
        is_compilation: album.is_compilation,
        artwork_binding: artwork,
        favorite: album.favorite,
        rating: album.user_rating.map(i64::from),
        ..Default::default()
    };
    scan.write_album(&input).await?;
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
            .map(|artist| (input.object_id.as_str(), artist.id.as_str()))
            .collect::<Vec<_>>(),
        &album
            .relations
            .genres
            .iter()
            .map(|genre| (input.object_id.as_str(), genre.id.as_str()))
            .collect::<Vec<_>>(),
        &album
            .release_types
            .iter()
            .map(|value| (input.object_id.as_str(), value.as_str()))
            .collect::<Vec<_>>(),
    )
    .await?;
    Ok(())
}

pub(super) async fn stage_track(
    scan: &mut library::Scan,
    track: Track,
) -> library::LibraryResult<bool> {
    let artwork = track
        .image_ref
        .as_ref()
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
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

    let sort_text = track
        .sort_name
        .as_deref()
        .unwrap_or(&track.title)
        .to_lowercase();
    let input = library::TrackInput {
        object_id: track.id,
        album_object_id: track.album_id,
        title: track.title,
        display_album: track.album,
        display_artist: track.artist,
        sort_text,
        duration_millis: i64::from(track.duration_seconds) * 1000,
        disc_number: i64::from(track.disc_number),
        track_number: i64::from(track.track_number),
        year: Some(i64::from(track.year)).filter(|year| *year > 0),
        release_date: track.release_date,
        date_added: track.date_added,
        source_format: track.source_format,
        comment: track.comment,
        bpm: track.bpm.map(i64::from),
        musicbrainz_recording_id: track.musicbrainz_recording_id,
        musicbrainz_release_track_id: track.musicbrainz_release_track_id,
        artwork_binding: artwork,
        favorite: track.favorite,
        rating: track.user_rating.map(i64::from),
        source_play_count: track.play_count.map(i64::from),
        source_skip_count: track.skip_count.map(i64::from),
        source_last_played: track.last_played,
        source_path: track.source_path,
        loudness_analysis_key: *hash.finalize().as_bytes(),
        audio_properties: track.audio_properties,
        ..Default::default()
    };
    let inserted = scan.write_track(&input).await?;
    if track.replay_gain_track_db.is_some() {
        scan.write_track_source_loudness(
            &input.object_id,
            None,
            None,
            track.replay_gain_track_db,
            track.replay_gain_track_peak,
        )
        .await?;
    }
    if let Some(album_id) = input.album_object_id.as_deref()
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
            .map(|artist| (input.object_id.as_str(), artist.id.as_str()))
            .collect::<Vec<_>>(),
        &track
            .relations
            .genres
            .iter()
            .map(|genre| (input.object_id.as_str(), genre.id.as_str()))
            .collect::<Vec<_>>(),
        &track
            .relations
            .moods
            .iter()
            .map(|mood| (input.object_id.as_str(), mood.id.as_str()))
            .collect::<Vec<_>>(),
    )
    .await?;
    if let Some(folders) = &track.relations.music_folders {
        scan.replace_track_folders(
            &input.object_id,
            &folders
                .iter()
                .map(|folder| library::ScanLink::new(&input.object_id, folder, 0))
                .collect::<Vec<_>>(),
        )
        .await?;
    }
    Ok(inserted)
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
    let sort_text = artist
        .sort_name
        .as_deref()
        .unwrap_or(&artist.name)
        .to_lowercase();
    scan.write_artist(&library::ArtistInput {
        object_id: artist.id,
        name: artist.name,
        sort_text: Some(sort_text),
        musicbrainz_artist_id: artist.musicbrainz_artist_id,
        artwork_binding: artwork,
        favorite: artist.favorite,
        rating: artist.user_rating.map(i64::from),
    })
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
    let sort_text = genre.name.to_lowercase();
    scan.write_genre(&library::GenreInput {
        object_id: genre.id,
        name: genre.name,
        sort_text: Some(sort_text),
        artwork_binding: artwork,
    })
    .await
}
async fn stage_artist_credit(
    scan: &mut library::Scan,
    artist: &ArtistCredit,
) -> library::LibraryResult<()> {
    scan.write_artist(&library::ArtistInput {
        object_id: artist.id.clone(),
        name: artist.name.clone(),
        sort_text: artist.sort_name.as_ref().map(|name| name.to_lowercase()),
        musicbrainz_artist_id: artist.musicbrainz_artist_id.clone(),
        ..Default::default()
    })
    .await
}
async fn stage_genre_credit(
    scan: &mut library::Scan,
    genre: &GenreCredit,
) -> library::LibraryResult<()> {
    scan.write_genre(&library::GenreInput {
        object_id: genre.id.clone(),
        name: genre.name.clone(),
        ..Default::default()
    })
    .await
}

pub(super) fn image_ref(source: &SubsonicSource, cover_art: Option<String>) -> Option<ImageRef> {
    cover_art.map(|id| ImageRef::new(source.id("cover", &id), None))
}

fn genre_credits_from_item(source: &SubsonicSource, item: &Value) -> Vec<GenreCredit> {
    let mut names = Vec::new();
    if let Some(genre) = json::field::<String>(item, "genre").filter(|name| !name.trim().is_empty())
    {
        names.push(genre);
    }
    for genre in json::items(&item["genres"]) {
        if let Some(name) = json::field::<String>(genre, "name")
            .filter(|name| !name.trim().is_empty() && !names.contains(name))
        {
            names.push(name);
        }
    }
    names
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

fn artist_credits_from_item(source: &SubsonicSource, artists: &Value) -> Vec<ArtistCredit> {
    json::items(artists)
        .iter()
        .filter_map(|artist| {
            Some(ArtistCredit {
                id: String::from(source.id("artist", &json::id(&artist["id"])?)),
                name: json::field(artist, "name").unwrap_or_default(),
                sort_name: clean_optional(json::field(artist, "sortName")),
                musicbrainz_artist_id: None,
            })
        })
        .collect()
}

fn scalar_artist_credit(source: &SubsonicSource, item: &Value, name: &str) -> Vec<ArtistCredit> {
    json::id(&item["artistId"])
        .map(|id| ArtistCredit {
            id: String::from(source.id("artist", &id)),
            name: name.to_string(),
            sort_name: None,
            musicbrainz_artist_id: None,
        })
        .into_iter()
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

fn structured_release_date(date: &Value) -> Option<String> {
    let year = json::field::<u16>(date, "year").filter(|year| *year > 0)?;
    match (
        json::field::<u8>(date, "month").filter(|month| (1..=12).contains(month)),
        json::field::<u8>(date, "day").filter(|day| (1..=31).contains(day)),
    ) {
        (Some(month), Some(day)) => Some(format!("{year:04}-{month:02}-{day:02}")),
        (Some(month), None) => Some(format!("{year:04}-{month:02}")),
        _ => Some(format!("{year:04}")),
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

pub(super) fn album_from_json(source: &SubsonicSource, album: &Value) -> Option<Album> {
    let raw_id = json::id(&album["id"])?;
    let structured_artists = artist_credits_from_item(source, &album["artists"]);
    let artist = clean_optional(json::field(album, "displayArtist"))
        .or_else(|| clean_optional(json::field(album, "artist")))
        .or_else(|| joined_artist_names(&structured_artists))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album_artists = if structured_artists.is_empty() {
        scalar_artist_credit(source, album, &artist)
    } else {
        structured_artists
    };
    let release_date = structured_release_date(&album["releaseDate"])
        .or_else(|| structured_release_date(&album["originalReleaseDate"]));
    let year = {
        let scalar = u16_from_option(json::field(album, "year"));
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
    Some(Album {
        sort_name: clean_optional(json::field(album, "sortName")),
        id: String::from(source.id("album", &raw_id)),
        title: json::field(album, "title")
            .or_else(|| json::field(album, "name"))
            .or_else(|| json::field(album, "album"))
            .unwrap_or_else(|| "Untitled Album".to_string()),
        artist,
        year,
        release_date,
        date_added: normalized_date(json::field(album, "created")),
        last_played: normalized_timestamp(json::field(album, "played")),
        play_count: json::field::<u64>(album, "playCount")
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        user_rating: json::field::<u32>(album, "userRating")
            .filter(|value| *value > 0)
            .map(|value| value.min(5).saturating_mul(2) as u8),
        favorite: favorite(&json::field(album, "starred")),
        color_seed: color_seed(&raw_id),
        image_ref: image_ref(source, json::id(&album["coverArt"])),
        local_artwork: None,
        release_types: normalize_release_types(json::strings(&album["releaseTypes"])),
        is_compilation: json::boolean(&album["isCompilation"]),
        musicbrainz_album_id: clean_optional(json::field(album, "musicBrainzId")),
        musicbrainz_release_group_id: None,
        relations: AlbumRelations {
            album_artists,
            artists: Vec::new(),
            genres: genre_credits_from_item(source, album),
        },
    })
}

pub(super) fn track_from_json(source: &SubsonicSource, song: &Value) -> Option<Track> {
    let raw_id = json::id(&song["id"])?;
    let structured_artists = artist_credits_from_item(source, &song["artists"]);
    let artist = clean_optional(json::field(song, "displayArtist"))
        .or_else(|| clean_optional(json::field(song, "artist")))
        .or_else(|| joined_artist_names(&structured_artists))
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let artist_credits = if structured_artists.is_empty() {
        scalar_artist_credit(source, song, &artist)
    } else {
        structured_artists
    };
    let suffix = json::field::<String>(song, "suffix");
    let content_type = json::field::<String>(song, "contentType");
    let path = json::field::<String>(song, "path");
    let source_format =
        source_format_from_song(suffix.as_deref(), content_type.as_deref(), path.as_deref());
    let replay_gain = &song["replayGain"];
    Some(Track {
        audio_properties: library::AudioProperties {
            bitrate_kbps: json::field(song, "bitRate"),
            sample_rate_hz: json::field(song, "samplingRate"),
            bit_depth: json::field(song, "bitDepth"),
            channels: json::field(song, "channelCount"),
        },
        sort_name: clean_optional(json::field(song, "sortName")),
        id: String::from(source.id("track", &raw_id)),
        album_id: json::id(&song["albumId"]).map(|id| String::from(source.id("album", &id))),
        title: json::field(song, "title").unwrap_or_else(|| "Untitled Track".to_string()),
        artist,
        album: json::field(song, "album").unwrap_or_else(|| "Unknown Album".to_string()),
        year: u16_from_option(json::field(song, "year")),
        release_date: None,
        date_added: normalized_date(json::field(song, "created")),
        last_played: crate::policy::unix_seconds(json::field(song, "played")),
        play_count: json::field::<u64>(song, "playCount")
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        user_rating: json::field::<u32>(song, "userRating")
            .filter(|value| *value > 0)
            .map(|value| value.min(5).saturating_mul(2) as u8),
        duration_seconds: json::field(song, "duration").unwrap_or_default(),
        favorite: favorite(&json::field(song, "starred")),
        disc_number: u16_from_option(json::field(song, "discNumber")),
        track_number: u16_from_option(json::field(song, "track")),
        image_ref: image_ref(source, json::id(&song["coverArt"])),
        local_artwork: None,
        musicbrainz_recording_id: clean_optional(json::field(song, "musicBrainzId")),
        musicbrainz_release_track_id: None,
        source_path: path,
        cue: None,
        source_format,
        comment: clean_optional(json::field(song, "comment")),
        skip_count: None,
        bpm: json::field::<u16>(song, "bpm").filter(|value| *value > 0),
        replay_gain_track_db: finite(json::field(replay_gain, "trackGain")),
        replay_gain_track_peak: positive_finite(json::field(replay_gain, "trackPeak")),
        replay_gain_album_db: finite(json::field(replay_gain, "albumGain")),
        replay_gain_album_peak: positive_finite(json::field(replay_gain, "albumPeak")),
        relations: TrackRelations {
            artists: artist_credits,
            album_artists: artist_credits_from_item(source, &song["albumArtists"]),
            genres: genre_credits_from_item(source, song),
            moods: moods_from_item(source, json::strings(&song["moods"])),
            music_folders: None,
        },
    })
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
pub(super) fn artist_from_json(source: &SubsonicSource, artist: &Value) -> Option<Artist> {
    let raw_id = json::id(&artist["id"])?;
    Some(Artist {
        sort_name: clean_optional(json::field(artist, "sortName")),
        id: String::from(source.id("artist", &raw_id)),
        name: json::field(artist, "name").unwrap_or_else(|| "Unknown Artist".to_string()),
        favorite: favorite(&json::field(artist, "starred")),
        last_played: normalized_timestamp(json::field(artist, "played")),
        play_count: json::field::<u64>(artist, "playCount")
            .map(|value| value.min(u64::from(u32::MAX)) as u32),
        user_rating: json::field::<u32>(artist, "userRating")
            .filter(|value| *value > 0)
            .map(|value| value.min(5).saturating_mul(2) as u8),
        musicbrainz_artist_id: clean_optional(json::field(artist, "musicBrainzId")),
        image_ref: image_ref(source, json::id(&artist["coverArt"])),
        local_artwork: None,
    })
}

fn normalized_timestamp(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn playlist_from_json(source: &SubsonicSource, playlist: &Value) -> Option<Playlist> {
    let raw_id = json::id(&playlist["id"])?;
    let writable = !json::boolean(&playlist["readonly"]).unwrap_or(false);
    // Navidrome's readonly also covers owned playlists with file-managed or smart tracks.
    let metadata_writable = writable
        || (source.flavor == SubsonicFlavor::Navidrome
            && playlist["owner"].as_str() == Some(source.username.as_str()));
    Some(Playlist {
        id: String::from(source.id("playlist", &raw_id)),
        name: json::field(playlist, "name").unwrap_or_else(|| "Untitled Playlist".to_string()),
        image_ref: image_ref(source, json::id(&playlist["coverArt"])),
        duration_seconds: 0,
        track_count: json::items(&playlist["entry"]).len(),
        revision: json::field::<String>(playlist, "changed").filter(|value| !value.is_empty()),
        valid_until: crate::policy::unix_seconds(json::field(playlist, "validUntil")),
        writable,
        metadata_writable,
    })
}

pub(super) async fn stage_playlist_header(
    scan: &mut library::Scan,
    playlist: &Playlist,
) -> library::LibraryResult<()> {
    let artwork = playlist
        .image_ref
        .as_ref()
        .map(|image| crate::native_artwork_binding(scan.source_id(), image))
        .transpose()?;
    scan.write_playlist(
        &playlist.id,
        &playlist.name,
        &playlist.name.to_lowercase(),
        &playlist.name.to_lowercase(),
        artwork.as_deref(),
    )
    .await?;
    scan.write_playlist_permissions(&playlist.id, playlist.writable, playlist.metadata_writable)
        .await
}
