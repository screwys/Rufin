//! Shared file facts become Library scan rows here.

use super::media::{self as media, ScannedTrack};
use crate::SourceResult;
use library::Scan;
use std::collections::BTreeSet;

pub(crate) async fn stage_audio_tracks_batch(
    scan: &mut Scan,
    tracks: &[ScannedTrack],
) -> SourceResult<()> {
    let mut albums = BTreeSet::new();
    let mut artists = BTreeSet::new();
    let mut genres = BTreeSet::new();
    let mut moods = BTreeSet::new();
    for track in tracks {
        if albums.insert(track.album_id.as_str()) {
            stage_album(scan, track).await?;
        }
        for artist in &track.album_artists {
            if artists.insert(artist.id.as_str()) {
                stage_artist(scan, artist).await?;
            }
        }
        stage_track_row(scan, track).await?;
        for artist in &track.artists {
            if artists.insert(artist.id.as_str()) {
                stage_artist(scan, artist).await?;
            }
        }
        for genre in &track.genres {
            if genres.insert(genre.id.as_str()) {
                stage_genre(scan, genre).await?;
            }
        }
        for mood in &track.moods {
            if moods.insert(mood.id.as_str()) {
                stage_mood(scan, mood).await?;
            }
        }
        stage_loudness(scan, track).await?;
    }
    let album_artists = tracks
        .iter()
        .flat_map(|track| {
            track
                .album_artists
                .iter()
                .map(|artist| (track.album_id.as_str(), artist.id.as_str()))
        })
        .collect::<Vec<_>>();
    let album_genres = tracks
        .iter()
        .flat_map(|track| {
            track
                .genres
                .iter()
                .map(|genre| (track.album_id.as_str(), genre.id.as_str()))
        })
        .collect::<Vec<_>>();
    let album_release_types = tracks
        .iter()
        .flat_map(|track| {
            track
                .release_types
                .iter()
                .map(|kind| (track.album_id.as_str(), kind.as_str()))
        })
        .collect::<Vec<_>>();
    let track_artists = tracks
        .iter()
        .flat_map(|track| {
            track
                .artists
                .iter()
                .map(|artist| (track.id.as_str(), artist.id.as_str()))
        })
        .collect::<Vec<_>>();
    let track_genres = tracks
        .iter()
        .flat_map(|track| {
            track
                .genres
                .iter()
                .map(|genre| (track.id.as_str(), genre.id.as_str()))
        })
        .collect::<Vec<_>>();
    let track_moods = tracks
        .iter()
        .flat_map(|track| {
            track
                .moods
                .iter()
                .map(|mood| (track.id.as_str(), mood.id.as_str()))
        })
        .collect::<Vec<_>>();
    scan.write_album_relations(&album_artists, &album_genres, &album_release_types)
        .await?;
    scan.write_track_relations(&track_artists, &track_genres, &track_moods)
        .await?;
    Ok(())
}

async fn stage_album(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    let album_artwork = track
        .local_artwork
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
    scan.write_album(&library::AlbumInput {
        object_id: track.album_id.clone(),
        title: track.album.clone(),
        display_artist: track.album_artist.clone(),
        sort_text: track
            .sort_album
            .as_deref()
            .unwrap_or(&track.album)
            .to_lowercase(),
        year: Some(i64::from(track.year)).filter(|year| *year > 0),
        release_date: track.release_date.clone(),
        musicbrainz_release_id: track.musicbrainz_album_id.clone(),
        musicbrainz_release_group_id: track.musicbrainz_release_group_id.clone(),
        is_compilation: track.is_compilation,
        artwork_binding: album_artwork,
        favorite: Some(false),
        first_seen_at: Some(scan.accepted_at()),
        ..Default::default()
    })
    .await?;
    Ok(())
}

async fn stage_track_row(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    let artwork = track
        .local_artwork
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;

    scan.write_track(&library::TrackInput {
        object_id: track.id.clone(),
        album_object_id: Some(track.album_id.clone()),
        title: track.title.clone(),
        display_album: track.album.clone(),
        display_artist: track.artist.clone(),
        sort_text: track
            .sort_title
            .as_deref()
            .unwrap_or(&track.title)
            .to_lowercase(),
        duration_millis: i64::from(track.duration_seconds) * 1_000,
        disc_number: i64::from(track.disc_number),
        track_number: i64::from(track.track_number),
        year: Some(i64::from(track.year)).filter(|year| *year > 0),
        release_date: track.release_date.clone(),
        media_uri: track.local_uri.clone(),
        source_format: track.source_format.clone(),
        comment: track.comment.clone(),
        bpm: track.bpm.map(i64::from),
        musicbrainz_recording_id: track.musicbrainz_recording_id.clone(),
        musicbrainz_release_track_id: track.musicbrainz_release_track_id.clone(),
        cue_path: track.cue_path.clone(),
        cue_start_millis: track.cue_start_millis,
        cue_end_millis: track.cue_end_millis,
        artwork_binding: artwork,
        favorite: Some(false),
        rating: track.user_rating.map(i64::from),
        first_seen_at: Some(scan.accepted_at()),
        source_path: Some(track.source_path.clone()),
        loudness_analysis_key: audio_key(track),
        audio_properties: track.audio_properties.clone(),
        ..Default::default()
    })
    .await?;
    Ok(())
}

async fn stage_loudness(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    if track.track_r128_lufs.is_some() || track.replay_gain_track_db.is_some() {
        scan.write_track_source_loudness(
            &track.id,
            track.track_r128_lufs,
            None,
            track.replay_gain_track_db,
            track.replay_gain_track_peak,
        )
        .await?;
    }
    if track.album_r128_lufs.is_some() || track.replay_gain_album_db.is_some() {
        scan.write_album_source_loudness(
            &track.album_id,
            track.album_r128_lufs,
            None,
            track.replay_gain_album_db,
            track.replay_gain_album_peak,
        )
        .await?;
    }
    Ok(())
}

async fn stage_genre(scan: &mut Scan, genre: &media::NamedCredit) -> SourceResult<()> {
    Ok(scan
        .write_genre(&library::GenreInput {
            object_id: genre.id.clone(),
            name: genre.name.clone(),
            sort_text: Some(genre.name.to_lowercase()),
            ..Default::default()
        })
        .await?)
}

async fn stage_mood(scan: &mut Scan, mood: &media::NamedCredit) -> SourceResult<()> {
    Ok(scan
        .write_mood(
            &mood.id,
            &mood.name,
            &mood.name.to_lowercase(),
            &mood.name.to_lowercase(),
        )
        .await?)
}

async fn stage_artist(scan: &mut Scan, artist: &media::ArtistCredit) -> SourceResult<()> {
    Ok(scan
        .write_artist(&library::ArtistInput {
            object_id: artist.id.clone(),
            name: artist.name.clone(),
            sort_text: Some(
                artist
                    .sort_name
                    .as_deref()
                    .unwrap_or(&artist.name)
                    .to_lowercase(),
            ),
            musicbrainz_artist_id: artist.musicbrainz_artist_id.clone(),
            ..Default::default()
        })
        .await?)
}

fn audio_key(track: &ScannedTrack) -> [u8; 32] {
    let mut hash = track.audio_revision.clone();
    hash.update(track.cue_path.as_deref().unwrap_or_default().as_bytes());
    hash.update(&track.cue_start_millis.unwrap_or(-1).to_le_bytes());
    hash.update(&track.cue_end_millis.unwrap_or(-1).to_le_bytes());
    *hash.finalize().as_bytes()
}
