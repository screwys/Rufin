//! File lyrics use the same sidecar order and parsed bundles as playback.

use std::path::{Path, PathBuf};

use lyrics::{LyricsBundle, LyricsOrigin, UserPreferences};

use super::media::ScannedTrack;

pub(crate) fn is_sidecar(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lrc"))
}

pub(crate) fn set_lyrics(track: &mut ScannedTrack, bundle: Option<LyricsBundle>) {
    let mut bundle =
        bundle.unwrap_or_else(|| LyricsBundle::from_documents(LyricsOrigin::Native, Vec::new()));
    bundle.origin = LyricsOrigin::Native;
    track.lyrics = Some(bundle);
}

pub(crate) fn read_local(track: &mut ScannedTrack) {
    let Some(path) = track.track.source_path.as_deref() else {
        return;
    };
    let input = lyrics::LocalLyricsInput {
        audio_path: PathBuf::from(path),
        title: track.track.metadata.title.clone(),
        cue_track: track.track.cue_path.is_some(),
    };
    track.lyrics_paths = lyrics::local_sidecar_candidates(&input)
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let embedded = track
        .lyrics
        .take()
        .and_then(lyrics::lyrics_with_displayable_content);
    let bundle = lyrics::local_file_lyrics(&input, &UserPreferences::default(), || embedded);
    set_lyrics(track, bundle);
}
