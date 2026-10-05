use super::Route;
use library::{AlbumArtistLink, AlbumRow, TrackArtistLink, TrackRow};
use std::ops::Range;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DetailLinks {
    text: String,
    links: Vec<DetailLink>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetailLink {
    pub range: Range<usize>,
    pub route: Route,
}

impl DetailLinks {
    pub fn text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            links: Vec::new(),
        }
    }

    pub fn route(text: &str, route: Option<Route>) -> Self {
        let Some(route) = route else {
            return Self::text(text);
        };
        let end = text.len();
        Self {
            text: text.to_string(),
            links: vec![DetailLink {
                range: 0..end,
                route,
            }],
        }
    }

    fn artist_text<C>(
        text: &str,
        credits: &[C],
        key: impl Fn(&C) -> String,
        name: impl Fn(&C) -> &str,
        album_artist: bool,
    ) -> Self {
        let credits = credits
            .iter()
            .filter(|credit| !name(credit).trim().is_empty())
            .collect::<Vec<_>>();
        if credits.is_empty() {
            return Self::text(text);
        }

        let mut folded = String::new();
        let mut boundaries = Vec::new();
        for (offset, character) in text.char_indices() {
            boundaries.push((folded.len(), offset));
            folded.extend(character.to_lowercase());
        }
        boundaries.push((folded.len(), text.len()));
        let mut candidates = credits
            .iter()
            .enumerate()
            .flat_map(|(credit_index, credit)| {
                folded
                    .match_indices(&name(credit).to_lowercase())
                    .filter_map(|(start, matched)| {
                        let start = boundaries
                            .binary_search_by_key(&start, |(folded, _)| *folded)
                            .ok()?;
                        let end = boundaries
                            .binary_search_by_key(
                                &(boundaries[start].0 + matched.len()),
                                |(folded, _)| *folded,
                            )
                            .ok()?;
                        Some((boundaries[start].1, boundaries[end].1, credit_index))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| (right.1 - right.0).cmp(&(left.1 - left.0)))
                .then_with(|| left.2.cmp(&right.2))
        });

        let mut matched_credits = vec![false; credits.len()];
        let mut spans = Vec::new();
        let mut cursor = 0;
        for (start, end, credit_index) in candidates {
            if start < cursor {
                continue;
            }
            spans.push((start, end, credit_index));
            matched_credits[credit_index] = true;
            cursor = end;
        }

        let mut text = text.to_string();
        let mut links = spans
            .into_iter()
            .map(|(start, end, credit_index)| DetailLink {
                range: start..end,
                route: if album_artist {
                    Route::AlbumArtistDetail(key(credits[credit_index]))
                } else {
                    Route::ArtistDetail(key(credits[credit_index]))
                },
            })
            .collect::<Vec<_>>();

        for (credit_index, credit) in credits.iter().enumerate() {
            if matched_credits[credit_index] {
                continue;
            }
            if !text.is_empty() {
                text.push_str(", ");
            }
            let start = text.len();
            text.push_str(name(credit).trim());
            links.push(DetailLink {
                range: start..text.len(),
                route: if album_artist {
                    Route::AlbumArtistDetail(key(credit))
                } else {
                    Route::ArtistDetail(key(credit))
                },
            });
        }

        Self { text, links }
    }

    pub fn route_for_link(&self, link: &str) -> Option<Route> {
        link.parse::<usize>()
            .ok()
            .and_then(|index| self.links.get(index))
            .map(|link| link.route.clone())
    }
    pub fn display_text(&self) -> &str {
        &self.text
    }
    pub fn spans(&self) -> &[DetailLink] {
        &self.links
    }
}

pub fn track_artist_links(track: &TrackRow) -> DetailLinks {
    let album_artist = track.artists.is_empty();
    let credits = if track.artists.is_empty() {
        &track.album_artists
    } else {
        &track.artists
    };
    DetailLinks::artist_text(
        &track.artist,
        credits,
        |credit: &TrackArtistLink| credit.media_uri.clone(),
        |credit| credit.name.as_str(),
        album_artist,
    )
}

pub fn metadata_links(
    field: crate::settings::layout::LibraryField,
    text: &str,
    album_uri: Option<&str>,
    artists: &[TrackArtistLink],
    album_artists: &[TrackArtistLink],
) -> DetailLinks {
    match field {
        crate::settings::layout::LibraryField::Album => DetailLinks::route(
            text,
            album_uri.map(|uri| Route::AlbumDetail(uri.to_owned())),
        ),
        crate::settings::layout::LibraryField::Artist
        | crate::settings::layout::LibraryField::AlbumArtist => {
            let album_artist =
                field == crate::settings::layout::LibraryField::AlbumArtist || artists.is_empty();
            DetailLinks::artist_text(
                text,
                if album_artist { album_artists } else { artists },
                |credit| credit.media_uri.clone(),
                |credit| credit.name.as_str(),
                album_artist,
            )
        }
        _ => DetailLinks::text(text),
    }
}

pub use crate::settings::presentation::joined_credits;

pub fn track_album_artist_links(track: &TrackRow) -> DetailLinks {
    let text = joined_credits(&track.album_artists);
    DetailLinks::artist_text(
        &text,
        &track.album_artists,
        |credit: &TrackArtistLink| credit.media_uri.clone(),
        |credit| credit.name.as_str(),
        true,
    )
}

pub fn track_artist_album_links(track: &TrackRow) -> DetailLinks {
    let mut links = track_artist_links(track);
    if track.album.trim().is_empty() {
        return links;
    }
    if !links.text.is_empty() {
        links.text.push_str(" / ");
    }
    let start = links.text.len();
    links.text.push_str(&track.album);
    if let Some(album_uri) = &track.album_media_uri {
        links.links.push(DetailLink {
            range: start..links.text.len(),
            route: Route::AlbumDetail(album_uri.clone()),
        });
    }
    links
}

pub fn album_artist_links(album: &AlbumRow) -> DetailLinks {
    DetailLinks::artist_text(
        &album.display_artist,
        &album.album_artists,
        |credit: &AlbumArtistLink| credit.media_uri.clone(),
        |credit| credit.name.as_str(),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use library::ArtistKey;
    #[test]
    fn credited_case_preserves_display_text_and_unicode_link_boundaries() {
        for (display, credited) in [
            ("Mei Ehara", "mei ehara"),
            ("Édith", "édith"),
            ("İpek", "i\u{307}pek"),
        ] {
            let links = DetailLinks::artist_text(
                display,
                &[credit(3, credited)],
                |artist| artist.media_uri.clone(),
                |artist| artist.name.as_str(),
                false,
            );
            assert_eq!(links.text, display);
            assert_eq!(links.links.len(), 1);
            assert_eq!(links.links[0].range, 0..display.len());
        }
    }
    #[test]
    fn projected_metadata_keeps_album_and_all_artist_routes() {
        let album_uri = uri("album", 9);
        let artists = [credit(3, "First"), credit(4, "Second")];
        let album_artists = [credit(5, "Album Artist")];
        let links = metadata_links(
            crate::settings::layout::LibraryField::Artist,
            "First feat. Second",
            Some(&album_uri),
            &artists,
            &album_artists,
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::ArtistDetail(uri("artist", 3)))
        );
        assert_eq!(
            links.route_for_link("1"),
            Some(Route::ArtistDetail(uri("artist", 4)))
        );
        let links = metadata_links(
            crate::settings::layout::LibraryField::Album,
            "Album",
            Some(&album_uri),
            &artists,
            &album_artists,
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumDetail(album_uri))
        );
        let links = metadata_links(
            crate::settings::layout::LibraryField::AlbumArtist,
            "Album Artist",
            None,
            &artists,
            &album_artists,
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumArtistDetail(uri("artist", 5)))
        );
        let missing = metadata_links(
            crate::settings::layout::LibraryField::Album,
            "Unavailable album",
            None,
            &[],
            &[],
        );
        assert_eq!(missing.route_for_link("0"), None);
        assert_eq!(missing.text, "Unavailable album");
    }
    fn uri(kind: &str, id: i64) -> String {
        library::source_entity_uri(&library::SourceId::new("source"), kind, &id.to_string())
    }

    fn credit(id: i64, name: &str) -> TrackArtistLink {
        TrackArtistLink {
            artist_key: ArtistKey::from_raw(id),
            media_uri: uri("artist", id),
            name: name.to_string(),
        }
    }
}
