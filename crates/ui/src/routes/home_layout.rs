use adw::prelude::*;

use crate::localization::localized_label;

use crate::shell::actions::icon_button;

const HOME_SHOWCASE_METADATA_MIN_WIDTH: i32 = 360;
const HOME_SHOWCASE_COVER_GROWTH_WIDTH: i32 = 444;
const HOME_SHOWCASE_COMPACT_WIDTH: i32 = 640;
const HOME_SHOWCASE_FULL_COVER: i32 = 196;
const HOME_SHOWCASE_MIN_COVER: i32 = 150;
const HOME_SHOWCASE_TIGHT_WIDTH: i32 = 520;

pub(super) struct HomeSectionHeader {
    pub(super) root: gtk::Box,
    pub(super) previous: gtk::Button,
    pub(super) next: gtk::Button,
    pub(super) refresh: gtk::Button,
}

pub(super) fn home_section_header(title: &str) -> HomeSectionHeader {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Fill);
    header.set_width_request(1);

    let heading = localized_label(title);
    heading.add_css_class("section-heading");
    heading.set_xalign(0.0);
    heading.set_hexpand(false);
    heading.set_width_chars(1);
    heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
    header.append(&heading);

    let refresh = icon_button("rufin-view-refresh-symbolic", "Refresh section");
    refresh.add_css_class("home-section-control-button");
    header.append(&refresh);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&spacer);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.set_halign(gtk::Align::End);
    controls.set_hexpand(false);

    let previous = icon_button("rufin-go-previous-symbolic", "Previous page");
    let next = icon_button("rufin-go-next-symbolic", "Next page");
    next.add_css_class("home-section-control-button");
    controls.append(&previous);
    controls.append(&next);
    header.append(&controls);

    HomeSectionHeader {
        root: header,
        previous,
        next,
        refresh,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HomeShowcaseMode {
    CoverOnly,
    Compact,
    Full,
}

pub(super) fn home_showcase_mode(width: i32) -> HomeShowcaseMode {
    if width < HOME_SHOWCASE_METADATA_MIN_WIDTH {
        HomeShowcaseMode::CoverOnly
    } else if home_showcase_is_compact(width) {
        HomeShowcaseMode::Compact
    } else {
        HomeShowcaseMode::Full
    }
}

pub(super) fn home_showcase_cover_size(width: i32) -> i32 {
    if width < HOME_SHOWCASE_METADATA_MIN_WIDTH {
        width.clamp(96, HOME_SHOWCASE_MIN_COVER)
    } else if width < HOME_SHOWCASE_COVER_GROWTH_WIDTH {
        HOME_SHOWCASE_MIN_COVER
    } else if width < HOME_SHOWCASE_COMPACT_WIDTH {
        HOME_SHOWCASE_MIN_COVER
            + ((width - HOME_SHOWCASE_COVER_GROWTH_WIDTH)
                * (HOME_SHOWCASE_FULL_COVER - HOME_SHOWCASE_MIN_COVER)
                / (HOME_SHOWCASE_COMPACT_WIDTH - HOME_SHOWCASE_COVER_GROWTH_WIDTH))
    } else {
        HOME_SHOWCASE_FULL_COVER
    }
}

pub(super) fn home_showcase_is_compact(width: i32) -> bool {
    width < HOME_SHOWCASE_COMPACT_WIDTH
}

pub(super) fn home_showcase_spacing(width: i32) -> i32 {
    if width < HOME_SHOWCASE_TIGHT_WIDTH {
        12
    } else if home_showcase_is_compact(width) {
        18
    } else {
        24
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HOME_SHOWCASE_FULL_COVER, HOME_SHOWCASE_MIN_COVER, HomeShowcaseMode,
        home_showcase_cover_size, home_showcase_mode,
    };

    #[test]
    fn home_compacts_width_bound_widgets() {
        assert_eq!(home_showcase_mode(359), HomeShowcaseMode::CoverOnly);
        assert_eq!(home_showcase_mode(360), HomeShowcaseMode::Compact);
        assert_eq!(home_showcase_mode(435), HomeShowcaseMode::Compact);
        assert_eq!(home_showcase_mode(640), HomeShowcaseMode::Full);
        assert_eq!(home_showcase_cover_size(359), HOME_SHOWCASE_MIN_COVER);
        assert_eq!(home_showcase_cover_size(360), HOME_SHOWCASE_MIN_COVER);
        assert_eq!(home_showcase_cover_size(435), HOME_SHOWCASE_MIN_COVER);
        assert_eq!(home_showcase_cover_size(444), HOME_SHOWCASE_MIN_COVER);
        assert_eq!(home_showcase_cover_size(520), 167);
        assert_eq!(home_showcase_cover_size(639), 195);
        assert_eq!(home_showcase_cover_size(640), HOME_SHOWCASE_FULL_COVER);
    }

    #[test]
    fn home_showcase_cover_tracks_width_without_resize_jumps() {
        let mut previous = home_showcase_cover_size(96);
        for width in 97..=800 {
            let size = home_showcase_cover_size(width);
            assert!(size >= previous);
            assert!(size - previous <= 1);
            previous = size;
        }
    }
}
