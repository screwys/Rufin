use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use crate::preferences::source::login::source_kind_icon_name;
use crate::shell::Shell;
use ::library::{AlbumArtistLink, AlbumRow, ArtistKey, TrackArtistLink, TrackRow};
use adw::prelude::{ObjectExt, WidgetExt};
use gtk::glib;
use localization::msgid;

use super::route::Route;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DetailLinks {
    text: String,
    links: Vec<DetailLink>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DetailLink {
    range: Range<usize>,
    route: Route,
}

impl DetailLinks {
    pub(crate) fn text(text: &str) -> Self {
        Self {
            text: text.to_string(),
            links: Vec::new(),
        }
    }

    pub(crate) fn route(text: &str, route: Option<Route>) -> Self {
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
        key: impl Fn(&C) -> ArtistKey,
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

        let mut candidates = credits
            .iter()
            .enumerate()
            .flat_map(|(credit_index, credit)| {
                text.match_indices(name(credit))
                    .map(move |(start, matched)| (start, start + matched.len(), credit_index))
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

    fn markup(&self, hovered: Option<(usize, &gtk::gdk::RGBA)>) -> String {
        let mut markup = String::new();
        let mut cursor = 0;
        for (link_index, link) in self.links.iter().enumerate() {
            let prefix = self
                .text
                .get(cursor..link.range.start)
                .expect("detail link ranges stay on text boundaries");
            markup.push_str(&glib::markup_escape_text(prefix));
            markup.push_str(&format!(
                r#"<a href="{link_index}" class="inline-detail-link">"#
            ));
            let link_text = self
                .text
                .get(link.range.clone())
                .expect("detail link ranges stay on text boundaries");
            let text = glib::markup_escape_text(link_text);
            if let Some((hovered_index, color)) = hovered
                && hovered_index == link_index
            {
                let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                let luminance =
                    color.red() * 0.2126 + color.green() * 0.7152 + color.blue() * 0.0722;
                let underline_target = if luminance > 0.5 { 0.0 } else { 1.0 };
                let underline_channel =
                    |value: f32| channel(value * 0.65 + underline_target * 0.35);
                markup.push_str(&format!(
                    r##"<span foreground="#{:02X}{:02X}{:02X}{:02X}" underline="single" underline_color="#{:02X}{:02X}{:02X}">{text}</span>"##,
                    channel(color.red()),
                    channel(color.green()),
                    channel(color.blue()),
                    channel(color.alpha()),
                    underline_channel(color.red()),
                    underline_channel(color.green()),
                    underline_channel(color.blue()),
                ));
            } else {
                markup.push_str(&text);
            }
            markup.push_str("</a>");
            cursor = link.range.end;
        }
        let suffix = self
            .text
            .get(cursor..)
            .expect("detail link ranges stay on text boundaries");
        markup.push_str(&glib::markup_escape_text(suffix));
        markup
    }

    pub(crate) fn route_for_link(&self, link: &str) -> Option<Route> {
        link.parse::<usize>()
            .ok()
            .and_then(|index| self.links.get(index))
            .map(|link| link.route.clone())
    }
}

#[derive(Clone)]
pub(crate) struct DetailLinkBinding {
    label: gtk::Label,
    links: Rc<RefCell<DetailLinks>>,
    hovered: Rc<Cell<Option<usize>>>,
}

impl DetailLinkBinding {
    pub(crate) fn new(label: &gtk::Label, shell: &Rc<Shell>) -> Self {
        let links = Rc::new(RefCell::new(DetailLinks::default()));
        let hovered = Rc::new(Cell::new(None));

        let update_hover = {
            let label = label.downgrade();
            let links = Rc::clone(&links);
            let hovered = Rc::clone(&hovered);
            Rc::new(move |x: f64, y: f64| {
                let Some(label) = label.upgrade() else {
                    return;
                };
                let (offset_x, offset_y) = label.layout_offsets();
                let scale = f64::from(gtk::pango::SCALE);
                let (inside, byte_index, _) = label.layout().xy_to_index(
                    ((x - f64::from(offset_x)) * scale) as i32,
                    ((y - f64::from(offset_y)) * scale) as i32,
                );
                let hovered_link = inside.then_some(byte_index).and_then(|byte_index| {
                    usize::try_from(byte_index).ok().and_then(|byte_index| {
                        links
                            .borrow()
                            .links
                            .iter()
                            .position(|link| link.range.contains(&byte_index))
                    })
                });
                if hovered.replace(hovered_link) == hovered_link {
                    return;
                }
                let markup = if let Some(link_index) = hovered_link {
                    let color = label.color();
                    links.borrow().markup(Some((link_index, &color)))
                } else {
                    links.borrow().markup(None)
                };
                label.set_markup(&markup);
            })
        };
        let motion = gtk::EventControllerMotion::new();
        let enter_hover = Rc::clone(&update_hover);
        motion.connect_enter(move |_, x, y| enter_hover(x, y));
        motion.connect_motion(move |_, x, y| update_hover(x, y));
        let leave_label = label.downgrade();
        let leave_links = Rc::clone(&links);
        let leave_hovered = Rc::clone(&hovered);
        motion.connect_leave(move |_| {
            if leave_hovered.take().is_some()
                && let Some(label) = leave_label.upgrade()
            {
                label.set_markup(&leave_links.borrow().markup(None));
            }
        });
        label.add_controller(motion);

        let activate_links = Rc::clone(&links);
        let shell = Rc::clone(shell);
        label.connect_activate_link(move |_, link| {
            if let Some(route) = activate_links.borrow().route_for_link(link) {
                shell.navigate(route);
            }
            glib::Propagation::Stop
        });
        Self {
            label: label.clone(),
            links,
            hovered,
        }
    }

    pub(crate) fn bind(&self, links: DetailLinks) {
        let markup = links.markup(None);
        self.hovered.set(None);
        self.links.replace(links);
        self.label.set_markup(&markup);
    }

    pub(crate) fn clear(&self) {
        self.hovered.set(None);
        self.links.replace(DetailLinks::default());
        self.label.set_text("");
    }
}

pub(crate) fn track_artist_links(track: &TrackRow) -> DetailLinks {
    let album_artist = track.artists.is_empty();
    let credits = if track.artists.is_empty() {
        &track.album_artists
    } else {
        &track.artists
    };
    DetailLinks::artist_text(
        &track.display_artist,
        credits,
        |credit: &TrackArtistLink| credit.artist_key,
        |credit| credit.name.as_str(),
        album_artist,
    )
}

pub(crate) fn playback_artist_links(text: &str, credits: &[TrackArtistLink]) -> DetailLinks {
    DetailLinks::artist_text(
        text,
        credits,
        |credit: &TrackArtistLink| credit.artist_key,
        |credit| credit.name.as_str(),
        false,
    )
}

pub(crate) fn track_album_artist_links(track: &TrackRow) -> DetailLinks {
    let text = super::library_fields::joined_credits(&track.album_artists);
    DetailLinks::artist_text(
        &text,
        &track.album_artists,
        |credit: &TrackArtistLink| credit.artist_key,
        |credit| credit.name.as_str(),
        true,
    )
}

pub(crate) fn album_artist_links(album: &AlbumRow) -> DetailLinks {
    DetailLinks::artist_text(
        &album.display_artist,
        &album.album_artists,
        |credit: &AlbumArtistLink| credit.artist_key,
        |credit| credit.name.as_str(),
        true,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DetailEntityKind {
    Album,
    Artist,
}

impl DetailEntityKind {
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
}

pub(crate) struct DetailExternalLink {
    pub(crate) label: &'static str,
    pub(crate) icon_name: &'static str,
    pub(crate) url: String,
}

pub(crate) fn server_entity_link(
    source_kind: &str,
    base_url: &str,
    kind: DetailEntityKind,
    entity_id: &str,
) -> Option<DetailExternalLink> {
    let base_url = clean_source_base_url(base_url)?;
    match source_kind {
        "jellyfin" => {
            let item_id = raw_source_entity_id(entity_id, "jellyfin", kind)?;
            Some(DetailExternalLink {
                label: msgid("Open on Jellyfin"),
                icon_name: source_kind_icon_name("jellyfin")?,
                url: format!("{base_url}/web/index.html#!/details?id={item_id}"),
            })
        }
        "navidrome" => {
            let item_id = raw_source_entity_id(entity_id, "navidrome", kind)?;
            Some(DetailExternalLink {
                label: msgid("Open on Navidrome"),
                icon_name: source_kind_icon_name("navidrome")?,
                url: format!(
                    "{base_url}/app/#/{}/{}/show",
                    kind.id_prefix(),
                    percent_encode_path_segment(item_id)
                ),
            })
        }
        _ => None,
    }
}

fn raw_source_entity_id<'a>(
    entity_id: &'a str,
    source_kind: &str,
    kind: DetailEntityKind,
) -> Option<&'a str> {
    let raw_id = entity_id.strip_prefix(&format!("{source_kind}:{}:", kind.id_prefix()))?;
    let raw_id = raw_id.trim();
    (!raw_id.is_empty()).then_some(raw_id)
}

fn clean_source_base_url(base_url: &str) -> Option<&str> {
    let base_url = base_url.trim().trim_end_matches('/');
    (!base_url.is_empty()).then_some(base_url)
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use library::{
        AlbumArtistLink, AlbumKey, AlbumRow, ArtistKey, SourceKey, TrackArtistLink, TrackKey,
        TrackRow,
    };

    #[test]
    fn track_artist_links_preserve_text_and_each_canonical_destination() {
        let mut track = track("A label without a relationship");
        let links = track_artist_links(&track);
        assert_eq!(links.markup(None), "A label without a relationship");
        assert_eq!(links.route_for_link("0"), None);

        track.display_artist = "First feat. Second".to_string();
        track.artists = vec![credit(3, "First"), credit(4, "Second")];
        let links = track_artist_links(&track);
        assert_eq!(
            links.markup(None),
            r#"<a href="0" class="inline-detail-link">First</a> feat. <a href="1" class="inline-detail-link">Second</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::ArtistDetail(ArtistKey::from_raw(3)))
        );
        assert_eq!(
            links.route_for_link("1"),
            Some(Route::ArtistDetail(ArtistKey::from_raw(4)))
        );
    }

    #[test]
    fn track_artist_links_fall_back_to_album_credits_without_parsing_the_label() {
        let mut track = track("Display name");
        track.album_artists = vec![credit(4, "Canonical artist")];
        let links = track_artist_links(&track);
        assert_eq!(
            links.markup(None),
            r#"Display name, <a href="0" class="inline-detail-link">Canonical artist</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumArtistDetail(ArtistKey::from_raw(4)))
        );
    }

    #[test]
    fn album_and_track_album_artist_links_keep_all_credits() {
        let credits = vec![credit(5, "First"), credit(6, "Second")];
        let mut album = album();
        album.album_artists = vec![album_credit(5, "First"), album_credit(6, "Second")];
        let links = album_artist_links(&album);
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumArtistDetail(ArtistKey::from_raw(5)))
        );
        assert_eq!(
            links.route_for_link("1"),
            Some(Route::AlbumArtistDetail(ArtistKey::from_raw(6)))
        );
        let mut track = track("First, Second");
        track.album_artists = credits;
        let links = track_album_artist_links(&track);
        assert_eq!(
            links.markup(None),
            r#"<a href="0" class="inline-detail-link">First</a>, <a href="1" class="inline-detail-link">Second</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumArtistDetail(ArtistKey::from_raw(5)))
        );
    }

    #[test]
    fn ordinary_detail_links_remain_single_destinations() {
        let links = DetailLinks::route(
            "Album & title",
            Some(Route::AlbumDetail(AlbumKey::from_raw(8))),
        );
        assert_eq!(
            links.markup(None),
            r#"<a href="0" class="inline-detail-link">Album &amp; title</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumDetail(AlbumKey::from_raw(8)))
        );
    }

    fn credit(id: i64, name: &str) -> TrackArtistLink {
        TrackArtistLink {
            artist_key: ArtistKey::from_raw(id),
            name: name.to_string(),
        }
    }

    fn album_credit(id: i64, name: &str) -> AlbumArtistLink {
        AlbumArtistLink {
            artist_key: ArtistKey::from_raw(id),
            name: name.to_string(),
        }
    }

    fn track(display_artist: &str) -> TrackRow {
        TrackRow {
            track_key: TrackKey::from_raw(1),
            source_key: SourceKey::from_raw(1),
            source_id: "source".to_string(),
            object_id: "track".to_string(),
            album_key: Some(AlbumKey::from_raw(1)),
            title: "Track".to_string(),
            display_album: "Album".to_string(),
            display_artist: display_artist.to_string(),
            duration_millis: 1,
            disc_number: 1,
            track_number: 1,
            year: None,
            release_date: None,
            date_added: None,
            media_uri: None,
            source_format: None,
            comment: None,
            bpm: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            cue_path: None,
            cue_start_millis: None,
            cue_end_millis: None,
            loudness_analysis_key: [0; 32],
            artwork_binding: None,
            favorite: false,
            rating: None,
            last_played: None,
            play_count: 0,
            skip_count: 0,
            is_downloaded: false,
            artists: Vec::new(),
            album_artists: Vec::new(),
            genres: Vec::new(),
        }
    }

    fn album() -> AlbumRow {
        AlbumRow {
            album_key: AlbumKey::from_raw(1),
            source_key: SourceKey::from_raw(1),
            object_id: "album".to_string(),
            title: "Album".to_string(),
            display_artist: "First, Second".to_string(),
            year: None,
            release_date: None,
            date_added: None,
            musicbrainz_release_id: None,
            musicbrainz_release_group_id: None,
            is_compilation: None,
            release_lookup_identity: None,
            artwork_binding: None,
            favorite: false,
            rating: None,
            play_count: 0,
            last_played: None,
            track_count: 0,
            duration_millis: 0,
            downloaded_count: 0,
            album_artists: Vec::new(),
            genres: Vec::new(),
            release_types: Vec::new(),
        }
    }
}
