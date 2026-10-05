use crate::route::Route;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub use rufin_core::route::detail_links::{
    DetailLinks, album_artist_links, joined_credits, metadata_links, track_album_artist_links,
    track_artist_album_links, track_artist_links,
};

fn markup(links: &DetailLinks) -> String {
    let mut markup = String::new();
    let mut cursor = 0;
    for (link_index, link) in links.spans().iter().enumerate() {
        let prefix = links
            .display_text()
            .get(cursor..link.range.start)
            .expect("detail link ranges stay on text boundaries");
        markup.push_str(&glib::markup_escape_text(prefix));
        markup.push_str(&format!(
            r#"<a href="{link_index}" class="inline-detail-link">"#
        ));
        let link_text = links
            .display_text()
            .get(link.range.clone())
            .expect("detail link ranges stay on text boundaries");
        markup.push_str(&glib::markup_escape_text(link_text));
        markup.push_str("</a>");
        cursor = link.range.end;
    }
    let suffix = links
        .display_text()
        .get(cursor..)
        .expect("detail link ranges stay on text boundaries");
    markup.push_str(&glib::markup_escape_text(suffix));
    markup
}

#[derive(Clone)]
pub struct DetailLinkBinding {
    label: glib::WeakRef<gtk::Label>,
    links: Rc<RefCell<DetailLinks>>,
}

impl DetailLinkBinding {
    pub fn new(label: &gtk::Label, navigate: Rc<dyn Fn(Route)>) -> Self {
        let links = Rc::new(RefCell::new(DetailLinks::default()));

        label.connect_state_flags_changed(|label, previous| {
            let link_states = gtk::StateFlags::PRELIGHT
                | gtk::StateFlags::FOCUSED
                | gtk::StateFlags::FOCUS_VISIBLE;
            if (previous ^ label.state_flags()).intersects(link_states) {
                // Refresh cached link attributes when hover or keyboard focus changes.
                label.set_attributes(Some(&label.attributes().unwrap_or_default()));
            }
        });

        let activate_links = Rc::clone(&links);
        label.connect_activate_link(move |_, link| {
            let route = activate_links.borrow().route_for_link(link);
            if let Some(route) = route {
                navigate(route);
            }
            glib::Propagation::Stop
        });
        Self {
            label: label.downgrade(),
            links,
        }
    }

    pub fn links(&self) -> DetailLinks {
        self.links.borrow().clone()
    }

    pub fn bind(&self, links: DetailLinks) {
        let has_links = !links.spans().is_empty();
        let text = if has_links {
            markup(&links)
        } else {
            links.display_text().to_string()
        };
        self.links.replace(links);
        if let Some(label) = self.label.upgrade() {
            if has_links {
                label.set_markup(&text);
            } else {
                label.set_text(&text);
            }
        }
    }

    pub fn clear(&self) {
        self.links.replace(DetailLinks::default());
        if let Some(label) = self.label.upgrade() {
            label.set_text("");
        }
    }
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
        assert_eq!(markup(&links), "A label without a relationship");
        assert_eq!(links.route_for_link("0"), None);

        track.artist = "First feat. Second".to_string();
        track.artists = vec![credit(3, "First"), credit(4, "Second")];
        let links = track_artist_links(&track);
        assert_eq!(
            markup(&links),
            r#"<a href="0" class="inline-detail-link">First</a> feat. <a href="1" class="inline-detail-link">Second</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::ArtistDetail(uri("artist", 3)))
        );
        assert_eq!(
            links.route_for_link("1"),
            Some(Route::ArtistDetail(uri("artist", 4)))
        );
    }

    #[test]
    fn track_artist_links_fall_back_to_album_credits_without_parsing_the_label() {
        let mut track = track("Display name");
        track.album_artists = vec![credit(4, "Canonical artist")];
        let links = track_artist_links(&track);
        assert_eq!(
            markup(&links),
            r#"Display name, <a href="0" class="inline-detail-link">Canonical artist</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumArtistDetail(uri("artist", 4)))
        );
    }

    #[test]
    fn folder_metadata_links_artist_and_album_destinations() {
        let mut track = track("Artist");
        track.artists = vec![credit(3, "Artist")];
        track.album = "Album".to_string();
        track.album_key = Some(AlbumKey::from_raw(8));
        track.album_media_uri = Some(uri("album", 8));
        let links = track_artist_album_links(&track);
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::ArtistDetail(uri("artist", 3)))
        );
        assert_eq!(
            links.route_for_link("1"),
            Some(Route::AlbumDetail(uri("album", 8)))
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
            Some(Route::AlbumArtistDetail(uri("artist", 5)))
        );
        assert_eq!(
            links.route_for_link("1"),
            Some(Route::AlbumArtistDetail(uri("artist", 6)))
        );
        let mut track = track("First, Second");
        track.album_artists = credits;
        let links = track_album_artist_links(&track);
        assert_eq!(
            markup(&links),
            r#"<a href="0" class="inline-detail-link">First</a>, <a href="1" class="inline-detail-link">Second</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumArtistDetail(uri("artist", 5)))
        );
    }

    #[test]
    fn ordinary_detail_links_remain_single_destinations() {
        let links = DetailLinks::route("Album & title", Some(Route::AlbumDetail(uri("album", 8))));
        assert_eq!(
            markup(&links),
            r#"<a href="0" class="inline-detail-link">Album &amp; title</a>"#
        );
        assert_eq!(
            links.route_for_link("0"),
            Some(Route::AlbumDetail(uri("album", 8)))
        );
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

    fn album_credit(id: i64, name: &str) -> AlbumArtistLink {
        AlbumArtistLink {
            artist_key: ArtistKey::from_raw(id),
            media_uri: uri("artist", id),
            name: name.to_string(),
        }
    }

    fn track(display_artist: &str) -> TrackRow {
        TrackRow {
            audio_properties: Default::default(),
            source_name: String::new(),
            source_path: None,
            track_key: TrackKey::from_raw(1),
            source_key: SourceKey::from_raw(1),
            source_id: "source".to_string(),
            object_id: "track".to_string(),
            album_key: Some(AlbumKey::from_raw(1)),
            album_media_uri: None,
            title: "Track".to_string(),
            album: "Album".to_string(),
            artist: display_artist.to_string(),
            album_display_artist: None,
            duration_millis: 1,
            disc_number: 1,
            track_number: 1,
            year: None,
            release_date: None,
            date_added: None,
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "track",
                "track",
            ),
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
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
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
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "album",
                "album",
            ),
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
