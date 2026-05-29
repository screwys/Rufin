use std::collections::HashSet;

use rufin_core::{Album, AppSettings, Artist, Genre, ImageRef, Playlist, Track};

use crate::external_metadata;

pub(super) fn external_album_image_refs_from_albums(
    mut albums: Vec<Album>,
    settings: &AppSettings,
) -> Vec<ImageRef> {
    let mut image_refs = Vec::new();
    let mut seen = HashSet::new();
    external_metadata::normalize_albums(&mut albums, settings);
    for image_ref in albums
        .iter()
        .filter_map(|album| album.image_ref.as_ref())
        .filter(|image_ref| external_metadata::is_external_image_ref(image_ref))
    {
        let key = (
            image_ref.item_id.clone(),
            image_ref.tag.clone().unwrap_or_default(),
        );
        if seen.insert(key) {
            image_refs.push(image_ref.clone());
        }
    }
    image_refs
}

pub(super) fn provider_artist_image_refs_from_artists(artists: Vec<Artist>) -> Vec<ImageRef> {
    let mut image_refs = Vec::new();
    let mut seen = HashSet::new();
    push_provider_artist_image_refs(&mut image_refs, &mut seen, artists);
    image_refs
}

pub(super) fn push_provider_album_image_refs(
    image_refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    albums: Vec<Album>,
) {
    for album in albums {
        push_provider_image_ref(image_refs, seen, album.image_ref.as_ref());
    }
}

pub(super) fn push_provider_track_image_refs(
    image_refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    tracks: Vec<Track>,
) {
    for track in tracks {
        push_provider_image_ref(image_refs, seen, track.image_ref.as_ref());
    }
}

pub(super) fn push_provider_artist_image_refs(
    image_refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    artists: Vec<Artist>,
) {
    for artist in artists {
        push_provider_image_ref(image_refs, seen, artist.image_ref.as_ref());
    }
}

pub(super) fn push_provider_genre_image_refs(
    image_refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    genres: Vec<Genre>,
) {
    for genre in genres {
        push_provider_image_ref(image_refs, seen, genre.image_ref.as_ref());
    }
}

pub(super) fn push_provider_playlist_image_refs(
    image_refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    playlists: Vec<Playlist>,
) {
    for playlist in playlists {
        push_provider_image_ref(image_refs, seen, playlist.image_ref.as_ref());
    }
}

fn push_provider_image_ref(
    image_refs: &mut Vec<ImageRef>,
    seen: &mut HashSet<(String, String)>,
    image_ref: Option<&ImageRef>,
) {
    let Some(image_ref) = image_ref else {
        return;
    };
    if external_metadata::is_external_image_ref(image_ref) {
        return;
    }
    let key = (
        image_ref.item_id.clone(),
        image_ref.tag.clone().unwrap_or_default(),
    );
    if seen.insert(key) {
        image_refs.push(image_ref.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rufin_core::{AlbumId, ArtistId, TrackId};

    #[test]
    fn synced_external_cover_candidates_use_only_albums_without_provider_art() {
        let settings = AppSettings {
            external_metadata_enabled: true,
            ..AppSettings::default()
        };
        let refs = external_album_image_refs_from_albums(
            vec![
                album_without_cover(1, "Loveless", "My Bloody Valentine"),
                album_with_cover(2, "Souvlaki", "Slowdive"),
                album_without_cover(3, "Loveless", "My Bloody Valentine"),
            ],
            &settings,
        );

        assert_eq!(refs.len(), 1);
        assert!(external_metadata::is_external_image_ref(&refs[0]));
        assert_eq!(
            external_metadata::album_art_from_image_ref(&refs[0]).map(|art| art.album),
            Some("Loveless".to_string())
        );
    }

    #[test]
    fn synced_external_cover_candidates_respect_private_mode() {
        let settings = AppSettings {
            external_metadata_enabled: true,
            private_mode: true,
            ..AppSettings::default()
        };

        assert!(
            external_album_image_refs_from_albums(
                vec![album_without_cover(1, "Loveless", "My Bloody Valentine")],
                &settings,
            )
            .is_empty()
        );
    }

    #[test]
    fn synced_external_cover_candidates_keep_existing_external_refs() {
        let settings = AppSettings {
            external_metadata_enabled: true,
            ..AppSettings::default()
        };
        let refs = external_album_image_refs_from_albums(
            vec![Album {
                image_ref: Some(ImageRef::new(
                    "external:album:Example%20Artist:Example%20Album",
                    Some("external-v1-existing".to_string()),
                )),
                ..album_without_cover(1, "Example Album", "Example Artist")
            }],
            &settings,
        );

        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].item_id,
            "external:album:Example%20Artist:Example%20Album"
        );
    }

    #[test]
    fn synced_provider_artist_cover_candidates_use_only_provider_art() {
        let refs = provider_artist_image_refs_from_artists(vec![
            artist_without_cover(1, "Slowdive"),
            artist_with_cover(2, "Ride"),
            artist_with_cover(2, "Ride"),
        ]);

        assert_eq!(refs.len(), 1);
        assert!(!external_metadata::is_external_image_ref(&refs[0]));
        assert_eq!(refs[0].item_id, "provider-artist-2");
    }

    #[test]
    fn synced_provider_artist_cover_candidates_skip_synthetic_external_refs() {
        assert!(
            provider_artist_image_refs_from_artists(vec![Artist {
                image_ref: Some(ImageRef::new(
                    "external:artist:Slowdive",
                    Some("external-artist-v1-old".to_string()),
                )),
                ..artist_without_cover(1, "Slowdive")
            }])
            .is_empty()
        );
    }

    #[test]
    fn initial_provider_cover_candidates_include_track_refs_once() {
        let mut refs = Vec::new();
        let mut seen = HashSet::new();
        push_provider_album_image_refs(
            &mut refs,
            &mut seen,
            vec![album_with_cover(1, "Souvlaki", "Slowdive")],
        );
        push_provider_track_image_refs(
            &mut refs,
            &mut seen,
            vec![
                track_with_cover(
                    1,
                    "Alison",
                    ImageRef::new("provider-album-1", Some("tag-1".to_string())),
                ),
                track_with_cover(
                    2,
                    "Machine Gun",
                    ImageRef::new("provider-track-2", Some("tag-2".to_string())),
                ),
                track_with_cover(
                    3,
                    "Sing",
                    ImageRef::new(
                        "external:album:Example%20Artist:Example%20Album",
                        Some("external-v1-test".to_string()),
                    ),
                ),
            ],
        );

        assert_eq!(
            refs.iter()
                .map(|image_ref| image_ref.item_id.as_str())
                .collect::<Vec<_>>(),
            vec!["provider-album-1", "provider-track-2"]
        );
    }

    fn album_without_cover(number: u32, title: &str, artist: &str) -> Album {
        Album {
            id: AlbumId::fake(number),
            title: title.to_string(),
            artist: artist.to_string(),
            artist_id: Some(ArtistId::fake(number)),
            album_artist_credits: Vec::new(),
            artist_credits: Vec::new(),
            year: 1991,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            track_count: 1,
            duration_seconds: 60,
            favorite: false,
            color_seed: number,
            image_ref: None,
            genres: Vec::new(),
        }
    }

    fn album_with_cover(number: u32, title: &str, artist: &str) -> Album {
        Album {
            image_ref: Some(ImageRef::new(
                format!("provider-album-{number}"),
                Some(format!("tag-{number}")),
            )),
            ..album_without_cover(number, title, artist)
        }
    }

    fn track_without_cover(number: u32, title: &str) -> Track {
        Track {
            id: TrackId::fake(number),
            album_id: AlbumId::fake(number),
            title: title.to_string(),
            artist: "Example Artist".to_string(),
            artist_id: Some(ArtistId::fake(number)),
            artist_credits: Vec::new(),
            album_artist_credits: Vec::new(),
            album: "Example Album".to_string(),
            year: 1991,
            release_date: None,
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: None,
            duration_seconds: 60,
            favorite: false,
            disc_number: 1,
            track_number: number as u16,
            image_ref: None,
            genres: Vec::new(),
            local_path: None,
            source_format: None,
        }
    }

    fn track_with_cover(number: u32, title: &str, image_ref: ImageRef) -> Track {
        Track {
            image_ref: Some(image_ref),
            ..track_without_cover(number, title)
        }
    }

    fn artist_without_cover(number: u32, name: &str) -> Artist {
        Artist {
            id: ArtistId::fake(number),
            name: name.to_string(),
            album_count: 1,
            track_count: 1,
            favorite: false,
            last_played: None,
            play_count: None,
            user_rating: None,
            image_ref: None,
        }
    }

    fn artist_with_cover(number: u32, name: &str) -> Artist {
        Artist {
            image_ref: Some(ImageRef::new(
                format!("provider-artist-{number}"),
                Some(format!("tag-{number}")),
            )),
            ..artist_without_cover(number, name)
        }
    }
}
