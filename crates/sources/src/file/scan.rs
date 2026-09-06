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
    scan.write_album(
        &track.album_id,
        &track.album,
        &track.album.to_lowercase(),
        &track.album_artist,
        &track.album.to_lowercase(),
        Some(i64::from(track.year)).filter(|year| *year > 0),
        None,
        None,
        track.musicbrainz_album_id.as_deref(),
        track.musicbrainz_release_group_id.as_deref(),
        track.is_compilation,
        album_artwork.as_deref(),
        false,
        None,
        Some(scan.accepted_at()),
    )
    .await?;
    Ok(())
}

async fn stage_track_row(scan: &mut Scan, track: &ScannedTrack) -> SourceResult<()> {
    let artwork = track
        .local_artwork
        .as_ref()
        .map(serde_json::to_vec)
        .transpose()?;
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
        Some(&track.album_id),
        &track.title,
        &normalized_search,
        &track.album,
        &track.artist,
        &track.title.to_lowercase(),
        i64::from(track.duration_seconds) * 1_000,
        i64::from(track.disc_number),
        i64::from(track.track_number),
        Some(i64::from(track.year)).filter(|year| *year > 0),
        None,
        None,
        track.local_uri.as_deref(),
        track.source_format.as_deref(),
        track.comment.as_deref(),
        track.bpm.map(i64::from),
        track.musicbrainz_recording_id.as_deref(),
        track.musicbrainz_release_track_id.as_deref(),
        track.cue_path.as_deref(),
        track.cue_start_millis,
        track.cue_end_millis,
        artwork.as_deref(),
        false,
        track.user_rating.map(i64::from),
        Some(scan.accepted_at()),
        None,
        None,
        None,
        Some(&track.source_path),
        audio_key(track),
    )
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
        .write_genre(
            &genre.id,
            &genre.name,
            &genre.name.to_lowercase(),
            &genre.name.to_lowercase(),
            None,
        )
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
        .write_artist(
            &artist.id,
            &artist.name,
            &artist.name.to_lowercase(),
            &artist.name.to_lowercase(),
            artist.musicbrainz_artist_id.as_deref(),
            None,
            None,
            None,
        )
        .await?)
}

fn audio_key(track: &ScannedTrack) -> [u8; 32] {
    let mut hash = track.audio_revision.clone();
    hash.update(track.cue_path.as_deref().unwrap_or_default().as_bytes());
    hash.update(&track.cue_start_millis.unwrap_or(-1).to_le_bytes());
    hash.update(&track.cue_end_millis.unwrap_or(-1).to_le_bytes());
    *hash.finalize().as_bytes()
}
