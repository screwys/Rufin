//! The Radio page: the station on air, then the selected source's stations and Rufin's
//! suggested ones as cards, with the Add Station and Edit Station dialogs.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use localization::{msgid, tr, tr_with};
use rufin_core::stations::{
    self, OnAir, RadioPage, Station, StationChannel, StationFound, StationInput, StationProbe,
};
use tracing::warn;

use crate::library::CatalogUi;
use crate::library::collections::library_route_inset;
use crate::library::detail_showcase::{
    DetailStrip, MetaLine, band_stops, open_web_link, paint_band,
};
use crate::library::route_layout::{PRIMARY_ROUTE_HORIZONTAL_INSET, ROUTE_TOP_MARGIN};
use crate::shell::shell::Shell;
use crate::widgets::artwork::ArtworkTile;
use crate::widgets::controls::{DELETE_ICON, EDIT_ICON};
use crate::widgets::interactions::ContextMenuSurface;
use crate::widgets::layout::width_allocation_owner;
use crate::widgets::library_fields::opaque_artwork;
use crate::widgets::mounted_route::{MountedRoute, MountedRouteSearchTarget};
use crate::widgets::reorder::{ChildReorder, install_child_reorder};

/// The narrowest card of the grid; narrower pages show one column.
const CARD_MIN_WIDTH: i32 = 320;
const CARD_GAP: i32 = 10;
const CARD_COVER_SIZE: i32 = 72;
/// The size the header's picture is rendered at, for any size it is shown at.
const HEADER_COVER_SIZE: i32 = 224;
/// From this width the header takes its large picture and Heard sits beside its text.
const WIDE_WIDTH: i32 = 700;
const HEARD_WIDTH: i32 = 340;
const HEARD_ROWS: usize = 3;
const RESULT_COVER_SIZE: i32 = 36;
const CHECK_ICON: &str = "rufin-object-select-symbolic";

crate::composite_box!(
    pub StationCardView,
    station_card_view_imp,
    "RufinStationCardView",
    "/app/rufin/rufin/ui/routes/radio_station_card.ui",
    {
        cover: gtk::Box,
        heading: gtk::Box,
        name: gtk::Label,
        manage: gtk::Box,
        drag: gtk::Image,
        pin: gtk::Button,
        edit: gtk::Button,
        delete: gtk::Button,
        add: gtk::Button,
        genres_row: gtk::Box,
        genres: gtk::Label,
        description: gtk::Label,
        channels: adw::WrapBox,
    }
);

/// Runs a station operation away from GTK and hands `done` its answer; its error shows as
/// other failed operations do, and `done` hears None.
fn run<T: Send + 'static>(
    shell: &Rc<Shell>,
    work: impl Future<Output = Result<T, String>> + Send + 'static,
    done: impl FnOnce(Option<T>) + 'static,
) {
    let task = shell.products.runtime.spawn(work);
    let shell = Rc::downgrade(shell);
    glib::spawn_future_local(async move {
        let result = match task.await {
            Ok(result) => result,
            Err(error) => Err(error.to_string()),
        };
        match result {
            Ok(value) => done(Some(value)),
            Err(error) => {
                if let Some(shell) = shell.upgrade() {
                    shell.control_feedback.show_feedback_toast(error);
                }
                done(None);
            }
        }
    });
}

/// A station's picture; one without a picture shows the radio icon on the fallback tile.
pub(crate) fn station_cover(shell: &Shell, binding: Option<&[u8]>, size: i32) -> ArtworkTile {
    let tile = ArtworkTile::new(size);
    tile.area.add_css_class("station-cover");
    shell
        .artwork
        .bind_artwork_tile(&tile, opaque_artwork(binding), size);
    tile
}

fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Marks `button` as `station`'s pin: whether the station or a channel of it is in Saved.
fn show_pin(button: &gtk::Button, station: &Station) {
    // The button of a station with channels opens their list, which removes one.
    let pin = tr(if station.pinned && station.channels.is_empty() {
        msgid("Remove from Saved")
    } else {
        msgid("Add to Saved")
    });
    button.set_tooltip_text(Some(&pin));
    button.update_property(&[gtk::accessible::Property::Label(&pin)]);
    let saved = station.pinned || station.channels.iter().any(|channel| channel.pinned);
    button.set_icon_name(if saved {
        "rufin-bookmark-filled-symbolic"
    } else {
        "rufin-bookmark-symbolic"
    });
    crate::widgets::controls::set_active_class(button, saved);
}

/// Adds `station` to Saved or takes it out; one with channels opens their list from `button`.
fn pin_station(shell: &Rc<Shell>, button: &gtk::Button, station: Station) {
    if station.channels.is_empty() {
        if let Some(pin) = station.pin {
            shell.set_sidebar_pin(pin, !station.pinned);
        }
        return;
    }
    // Each channel is saved by itself.
    let surface = ContextMenuSurface::new(button.upcast_ref(), "station-channels", None);
    for channel in station.channels {
        let Some(pin) = channel.pin else {
            continue;
        };
        let shell = Rc::downgrade(shell);
        surface.append_fixed_choice(&channel.name, channel.pinned, move || {
            if let Some(shell) = shell.upgrade() {
                shell.set_sidebar_pin(pin.clone(), !channel.pinned);
            }
        });
    }
    surface.popup();
}

/// Plays a station, alone in the queue: an added one by its key, a suggested one by its id
/// in Rufin's list, and its channel `channel` when one is named.
pub(crate) fn play_station(
    shell: &Rc<Shell>,
    key: Option<i64>,
    listed: Option<String>,
    channel: Option<String>,
) {
    let (source, playback) = (
        shell.products.source.clone(),
        shell.products.playback.clone(),
    );
    run(
        shell,
        async move { stations::play(&source, &playback, key, listed, channel).await },
        |_| {},
    );
}

/// Whether two reads show the same card, apart from what plays and what is pinned.
fn same_card(shown: &Station, read: &Station) -> bool {
    let still = |station: &Station| Station {
        playing: false,
        pinned: false,
        channels: station
            .channels
            .iter()
            .map(|channel| StationChannel {
                playing: false,
                pinned: false,
                ..channel.clone()
            })
            .collect(),
        ..station.clone()
    };
    still(shown) == still(read)
}

/// A row of Heard: when a song was announced, its title and its artist, on one line.
struct HeardSlot {
    row: gtk::Box,
    time: gtk::Label,
    title: gtk::Label,
    artist: gtk::Label,
}

impl HeardSlot {
    fn new() -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.set_visible(false);
        let time = gtk::Label::new(None);
        time.add_css_class("radio-heard-time");
        time.add_css_class("numeric");
        time.set_xalign(0.0);
        time.set_width_request(40);
        row.append(&time);
        let line = |class: &str| {
            let label = gtk::Label::new(None);
            label.add_css_class(class);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_width_chars(1);
            row.append(&label);
            label
        };
        let (title, artist) = (line("radio-heard-title"), line("radio-heard-artist"));
        // The artist gives way first; what is left of the row is its.
        artist.set_hexpand(true);
        Self {
            row,
            time,
            title,
            artist,
        }
    }

    /// Shows `song`, or hides the row without one.
    fn show(&self, song: Option<&stations::HeardSong>) {
        self.row.set_visible(song.is_some());
        let Some(song) = song else {
            return;
        };
        let at = glib::DateTime::from_unix_local(song.at_unix_seconds)
            .and_then(|at| at.format("%H:%M"))
            .map(String::from)
            .unwrap_or_default();
        self.time.set_text(&at);
        self.title.set_text(&song.title);
        self.artist.set_text(&song.artist);
    }
}

/// The station on air, as one card in its picture's colours: the picture, with its name and
/// the playing channel, the announced song, the facts, the genres and the buttons beside it,
/// and the songs heard before under them, or at their side when the card is wide.
struct OnAirHeader {
    root: gtk::Overlay,
    band: gtk::DrawingArea,
    stops: Rc<Cell<[[u8; 3]; 3]>>,
    body: gtk::Box,
    top: gtk::Box,
    cover: ArtworkTile,
    /// The picture the cover shows; None before the first station.
    picture: RefCell<Option<Option<Vec<u8>>>>,
    name: gtk::Label,
    channel: gtk::Label,
    song: gtk::Label,
    artist: gtk::Label,
    format_fact: gtk::Box,
    format: gtk::Label,
    /// The facts and the genres as one line.
    meta: gtk::Widget,
    genres: Rc<MetaLine>,
    genre_names: RefCell<Vec<String>>,
    toggle_icon: gtk::Image,
    toggle_text: gtk::Label,
    /// The station's name with the toggle, at the page's top once the header has scrolled away.
    strip: DetailStrip,
    pin: gtk::Button,
    edit: gtk::Button,
    homepage: gtk::Button,
    homepage_url: Rc<RefCell<String>>,
    /// The station the header shows, which its pin and Edit act on.
    station: Rc<RefCell<Option<Station>>>,
    /// The station Play plays, when the header shows one that is not in the queue.
    tuned: Rc<RefCell<Option<Station>>>,
    heard: gtk::Box,
    slots: Vec<HeardSlot>,
    /// The width last laid out.
    width: Cell<i32>,
}

impl OnAirHeader {
    fn no_colours() -> [[u8; 3]; 3] {
        rufin_core::artwork::showcase_band(&[], 0, 0, 0)
    }

    /// The card's sizes at `width`. Wide, the picture is large and Heard is a column at the
    /// text's side; narrow, Heard takes the card's width under the picture and the text.
    fn lay_out(&self, width: i32) {
        if width <= 1 || self.width.replace(width) == width {
            return;
        }
        let wide = width >= WIDE_WIDTH;
        let (inset, gap, cover) = if wide {
            (20, 24, 164)
        } else {
            (16, 16, (width * 34 / 100).clamp(120, 150))
        };
        set_class(&self.root, "radio-on-air-wide", wide);
        self.cover.set_square_size(cover);
        self.top.set_spacing(gap);
        self.body.set_margin_top(inset);
        self.body.set_margin_bottom(inset);
        self.body.set_margin_start(inset);
        self.body.set_margin_end(inset);
        self.body.set_orientation(if wide {
            gtk::Orientation::Horizontal
        } else {
            gtk::Orientation::Vertical
        });
        self.body.set_spacing(if wide { gap } else { 0 });
        self.meta.set_margin_top(if wide { 12 } else { 8 });
        self.heard.set_valign(if wide {
            gtk::Align::Center
        } else {
            gtk::Align::Fill
        });
        self.heard
            .set_width_request(if wide { HEARD_WIDTH } else { -1 });
        set_class(&self.heard, "radio-heard-side", wide);
        set_class(&self.heard, "radio-heard-below", !wide);
    }

    fn show(&self, shell: &Shell, on_air: &OnAir) {
        let station = &on_air.station;
        let picture = Some(station.artwork_binding.clone());
        if *self.picture.borrow() != picture {
            self.picture.replace(picture);
            // A picture brings its colours when it arrives; until then the band keeps its own.
            if station.artwork_binding.is_none() {
                self.stops.set(Self::no_colours());
                self.band.queue_draw();
                self.strip.band.queue_draw();
            }
            shell.artwork.bind_artwork_tile(
                &self.cover,
                opaque_artwork(station.artwork_binding.as_deref()),
                HEADER_COVER_SIZE,
            );
        }
        self.name.set_text(&station.name);
        self.strip.title.set_text(&station.name);
        self.channel
            .set_text(on_air.channel.as_deref().unwrap_or_default());
        self.channel.set_visible(on_air.channel.is_some());
        for (label, text) in [(&self.song, &on_air.title), (&self.artist, &on_air.artist)] {
            let text = text.as_deref().unwrap_or_default();
            label.set_text(text);
            label.set_visible(!text.is_empty());
        }
        let format = on_air.format.as_deref().unwrap_or_default();
        self.format.set_text(format);
        self.format_fact.set_visible(!format.is_empty());
        self.show_genres(&station.genres);
        let (icon, label) = if on_air.playing {
            ("rufin-media-playback-pause-symbolic", tr(msgid("Pause")))
        } else {
            (crate::widgets::controls::PLAY_ICON, tr(msgid("Play")))
        };
        for (image, text) in [
            (&self.toggle_icon, &self.toggle_text),
            (&self.strip.play_icon, &self.strip.play_text),
        ] {
            image.set_icon_name(Some(icon));
            text.set_text(&label);
        }
        self.homepage.set_visible(!station.homepage.is_empty());
        self.homepage_url.replace(station.homepage.clone());
        // An added station has its card's pin and Edit; a suggested one has neither.
        self.pin
            .set_visible(station.key.is_some() && station.pin.is_some());
        self.edit.set_visible(station.key.is_some());
        show_pin(&self.pin, station);
        self.station.replace(Some(station.clone()));
        // The song that plays is beside the picture; Heard lists the ones before it.
        let now = on_air
            .heard
            .first()
            .is_some_and(|song| on_air.title.as_deref() == Some(song.title.as_str()));
        let mut earlier = on_air.heard.iter().skip(usize::from(now));
        for slot in &self.slots {
            slot.show(earlier.next());
        }
        self.heard
            .set_visible(on_air.heard.len() > usize::from(now));
        self.tuned.replace((!on_air.tuned).then(|| station.clone()));
        self.root.set_visible(true);
    }

    /// The genres as the showcase's chips. They lead nowhere, so they take no pointer.
    fn show_genres(&self, genres: &[String]) {
        if *self.genre_names.borrow() == genres {
            return;
        }
        self.genre_names.replace(genres.to_vec());
        self.genres.set_chips(genres.iter().map(|genre| {
            let chip = gtk::Button::with_label(genre);
            chip.add_css_class("flat");
            chip.add_css_class("detail-genre-chip");
            chip.add_css_class("detail-genre-static");
            chip.set_valign(gtk::Align::Center);
            chip.set_can_target(false);
            chip.set_focusable(false);
            chip.upcast()
        }));
    }
}

/// A station's card in a grid.
struct StationCard {
    child: gtk::FlowBoxChild,
    name: gtk::Label,
    pin: gtk::Button,
    station: Rc<RefCell<Station>>,
    /// The chips of the station's channels.
    chips: Vec<(String, gtk::Button)>,
}

impl StationCard {
    /// Marks the card and the chip of the channel that plays, and whether the station or a
    /// channel of it is pinned.
    fn show_playing(&self, station: &Station) {
        set_class(&self.name, "track-row-playing", station.playing);
        show_pin(&self.pin, station);
        for (name, chip) in &self.chips {
            let playing = station
                .channels
                .iter()
                .any(|channel| channel.playing && &channel.name == name);
            set_class(chip, "station-chip-playing", playing);
        }
        self.station.replace(station.clone());
    }
}

/// The mounted Radio page.
struct RadioRoute {
    shell: Weak<Shell>,
    root: gtk::Widget,
    on_air: OnAirHeader,
    stations: gtk::FlowBox,
    suggested_heading: gtk::Label,
    suggested: gtk::FlowBox,
    filter: gtk::SearchEntry,
    filter_toggle: gtk::ToggleButton,
    station_cards: RefCell<Vec<StationCard>>,
    suggested_cards: RefCell<Vec<StationCard>>,
    /// The stations' cards in the order the grid shows them, which a drag changes.
    order: ChildReorder,
    shown: RefCell<Option<RadioPage>>,
    /// The newest read; an older one that answers later is dropped.
    read: Cell<u64>,
    columns: Cell<u32>,
}

impl RadioRoute {
    fn new(catalog: &Rc<CatalogUi>) -> Rc<Self> {
        let shell = catalog.shell();
        let resource = "routes/radio.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            content: gtk::Box,
            on_air: gtk::Overlay,
            band: gtk::DrawingArea,
            on_air_body: gtk::Box,
            on_air_cover: gtk::Box,
            on_air_name: gtk::Label,
            on_air_channel: gtk::Label,
            on_air_song: gtk::Label,
            on_air_artist: gtk::Label,
            on_air_format_fact: gtk::Box,
            on_air_format: gtk::Label,
            on_air_meta: crate::widgets::layout::AllocationOwner,
            on_air_meta_line: adw::WrapBox,
            on_air_facts: gtk::Box,
            on_air_genre_more: gtk::Button,
            on_air_top: gtk::Box,
            on_air_toggle: gtk::Button,
            on_air_toggle_icon: gtk::Image,
            on_air_toggle_text: gtk::Label,
            on_air_pin: gtk::Button,
            on_air_edit: gtk::Button,
            on_air_homepage: gtk::Button,
            heard: gtk::Box,
            heard_rows: gtk::Box,
            filter: gtk::SearchEntry,
            add: gtk::Button,
            filter_toggle: gtk::ToggleButton,
                    stations: gtk::FlowBox,
            suggested_heading: gtk::Label,
            suggested: gtk::FlowBox,
        });
        crate::widgets::controls::configure_search_entry(&filter);
        content.set_margin_top(ROUTE_TOP_MARGIN);

        let cover = ArtworkTile::new(HEADER_COVER_SIZE);
        cover.area.add_css_class("station-cover");
        cover.area.add_css_class("detail-showcase-cover");
        cover.set_animated(true);
        on_air_cover.append(&cover.area);
        let stops = Rc::new(Cell::new(OnAirHeader::no_colours()));
        let painted = Rc::clone(&stops);
        band.set_draw_func(move |_, cairo, width, height| {
            paint_band(cairo, f64::from(width), f64::from(height), painted.get());
        });
        let strip = DetailStrip::new(
            {
                let stops = Rc::clone(&stops);
                move || Some(stops.get())
            },
            &on_air_toggle,
        );
        let (taken, bands) = (
            Rc::clone(&stops),
            [band.downgrade(), strip.band.downgrade()],
        );
        cover.connect_texture(Rc::new(move |texture| {
            let stops = band_stops(texture);
            if taken.replace(stops) != stops {
                for band in bands.iter().filter_map(glib::WeakRef::upgrade) {
                    band.queue_draw();
                }
            }
        }));
        let homepage_url = Rc::new(RefCell::new(String::new()));
        let (url, window) = (Rc::clone(&homepage_url), shell.chrome.window.downgrade());
        on_air_homepage.connect_clicked(move |_| {
            if let Some(window) = window.upgrade() {
                open_web_link(&window, &url.borrow());
            }
        });

        on_air_edit.set_icon_name(EDIT_ICON);
        let on_air_station = Rc::new(RefCell::new(None::<Station>));
        let (pin_shell, pinned) = (Rc::downgrade(shell), Rc::clone(&on_air_station));
        on_air_pin.connect_clicked(move |button| {
            let station = pinned.borrow().clone();
            if let (Some(shell), Some(station)) = (pin_shell.upgrade(), station) {
                pin_station(&shell, button, station);
            }
        });
        let (edit_shell, edited) = (Rc::downgrade(shell), Rc::clone(&on_air_station));
        on_air_edit.connect_clicked(move |_| {
            let station = edited.borrow().clone();
            if let (Some(shell), Some(station)) = (edit_shell.upgrade(), station) {
                present_edit_dialog(&shell, station);
            }
        });

        let slots = (0..HEARD_ROWS)
            .map(|_| {
                let slot = HeardSlot::new();
                heard_rows.append(&slot.row);
                slot
            })
            .collect::<Vec<_>>();

        let route = Rc::new_cyclic(|route: &Weak<Self>| {
            let order = Self::install_reorder(route, &stations);
            let weak = route.clone();
            let page = width_allocation_owner(
                &library_route_inset(content.clone().upcast()),
                move |width| {
                    if let Some(route) = weak.upgrade() {
                        route.lay_out(width - PRIMARY_ROUTE_HORIZONTAL_INSET);
                    }
                },
            );
            Self {
                shell: Rc::downgrade(shell),
                // The page scrolls the station on air away; its strip then stays at the top.
                root: super::detail_scroll::detail_scroll(&page, &on_air, &strip.root, None),
                on_air: OnAirHeader {
                    root: on_air,
                    band,
                    stops,
                    body: on_air_body,
                    cover,
                    picture: RefCell::new(None),
                    name: on_air_name,
                    channel: on_air_channel,
                    song: on_air_song,
                    artist: on_air_artist,
                    format_fact: on_air_format_fact,
                    format: on_air_format,
                    genres: MetaLine::bind(
                        &on_air_meta,
                        &on_air_meta_line,
                        &on_air_facts,
                        &on_air_genre_more,
                        || {},
                    ),
                    meta: on_air_meta.upcast(),
                    genre_names: RefCell::new(Vec::new()),
                    top: on_air_top,
                    toggle_icon: on_air_toggle_icon,
                    toggle_text: on_air_toggle_text,
                    strip,
                    pin: on_air_pin,
                    edit: on_air_edit,
                    homepage: on_air_homepage,
                    homepage_url,
                    station: on_air_station,
                    tuned: Rc::new(RefCell::new(None)),
                    heard,
                    slots,
                    width: Cell::new(0),
                },
                stations,
                suggested_heading,
                suggested,
                filter,
                filter_toggle,
                station_cards: RefCell::new(Vec::new()),
                suggested_cards: RefCell::new(Vec::new()),
                order,
                shown: RefCell::new(None),
                read: Cell::new(0),
                columns: Cell::new(0),
            }
        });
        route.lay_out(catalog.route_width() - PRIMARY_ROUTE_HORIZONTAL_INSET);
        let weak = Rc::downgrade(&route);
        on_air_toggle.connect_clicked(move |_| {
            let Some(route) = weak.upgrade() else {
                return;
            };
            let ready = route.on_air.tuned.borrow().clone();
            match ready {
                Some(station) => route.play(&station, None),
                None => {
                    if let Some(shell) = route.shell.upgrade() {
                        shell.products.playback.play_pause();
                    }
                }
            }
        });

        for (grid, suggested) in [(&route.stations, false), (&route.suggested, true)] {
            let weak = Rc::downgrade(&route);
            grid.connect_child_activated(move |_, child| {
                let Some(route) = weak.upgrade() else {
                    return;
                };
                let cards = if suggested {
                    route.suggested_cards.borrow()
                } else {
                    route.station_cards.borrow()
                };
                let station = cards
                    .iter()
                    .find(|card| card.child == *child)
                    .map(|card| card.station.borrow().clone());
                drop(cards);
                if let Some(station) = station {
                    route.play(&station, None);
                }
            });
        }
        let weak = Rc::downgrade(&route);
        add.connect_clicked(move |_| {
            if let Some(route) = weak.upgrade()
                && let Some(shell) = route.shell.upgrade()
            {
                let saved_on = route
                    .shown
                    .borrow()
                    .as_ref()
                    .and_then(|page| page.saved_on.clone());
                present_add_dialog(&shell, saved_on);
            }
        });
        let search = route.filter.downgrade();
        route.filter_toggle.connect_toggled(move |toggle| {
            let Some(search) = search.upgrade() else {
                return;
            };
            search.set_visible(toggle.is_active());
            if toggle.is_active() {
                search.grab_focus();
            } else {
                search.set_text("");
            }
        });
        let toggle = route.filter_toggle.downgrade();
        route.filter.connect_stop_search(move |_| {
            if let Some(toggle) = toggle.upgrade() {
                toggle.set_active(false);
            }
        });
        let weak = Rc::downgrade(&route);
        route.filter.connect_search_changed(move |_| {
            if let Some(route) = weak.upgrade() {
                route.apply_filter();
            }
        });
        route
    }

    /// The page's shape at `width`: the header's, and as many columns of cards as fit.
    fn lay_out(&self, width: i32) {
        self.on_air.lay_out(width);
        let columns = ((width + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).max(1) as u32;
        let previous = self.columns.replace(columns);
        if previous == columns {
            return;
        }
        for grid in [&self.stations, &self.suggested] {
            if columns > previous {
                grid.set_max_children_per_line(columns);
                grid.set_min_children_per_line(columns);
            } else {
                grid.set_min_children_per_line(columns);
                grid.set_max_children_per_line(columns);
            }
        }
    }

    /// Reads the page again and shows it.
    fn reread(self: &Rc<Self>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let read = self.read.get() + 1;
        self.read.set(read);
        let (source, playback) = (
            shell.products.source.clone(),
            shell.products.playback.clone(),
        );
        let route = Rc::downgrade(self);
        run(
            &shell,
            async move { stations::page(&source, &playback).await },
            move |page| {
                if let (Some(route), Some(page)) = (route.upgrade(), page)
                    && route.read.get() == read
                {
                    route.show(page);
                }
            },
        );
    }

    fn show(self: &Rc<Self>, page: RadioPage) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        match &page.on_air {
            Some(on_air) => self.on_air.show(&shell, on_air),
            None => self.on_air.root.set_visible(false),
        }
        // The cards name where a station is saved, so another place builds them again.
        let moved = self
            .shown
            .borrow()
            .as_ref()
            .is_none_or(|shown| shown.saved_on != page.saved_on);
        for (grid, cards, stations) in [
            (&self.stations, &self.station_cards, &page.stations),
            (&self.suggested, &self.suggested_cards, &page.suggested),
        ] {
            let same = !moved && {
                let cards = cards.borrow();
                cards.len() == stations.len()
                    && cards
                        .iter()
                        .zip(stations)
                        .all(|(card, station)| same_card(&card.station.borrow(), station))
            };
            if same {
                for (card, station) in cards.borrow().iter().zip(stations) {
                    card.show_playing(station);
                }
                continue;
            }
            grid.remove_all();
            let built = stations
                .iter()
                .map(|station| {
                    let card = self.card(&shell, station, page.saved_on.as_deref());
                    grid.append(&card.child);
                    card
                })
                .collect::<Vec<StationCard>>();
            if grid == &self.stations {
                self.order.set_order(
                    built
                        .iter()
                        .map(|card| card.child.clone().upcast())
                        .collect(),
                );
            }
            cards.replace(built);
        }
        self.stations.set_visible(!page.stations.is_empty());
        self.shown.replace(Some(page));
        self.apply_filter();
    }

    /// Shows the cards whose station's name holds the search text, in both sections.
    fn apply_filter(&self) {
        let query = self.filter.text().trim().to_lowercase();
        let filter = |cards: &RefCell<Vec<StationCard>>| {
            let mut shown = false;
            for card in cards.borrow().iter() {
                let matches =
                    query.is_empty() || card.station.borrow().name.to_lowercase().contains(&query);
                card.child.set_visible(matches);
                shown |= matches;
            }
            shown
        };
        filter(&self.station_cards);
        let suggested = filter(&self.suggested_cards);
        self.suggested_heading.set_visible(suggested);
        self.suggested.set_visible(suggested);
    }

    /// Lets a station be dragged by its handle to another's place. The card moves among the
    /// others while it is dragged, and the order is kept when it is let go.
    fn install_reorder(route: &Weak<Self>, stations: &gtk::FlowBox) -> ChildReorder {
        let weak = route.clone();
        stations.set_sort_func(move |a, b| {
            let Some(route) = weak.upgrade() else {
                return gtk::Ordering::Equal;
            };
            route.order.position(a).cmp(&route.order.position(b)).into()
        });
        let weak = route.clone();
        install_child_reorder(
            stations,
            |stations, _| stations.invalidate_sort(),
            move |_, order| {
                let Some(route) = weak.upgrade() else {
                    return;
                };
                let (Some(shell), cards) = (route.shell.upgrade(), route.station_cards.borrow())
                else {
                    return;
                };
                let key_of = |child: &gtk::Widget| {
                    cards
                        .iter()
                        .find(|card| card.child.upcast_ref::<gtk::Widget>() == child)
                        .and_then(|card| card.station.borrow().key)
                };
                let shown = cards
                    .iter()
                    .filter_map(|card| key_of(card.child.upcast_ref()));
                let keys = order.iter().filter_map(key_of).collect::<Vec<_>>();
                if keys.iter().copied().eq(shown) {
                    return;
                }
                let source = shell.products.source.clone();
                run(
                    &shell,
                    async move { stations::reorder(&source, keys).await },
                    |_| {},
                );
            },
        )
    }

    /// Plays `station`, or its channel `channel`.
    fn play(&self, station: &Station, channel: Option<String>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let key = station.key;
        let listed = station.listed.clone().filter(|_| key.is_none());
        play_station(&shell, key, listed, channel);
    }

    fn card(
        self: &Rc<Self>,
        shell: &Rc<Shell>,
        station: &Station,
        saved_on: Option<&str>,
    ) -> StationCard {
        let view = StationCardView::new();
        let imp = view.imp();
        imp.cover.append(
            &station_cover(shell, station.artwork_binding.as_deref(), CARD_COVER_SIZE).area,
        );
        imp.name.set_text(&station.name);
        let genres = station.genres.join(", ");
        imp.genres.set_text(&genres);
        imp.genres_row.set_visible(!genres.is_empty());
        imp.description.set_text(&station.description);
        imp.description.set_visible(!station.description.is_empty());

        let shown = Rc::new(RefCell::new(station.clone()));
        let chips = station
            .channels
            .iter()
            .map(|channel| {
                let chip = gtk::Button::with_label(&channel.name);
                chip.add_css_class("flat");
                chip.add_css_class("detail-genre-chip");
                let (route, station) = (Rc::downgrade(self), Rc::clone(&shown));
                let name = channel.name.clone();
                chip.connect_clicked(move |_| {
                    if let Some(route) = route.upgrade() {
                        let station = station.borrow().clone();
                        route.play(&station, Some(name.clone()));
                    }
                });
                imp.channels.append(&chip);
                (channel.name.clone(), chip)
            })
            .collect::<Vec<_>>();
        imp.channels.set_visible(!chips.is_empty());

        if station.key.is_some() {
            imp.edit.set_icon_name(EDIT_ICON);
            imp.delete.set_icon_name(DELETE_ICON);
            // Without Saved in the sidebar a station has no pin.
            imp.pin.set_visible(station.pin.is_some());
            let (pin_shell, pinned) = (Rc::downgrade(shell), Rc::clone(&shown));
            imp.pin.connect_clicked(move |button| {
                if let Some(shell) = pin_shell.upgrade() {
                    pin_station(&shell, button, pinned.borrow().clone());
                }
            });
            let (edit_shell, edited) = (Rc::downgrade(shell), Rc::clone(&shown));
            imp.edit.connect_clicked(move |_| {
                if let Some(shell) = edit_shell.upgrade() {
                    present_edit_dialog(&shell, edited.borrow().clone());
                }
            });
            let (delete_shell, deleted) = (Rc::downgrade(shell), Rc::clone(&shown));
            imp.delete.connect_clicked(move |_| {
                if let Some(shell) = delete_shell.upgrade() {
                    delete_station(&shell, &deleted.borrow());
                }
            });
            imp.manage.set_visible(true);
        } else if let Some(listed) = station.listed.clone() {
            let tooltip = match saved_on {
                Some(server) => tr_with("Add to {saved_on}", &[("saved_on", server)]),
                None => tr(msgid("Add")),
            };
            imp.add.set_tooltip_text(Some(&tooltip));
            imp.add
                .update_property(&[gtk::accessible::Property::Label(&tooltip)]);
            let shell = Rc::downgrade(shell);
            imp.add.connect_clicked(move |button| {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                button.set_sensitive(false);
                let (source, listed) = (shell.products.source.clone(), listed.clone());
                let button = button.downgrade();
                run(
                    &shell,
                    async move { stations::add_listed(&source, listed).await },
                    move |added| {
                        // An added station leaves Suggested with the page's next read.
                        if let (None, Some(button)) = (added, button.upgrade()) {
                            button.set_sensitive(true);
                        }
                    },
                );
            });
            imp.add.set_visible(true);
        }

        let child = gtk::FlowBoxChild::new();
        child.set_child(Some(&view));
        let card = StationCard {
            child,
            name: imp.name.get(),
            pin: imp.pin.get(),
            station: shown,
            chips,
        };
        card.show_playing(station);
        card
    }
}

/// Deletes an added station. One of Rufin's list comes back from Suggested; any other is asked
/// about first.
fn delete_station(shell: &Rc<Shell>, station: &Station) {
    let Some(key) = station.key else {
        return;
    };
    let weak = Rc::downgrade(shell);
    let delete = move || {
        if let Some(shell) = weak.upgrade() {
            let source = shell.products.source.clone();
            run(
                &shell,
                async move { stations::delete(&source, key).await },
                |_| {},
            );
        }
    };
    if station.listed.is_some() {
        delete();
        return;
    }
    let dialog = crate::playlists::delete_playlist_dialog(&station.name, delete);
    dialog.set_heading(Some(&tr(msgid("Delete Station"))));
    dialog.present(Some(&shell.chrome.window));
}

impl CatalogUi {
    /// The Radio page. It reads `stations::page` again when the stations or what plays
    /// change.
    pub fn radio_route(self: &Rc<Self>) -> MountedRoute {
        let route = RadioRoute::new(self);
        let source = self.products.source.clone();
        self.products.runtime.spawn(async move {
            // The stored stations still list and play when the server does not answer.
            if let Err(error) = stations::refresh(&source).await {
                warn!(%error, "could not read the server's radio stations");
            }
        });
        route.reread();
        let (filter, toggle) = (route.filter.clone(), route.filter_toggle.clone());
        let widget = route.root.clone();
        MountedRoute::new(widget, Rc::new(|| {}))
            .with_catalog_refresh(Rc::new(move || route.reread()))
            .with_search_provider(Rc::new(move || {
                let (filter, toggle) = (filter.clone(), toggle.clone());
                Some(MountedRouteSearchTarget {
                    search: filter.clone(),
                    focus: Some(Rc::new(move || {
                        toggle.set_active(true);
                        filter.grab_focus();
                    })),
                })
            }))
    }
}

/// A found station that is added already: a mark, not a button.
fn added_mark() -> gtk::Box {
    let mark = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    mark.add_css_class("station-added");
    mark.set_valign(gtk::Align::Center);
    mark.append(&gtk::Image::from_icon_name(CHECK_ICON));
    mark.append(&gtk::Label::new(Some(&tr(msgid("Added")))));
    mark
}

/// Add Station: its field searches radio-browser.info by name as it is typed, and reads a
/// stream address.
struct AddStation {
    shell: Weak<Shell>,
    dialog: glib::WeakRef<adw::Dialog>,
    entry: gtk::SearchEntry,
    results: gtk::ListBox,
    results_note: gtk::Label,
    probed: gtk::ListBox,
    stream: adw::ActionRow,
    name: adw::EntryRow,
    homepage: adw::EntryRow,
    message: gtk::Label,
    add: gtk::Button,
    /// The newest search or read; an older one that answers later is dropped.
    request: Cell<u64>,
    /// The stream the entered address leads to.
    stream_url: RefCell<String>,
}

impl AddStation {
    fn show_message(&self, text: &str) {
        self.results.set_visible(false);
        self.results_note.set_visible(false);
        self.probed.set_visible(false);
        self.add.set_visible(false);
        self.message.set_text(text);
        self.message.set_visible(!text.is_empty());
    }

    /// Searches for the entered name, or reads the entered address.
    fn entered(self: &Rc<Self>) {
        let request = self.request.get() + 1;
        self.request.set(request);
        let text = self.entry.text().trim().to_string();
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if text.is_empty() {
            self.show_message("");
            return;
        }
        // The fields of an address read before must not add another station.
        self.probed.set_visible(false);
        self.add.set_visible(false);
        let address = ["http://", "https://"]
            .iter()
            .any(|scheme| text.to_ascii_lowercase().starts_with(scheme));
        let weak = Rc::downgrade(self);
        let answered = move || weak.upgrade().filter(|add| add.request.get() == request);
        if address {
            self.show_message("");
            let task = shell
                .products
                .runtime
                .spawn(async move { stations::probe(text).await });
            glib::spawn_future_local(async move {
                let result = task.await;
                let Some(add) = answered() else {
                    return;
                };
                match result {
                    Ok(Ok(probe)) => add.show_probe(&probe),
                    Ok(Err(error)) => add.show_message(&error),
                    Err(error) => add.show_message(&error.to_string()),
                }
            });
        } else {
            let source = shell.products.source.clone();
            let task = shell
                .products
                .runtime
                .spawn(async move { stations::search(&source, text).await });
            glib::spawn_future_local(async move {
                let result = task.await;
                let Some(add) = answered() else {
                    return;
                };
                match result {
                    Ok(Ok(found)) if found.is_empty() => add.show_message(&tr("No results")),
                    Ok(Ok(found)) => add.show_results(&found),
                    Ok(Err(error)) => add.show_message(&error),
                    Err(error) => add.show_message(&error.to_string()),
                }
            });
        }
    }

    fn show_results(self: &Rc<Self>, found: &[StationFound]) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.show_message("");
        self.results.remove_all();
        for station in found {
            let row = adw::ActionRow::builder()
                .use_markup(false)
                .title(&station.name)
                .subtitle(&station.detail)
                .title_lines(1)
                .subtitle_lines(1)
                .activatable(false)
                .build();
            let cover = station_cover(
                &shell,
                station.artwork_binding.as_deref(),
                RESULT_COVER_SIZE,
            );
            cover.area.set_valign(gtk::Align::Center);
            row.add_prefix(&cover.area);
            if station.added {
                row.add_suffix(&added_mark());
            } else {
                let button = gtk::Button::with_label(&tr(msgid("Add")));
                button.set_valign(gtk::Align::Center);
                let input = StationInput {
                    name: station.name.clone(),
                    stream_url: station.stream_url.clone(),
                    homepage: station.homepage.clone(),
                    logo: station.logo.clone(),
                };
                let (shell, weak_row) = (Rc::downgrade(&shell), row.downgrade());
                button.connect_clicked(move |button| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    button.set_sensitive(false);
                    let (source, input) = (shell.products.source.clone(), input.clone());
                    let (button, row) = (button.downgrade(), weak_row.clone());
                    run(
                        &shell,
                        async move { stations::add(&source, input).await },
                        move |added| {
                            let (Some(button), Some(row)) = (button.upgrade(), row.upgrade())
                            else {
                                return;
                            };
                            if added.is_some() {
                                row.remove(&button);
                                row.add_suffix(&added_mark());
                            } else {
                                button.set_sensitive(true);
                            }
                        },
                    );
                });
                row.add_suffix(&button);
            }
            self.results.append(&row);
        }
        self.results.set_visible(true);
        self.results_note.set_visible(true);
    }

    fn show_probe(&self, probe: &StationProbe) {
        self.show_message("");
        self.stream.set_title(&probe.name);
        self.stream.set_subtitle(
            &[probe.format.as_str(), probe.host.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
        );
        self.name.set_text(&probe.name);
        self.homepage.set_text("");
        self.stream_url.replace(probe.stream_url.clone());
        self.probed.set_visible(true);
        self.add.set_visible(true);
    }

    /// Adds the stream the address leads to, under the name and homepage entered for it.
    fn add_probed(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let input = StationInput {
            name: self.name.text().trim().to_string(),
            stream_url: self.stream_url.borrow().clone(),
            homepage: self.homepage.text().trim().to_string(),
            logo: String::new(),
        };
        self.add.set_sensitive(false);
        let source = shell.products.source.clone();
        let (dialog, add) = (self.dialog.clone(), self.add.downgrade());
        run(
            &shell,
            async move { stations::add(&source, input).await },
            move |added| {
                if let Some(add) = add.upgrade() {
                    add.set_sensitive(true);
                }
                if let (Some(()), Some(dialog)) = (added, dialog.upgrade()) {
                    dialog.close();
                }
            },
        );
    }
}

/// Presents Add Station; `saved_on` names the server its stations are saved on.
fn present_add_dialog(shell: &Rc<Shell>, saved_on: Option<String>) {
    let resource = "routes/radio_add.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        dialog: adw::Dialog,
        toolbar: adw::ToolbarView,
        header: adw::HeaderBar,
        entry: gtk::SearchEntry,
        results: gtk::ListBox,
        results_note: gtk::Label,
        probed: gtk::ListBox,
        stream: adw::ActionRow,
        name: adw::EntryRow,
        homepage: adw::EntryRow,
        message: gtk::Label,
        saved: gtk::Label,
        add: gtk::Button,
    });
    crate::widgets::controls::configure_dialog_header(&header);
    // The dialog follows its content's height, at the width it would have without results.
    dialog.set_child(None::<&gtk::Widget>);
    dialog.set_child(Some(&crate::widgets::layout::preferred_width_owner(
        &toolbar, 440,
    )));
    crate::widgets::controls::configure_search_entry(&entry);
    let cover = station_cover(shell, None, RESULT_COVER_SIZE);
    cover.area.set_valign(gtk::Align::Center);
    stream.add_prefix(&cover.area);
    if let Some(server) = &saved_on {
        saved.set_text(&tr_with(
            "Stations are saved on {saved_on}.",
            &[("saved_on", server.as_str())],
        ));
        saved.set_visible(true);
    }
    let add_station = Rc::new(AddStation {
        shell: Rc::downgrade(shell),
        dialog: dialog.downgrade(),
        entry,
        results,
        results_note,
        probed,
        stream,
        name,
        homepage,
        message,
        add,
        request: Cell::new(0),
        stream_url: RefCell::new(String::new()),
    });
    let weak = Rc::downgrade(&add_station);
    add_station.entry.connect_search_changed(move |_| {
        if let Some(add) = weak.upgrade() {
            add.entered();
        }
    });
    let button = add_station.add.downgrade();
    add_station.name.connect_changed(move |name| {
        if let Some(button) = button.upgrade() {
            button.set_sensitive(!name.text().trim().is_empty());
        }
    });
    let weak = Rc::downgrade(&add_station);
    add_station.add.connect_clicked(move |_| {
        if let Some(add) = weak.upgrade() {
            add.add_probed();
        }
    });
    // The dialog keeps its state until it closes.
    let anchor = RefCell::new(Some(add_station));
    dialog.connect_closed(move |_| {
        anchor.borrow_mut().take();
    });
    shell.present_selected_dialog(&dialog);
}

/// Opens Edit Station, shaped as Edit Playlist: the station's picture, name and homepage,
/// the channel its card plays, and its stream when it was added by address.
fn present_edit_dialog(shell: &Rc<Shell>, station: Station) {
    let Some(key) = station.key else {
        return;
    };
    let source = shell.products.source.clone();
    let weak = Rc::downgrade(shell);
    run(
        shell,
        async move { stations::picture_editing(&source, key).await },
        move |editing| {
            if let (Some(shell), Some(editing)) = (weak.upgrade(), editing) {
                build_edit_dialog(&shell, key, station, editing);
            }
        },
    );
}

fn build_edit_dialog(
    shell: &Rc<Shell>,
    key: i64,
    station: Station,
    editing: rufin_core::playlists::PlaylistArtworkEditing,
) {
    let resource = "routes/radio_edit.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        dialog: adw::Dialog,
        content: gtk::Box,
        layout: adw::WrapBox,
        options: gtk::Box,
        name: adw::EntryRow,
        homepage: adw::EntryRow,
        channel: adw::ComboRow,
        stream: adw::EntryRow,
        cancel: gtk::Button,
        save: gtk::Button,
    });
    let artwork = crate::editors::playlist_artwork::ArtworkEditor::new(
        &shell.products.source,
        &shell.artwork,
        editing,
        &content,
        &save,
        None,
    );
    layout.insert_child_after(&artwork.frame, None::<&gtk::Widget>);
    options.append(&artwork.status);
    name.set_text(&station.name);
    homepage.set_text(&station.homepage);
    // Rufin's list names the streams of its stations; only a station added by address has
    // a stream to change.
    stream.set_text(&station.stream_url);
    stream.set_visible(station.listed.is_none());

    let channels = station
        .channels
        .iter()
        .map(|channel| channel.name.clone())
        .collect::<Vec<_>>();
    channel.set_model(Some(&gtk::StringList::new(
        &channels.iter().map(String::as_str).collect::<Vec<_>>(),
    )));
    if let Some(own) = station.channels.iter().position(|channel| channel.own) {
        channel.set_selected(own as u32);
    }
    channel.set_visible(!channels.is_empty());

    let button = save.downgrade();
    name.connect_changed(move |name| {
        if let Some(button) = button.upgrade() {
            button.set_sensitive(!name.text().trim().is_empty());
        }
    });
    let weak_dialog = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = weak_dialog.upgrade() {
            dialog.close();
        }
    });
    let (weak_shell, weak_dialog) = (Rc::downgrade(shell), dialog.downgrade());
    let (name, homepage, stream) = (name.downgrade(), homepage.downgrade(), stream.downgrade());
    let channel = channel.downgrade();
    // The dialog's Apply keeps the picture editor for as long as the dialog lives.
    let editor = Rc::clone(&artwork);
    save.connect_clicked(move |_| {
        let (Some(shell), Some(name), Some(homepage), Some(stream)) = (
            weak_shell.upgrade(),
            name.upgrade(),
            homepage.upgrade(),
            stream.upgrade(),
        ) else {
            return;
        };
        let input = StationInput {
            name: name.text().trim().to_string(),
            stream_url: stream.text().trim().to_string(),
            homepage: homepage.text().trim().to_string(),
            logo: String::new(),
        };
        let chosen = channel
            .upgrade()
            .and_then(|channel| channels.get(channel.selected() as usize).cloned());
        let picture = editor.change();
        editor.set_busy(true);
        let source = shell.products.source.clone();
        let (editor, dialog) = (Rc::downgrade(&editor), weak_dialog.clone());
        run(
            &shell,
            async move { stations::edit(&source, key, input, chosen, picture).await },
            move |saved| {
                if let Some(editor) = editor.upgrade() {
                    editor.set_busy(false);
                    if saved.is_some() {
                        editor.saved();
                    }
                }
                if let (Some(()), Some(dialog)) = (saved, dialog.upgrade()) {
                    dialog.close();
                }
            },
        );
    });
    shell.present_selected_dialog(&dialog);
}
