use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use library::{FavoriteItemId, MetadataItemId};
use localization::{msgid, tr};

const STAR_WIDTH: i32 = 95;
const STAR_HEIGHT: i32 = 20;

#[derive(Clone)]
pub(crate) struct RatingControl {
    root: gtk::Box,
    stars: Rc<[gtk::Image; 5]>,
    value: Rc<Cell<u8>>,
    preview: Rc<Cell<Option<u8>>>,
}

impl RatingControl {
    pub(crate) fn new(rating: Option<u8>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 1);
        root.set_homogeneous(true);
        root.set_size_request(STAR_WIDTH, STAR_HEIGHT);
        root.add_css_class("rating-stars");
        root.set_cursor_from_name(Some("pointer"));
        root.set_tooltip_text(Some(&tr(msgid("Rating"))));

        let value = Rc::new(Cell::new(rating.unwrap_or(0)));
        let preview = Rc::new(Cell::new(None));
        let stars = Rc::new(std::array::from_fn(|_| {
            let star = gtk::Image::new();
            star.add_css_class("rating-star");
            star.set_hexpand(true);
            star.set_halign(gtk::Align::Center);
            root.append(&star);
            star
        }));
        set_rating_icons(&stars, value.get());

        let hovered = Rc::clone(&preview);
        let hovered_stars = Rc::clone(&stars);
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(move |controller, x, _| {
            let rating = rating_at(x, widget_width(controller.widget()));
            hovered.set(Some(rating));
            set_rating_icons(&hovered_stars, rating);
        });
        let left = Rc::clone(&preview);
        let left_value = Rc::clone(&value);
        let left_stars = Rc::clone(&stars);
        motion.connect_leave(move |_| {
            left.set(None);
            set_rating_icons(&left_stars, left_value.get());
        });
        root.add_controller(motion);

        Self {
            root,
            stars,
            value,
            preview,
        }
    }

    pub(crate) fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub(crate) fn set_rating(&self, rating: Option<u8>) {
        self.value.set(rating.unwrap_or(0));
        if self.preview.get().is_none() {
            set_rating_icons(&self.stars, self.value.get());
        }
    }

    pub(crate) fn connect_commit(&self, commit: impl Fn(Option<u8>) + 'static) {
        let start = Rc::new(Cell::new(0.0));
        let drag = gtk::GestureDrag::new();
        let began_start = Rc::clone(&start);
        let began_preview = Rc::clone(&self.preview);
        let began_stars = Rc::clone(&self.stars);
        drag.connect_drag_begin(move |gesture, x, _| {
            began_start.set(x);
            preview_at(gesture.widget(), &began_preview, &began_stars, x);
        });
        let updated_start = Rc::clone(&start);
        let updated_preview = Rc::clone(&self.preview);
        let updated_stars = Rc::clone(&self.stars);
        drag.connect_drag_update(move |gesture, offset, _| {
            preview_at(
                gesture.widget(),
                &updated_preview,
                &updated_stars,
                updated_start.get() + offset,
            );
        });
        let value = Rc::clone(&self.value);
        let preview = Rc::clone(&self.preview);
        let committed_stars = Rc::clone(&self.stars);
        drag.connect_drag_end(move |gesture, offset, _| {
            let rating = rating_at(start.get() + offset, widget_width(gesture.widget()));
            value.set(rating);
            preview.set(Some(rating));
            set_rating_icons(&committed_stars, rating);
            commit(Some(rating));
        });
        self.root.add_controller(drag);
    }
}

fn preview_at(
    widget: Option<gtk::Widget>,
    preview: &Cell<Option<u8>>,
    stars: &[gtk::Image; 5],
    x: f64,
) {
    let rating = rating_at(x, widget_width(widget));
    preview.set(Some(rating));
    set_rating_icons(stars, rating);
}

fn widget_width(widget: Option<gtk::Widget>) -> i32 {
    widget.map_or(STAR_WIDTH, |widget| widget.width())
}

fn rating_at(x: f64, width: i32) -> u8 {
    ((x / f64::from(width.max(1)) * 10.0).ceil() as u8).clamp(1, 10)
}

fn set_rating_icons(stars: &[gtk::Image; 5], rating: u8) {
    let rating = rating.min(10);
    for (index, star) in stars.iter().enumerate() {
        let value = rating.saturating_sub(index as u8 * 2);
        if value == 0 {
            star.remove_css_class("rated");
        } else {
            star.add_css_class("rated");
        }
        star.set_icon_name(Some(match value {
            0 => "rufin-non-starred-symbolic",
            1 => "rufin-semi-starred-symbolic",
            _ => "rufin-starred-symbolic",
        }));
    }
}

pub(crate) fn context_rating_row(
    rating: Option<u8>,
    popover: &gtk::PopoverMenu,
    commit: impl Fn(Option<u8>) + 'static,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
    row.set_hexpand(true);
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    separator.set_margin_bottom(2);
    row.append(&separator);

    let control = RatingControl::new(rating);
    control.widget().set_hexpand(true);
    control.widget().set_halign(gtk::Align::Fill);
    control.widget().set_margin_start(10);
    control.widget().set_margin_end(10);
    control.widget().set_margin_top(2);
    control.widget().set_margin_bottom(2);
    let popover = popover.downgrade();
    control.connect_commit(move |rating| {
        if let Some(popover) = popover.upgrade() {
            crate::interactions::popdown_native_menu(&popover);
        }
        commit(rating);
    });
    row.append(control.widget());
    row
}

impl crate::shell::Shell {
    pub(crate) fn rating_available(&self, item: &FavoriteItemId) -> bool {
        let configured = self.source.configured.borrow();
        let Some(source) = configured
            .sources
            .iter()
            .find(|source| configured.selected_source_id.as_ref() == Some(&source.id))
        else {
            return false;
        };
        if source.kind != "local" {
            return true;
        }
        let FavoriteItemId::Track(track_id) = item else {
            return false;
        };
        self.metadata_editing_available(MetadataItemId::Track(track_id.clone()))
    }

    pub(crate) fn set_rating(&self, item: FavoriteItemId, rating: Option<u8>) {
        if let Some(source) = self.selected_source_operations() {
            source.set_rating(item, rating);
        }
    }

    pub(crate) fn set_current_track_rating(&self, rating: Option<u8>) {
        if let Some(track_id) = self
            .selected_playback()
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
            .map(|entry| entry.track.id.clone())
        {
            self.set_rating(FavoriteItemId::Track(track_id), rating);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rating_at;

    #[test]
    fn pointer_position_selects_half_stars() {
        assert_eq!(rating_at(9.5, 95), 1);
        assert_eq!(rating_at(19.0, 95), 2);
        assert_eq!(rating_at(47.5, 95), 5);
        assert_eq!(rating_at(57.0, 95), 6);
    }
}
