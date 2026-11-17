use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ::library::{GenreChoice, MixKind, ReadCancellation};
use adw::prelude::*;
use gtk::glib;
use rufin_core::playback::QueuePlacement;
use tracing::warn;

use localization::tr;

use crate::shell::shell::Shell;
use rufin_core::settings::{MixGenreSelection, MixSettings};

/// The kind tiles a page holds, in three columns.
const KINDS_PER_PAGE: usize = 6;
const KIND_COLUMNS: usize = 3;

#[derive(Clone)]
struct MixControls {
    /// Tracks, or albums.
    unit: adw::ToggleGroup,
    /// Each kind core offers with its tile, in the order the pages show them.
    kinds: Rc<Vec<(MixKind, gtk::ToggleButton)>>,
    genre: gtk::DropDown,
    min_year: gtk::Entry,
    max_year: gtk::Entry,
    /// The count of the unit chosen.
    limit: adw::SpinRow,
    /// The counts of tracks and of albums.
    limits: Rc<Cell<[usize; 2]>>,
    favorites: adw::SwitchRow,
    /// The kinds whose rules pick favorite tracks alone.
    favorite_kinds: Rc<Vec<MixKind>>,
    /// Favorites only as chosen, which a kind that fixes it on keeps.
    chosen_favorites: Rc<Cell<bool>>,
}

/// Favorites only follows the chosen kind: picking tracks of a kind whose rules pick favorites
/// alone it shows on and can't change; otherwise it is as chosen.
fn show_favorites(
    unit: &adw::ToggleGroup,
    kinds: &[(MixKind, gtk::ToggleButton)],
    favorite_kinds: &[MixKind],
    favorites: &adw::SwitchRow,
    chosen: bool,
) {
    let albums = unit.active_name().as_deref() == Some("albums");
    let fixed = !albums
        && kinds
            .iter()
            .find(|(_, tile)| tile.is_active())
            .is_some_and(|(kind, _)| favorite_kinds.contains(kind));
    // Insensitive first, so the switch's own handler leaves the choice as it was.
    favorites.set_sensitive(!fixed);
    favorites.set_active(fixed || chosen);
}

/// Fills `carousel` with pages of kind tiles, all in one group: a glyph over each kind's name.
/// Answers each kind with its tile, and the kinds whose rules pick favorite tracks alone.
fn kind_tiles(carousel: &adw::Carousel) -> (Vec<(MixKind, gtk::ToggleButton)>, Vec<MixKind>) {
    let mut tiles: Vec<(MixKind, gtk::ToggleButton)> = Vec::new();
    let kinds = rufin_core::mix::mix_kinds();
    let favorite_kinds = kinds
        .iter()
        .filter(|choice| choice.favorites)
        .map(|choice| choice.kind)
        .collect();
    for page in kinds.chunks(KINDS_PER_PAGE) {
        let grid = gtk::Grid::builder()
            .column_homogeneous(true)
            .row_homogeneous(true)
            .column_spacing(6)
            .row_spacing(6)
            .hexpand(true)
            .valign(gtk::Align::Start)
            .build();
        for (index, choice) in page.iter().enumerate() {
            let content = gtk::Box::new(gtk::Orientation::Vertical, 5);
            content.set_valign(gtk::Align::Center);
            let glyph = gtk::Image::from_icon_name(&choice.icon);
            glyph.set_pixel_size(20);
            content.append(&glyph);
            let title = gtk::Label::builder()
                .label(tr(&choice.title))
                .wrap(true)
                .lines(2)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .justify(gtk::Justification::Center)
                .build();
            // Two lines tall whatever its width: the carousel measures its pages before it gives
            // them their width, so a tile's height must not follow it.
            title.connect_realize(|title| {
                title.set_size_request(-1, title.create_pango_layout(Some("\n")).pixel_size().1);
            });
            content.append(&title);
            let tile = gtk::ToggleButton::builder()
                .child(&content)
                .css_classes(["mix-kind", "mix-glyph"])
                .build();
            if let Some((_, first)) = tiles.first() {
                tile.set_group(Some(first));
            }
            grid.attach(
                &tile,
                (index % KIND_COLUMNS) as i32,
                (index / KIND_COLUMNS) as i32,
                1,
                1,
            );
            tiles.push((choice.kind, tile));
        }
        // A short last page keeps the other pages' column widths.
        for column in page.len()..KIND_COLUMNS {
            grid.attach(
                &gtk::Box::new(gtk::Orientation::Vertical, 0),
                column as i32,
                0,
                1,
                1,
            );
        }
        carousel.append(&grid);
    }
    (tiles, favorite_kinds)
}

/// ‹ › page through the kinds, each insensitive at its end; Left and Right past the edge tile
/// of a page move to the next page's tile in the same row.
fn page_kinds(
    carousel: &adw::Carousel,
    tiles: &Rc<Vec<(MixKind, gtk::ToggleButton)>>,
    previous: &gtk::Button,
    next: &gtk::Button,
) {
    let sync = {
        let (previous, next) = (previous.clone(), next.clone());
        move |carousel: &adw::Carousel| {
            let page = carousel.position().round() as u32;
            previous.set_sensitive(page > 0);
            next.set_sensitive(page + 1 < carousel.n_pages());
        }
    };
    sync(carousel);
    carousel.connect_page_changed(move |carousel, _| sync(carousel));
    for (button, step) in [(previous, -1), (next, 1)] {
        let carousel = carousel.downgrade();
        button.connect_clicked(move |_| {
            let Some(carousel) = carousel.upgrade() else {
                return;
            };
            let page = carousel.position().round() as i32 + step;
            if (0..carousel.n_pages() as i32).contains(&page) {
                carousel.scroll_to(&carousel.nth_page(page as u32), true);
            }
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = carousel.downgrade();
    let tiles = Rc::downgrade(tiles);
    keys.connect_key_pressed(move |_, key, _, _| {
        let forward = match key {
            gtk::gdk::Key::Right => true,
            gtk::gdk::Key::Left => false,
            _ => return glib::Propagation::Proceed,
        };
        let (Some(carousel), Some(tiles)) = (weak.upgrade(), tiles.upgrade()) else {
            return glib::Propagation::Proceed;
        };
        let Some(index) = tiles.iter().position(|(_, tile)| tile.has_focus()) else {
            return glib::Propagation::Proceed;
        };
        let (page, place) = (index / KINDS_PER_PAGE, index % KINDS_PER_PAGE);
        let (row, column) = (place / KIND_COLUMNS, place % KIND_COLUMNS);
        let edge = if forward {
            column + 1 == KIND_COLUMNS || index + 1 == tiles.len()
        } else {
            column == 0
        };
        let target = match (edge, forward) {
            (false, _) => None,
            (true, true) => Some(page + 1).filter(|page| page * KINDS_PER_PAGE < tiles.len()),
            (true, false) => page.checked_sub(1),
        };
        let Some(target) = target else {
            return glib::Propagation::Proceed;
        };
        let first = target * KINDS_PER_PAGE;
        let last = (first + KINDS_PER_PAGE).min(tiles.len()) - 1;
        let column = if forward { 0 } else { KIND_COLUMNS - 1 };
        let tile = (first + row * KIND_COLUMNS + column).min(last);
        carousel.scroll_to(&carousel.nth_page(target as u32), true);
        tiles[tile].1.grab_focus();
        glib::Propagation::Stop
    });
    carousel.add_controller(keys);
}

/// Opens the Mix popover over `anchor`, the player's Mix button.
pub(crate) fn present_mix_popover(anchor: &gtk::Button, shell: &Rc<Shell>) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let catalog = Arc::clone(&shell.products.library);
    let reading = selected.clone();
    let runtime = shell.products.runtime.clone();
    let task = runtime.spawn(async move {
        rufin_core::mix::mix_genres(
            &catalog,
            &reading,
            "",
            0..usize::MAX,
            &ReadCancellation::new(),
        )
        .await
        .map(|(_, genres)| genres)
    });
    let shell = Rc::downgrade(shell);
    let anchor = anchor.downgrade();
    glib::spawn_future_local(async move {
        let (Some(shell), Some(anchor)) = (shell.upgrade(), anchor.upgrade()) else {
            return;
        };
        match task.await.ok().and_then(Result::ok) {
            Some(genres) => present_mix_popover_loaded(&anchor, &shell, selected, genres),
            None => warn!("failed to load Mix genres"),
        }
    });
}

fn present_mix_popover_loaded(
    anchor: &gtk::Button,
    shell: &Rc<Shell>,
    selected: rufin_core::runtime::SelectedLibrary,
    genres: Vec<GenreChoice>,
) {
    let saved = shell.settings.current.borrow().mix.clone();
    let resource = "player/mix.ui";
    let builder = crate::ui_resource::builder(resource);
    let content: gtk::Box = crate::ui_resource::object(&builder, resource, "mix");
    let carousel: adw::Carousel = crate::ui_resource::object(&builder, resource, "kinds");
    let (kinds, favorite_kinds) = kind_tiles(&carousel);
    let controls = MixControls {
        unit: crate::ui_resource::object(&builder, resource, "unit"),
        kinds: Rc::new(kinds),
        genre: crate::ui_resource::object(&builder, resource, "genre"),
        min_year: crate::ui_resource::object(&builder, resource, "min_year"),
        max_year: crate::ui_resource::object(&builder, resource, "max_year"),
        limit: crate::ui_resource::object(&builder, resource, "limit"),
        limits: Rc::new(Cell::new([saved.limit, saved.album_limit])),
        favorites: crate::ui_resource::object(&builder, resource, "favorites"),
        favorite_kinds: Rc::new(favorite_kinds),
        chosen_favorites: Rc::new(Cell::new(saved.favorites)),
    };
    controls.favorites.connect_active_notify({
        let chosen = Rc::clone(&controls.chosen_favorites);
        move |row| {
            if row.is_sensitive() {
                chosen.set(row.is_active());
            }
        }
    });
    controls
        .unit
        .set_active_name(Some(if saved.albums { "albums" } else { "tracks" }));
    show_unit(&controls.limit, saved.albums, controls.limits.get());
    // Each unit keeps its own count.
    controls.unit.connect_active_name_notify({
        let limit = controls.limit.clone();
        let counts = Rc::clone(&controls.limits);
        move |unit| {
            let albums = unit.active_name().as_deref() == Some("albums");
            let mut limits = counts.get();
            limits[usize::from(!albums)] = limit.value() as usize;
            counts.set(limits);
            show_unit(&limit, albums, limits);
        }
    });
    let kind = controls
        .kinds
        .iter()
        .position(|(kind, _)| *kind == saved.kind)
        .unwrap_or_default();
    if let Some((_, tile)) = controls.kinds.get(kind) {
        tile.set_active(true);
        carousel.scroll_to(&carousel.nth_page((kind / KINDS_PER_PAGE) as u32), false);
    }
    show_favorites(
        &controls.unit,
        &controls.kinds,
        &controls.favorite_kinds,
        &controls.favorites,
        controls.chosen_favorites.get(),
    );
    // The tiles and the unit belong to the controls; their handlers hold them weakly.
    let follow = Rc::new({
        let unit = controls.unit.downgrade();
        let favorites = controls.favorites.downgrade();
        let kinds = Rc::downgrade(&controls.kinds);
        let favorite_kinds = Rc::clone(&controls.favorite_kinds);
        let chosen = Rc::clone(&controls.chosen_favorites);
        move || {
            if let (Some(unit), Some(favorites), Some(kinds)) =
                (unit.upgrade(), favorites.upgrade(), kinds.upgrade())
            {
                show_favorites(&unit, &kinds, &favorite_kinds, &favorites, chosen.get());
            }
        }
    });
    for (_, tile) in controls.kinds.iter() {
        let follow = Rc::clone(&follow);
        tile.connect_toggled(move |tile| {
            if tile.is_active() {
                follow();
            }
        });
    }
    controls.unit.connect_active_name_notify(move |_| follow());
    page_kinds(
        &carousel,
        &controls.kinds,
        &crate::ui_resource::object(&builder, resource, "previous_kinds"),
        &crate::ui_resource::object(&builder, resource, "next_kinds"),
    );
    configure_genre_dropdown(
        &controls.genre,
        &genres,
        saved.selected_genre_id(
            &selected.source_id,
            selected.music_folder_object_id.as_deref(),
        ),
    );
    for (entry, year) in [
        (&controls.min_year, saved.min_year),
        (&controls.max_year, saved.max_year),
    ] {
        entry.set_text(&year.map(|year| year.to_string()).unwrap_or_default());
    }

    let popover = gtk::Popover::new();
    popover.set_autohide(true);
    popover.set_position(gtk::PositionType::Top);
    popover.set_child(Some(&content));
    for (id, placement) in [
        ("play_next", QueuePlacement::Next),
        ("play_later", QueuePlacement::Last),
        ("play_now", QueuePlacement::Now),
    ] {
        let button: gtk::Button = crate::ui_resource::object(&builder, resource, id);
        let shell = Rc::clone(shell);
        let popover = popover.downgrade();
        let controls = controls.clone();
        let genres = genres.clone();
        let selected = selected.clone();
        button.connect_clicked(move |_| {
            let settings = settings_from_controls(
                &controls,
                &genres,
                selected.source_id.clone(),
                selected.music_folder_object_id.clone(),
            );
            shell
                .settings
                .update_app_settings("mix settings", |current| {
                    if current.mix == settings {
                        return false;
                    }
                    current.mix = settings.clone();
                    true
                });
            play_mix(&shell, settings, placement);
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        });
    }
    popover.set_parent(anchor);
    popover.connect_closed(|popover| popover.unparent());
    popover.popup();
}

/// Shows the count of the unit chosen, named after it.
fn show_unit(limit: &adw::SpinRow, albums: bool, limits: [usize; 2]) {
    limit.set_title(&if albums { tr("Albums") } else { tr("Tracks") });
    limit.set_subtitle(&if albums {
        tr("Each plays whole")
    } else {
        String::new()
    });
    limit.set_value(limits[usize::from(albums)] as f64);
}

fn configure_genre_dropdown(
    dropdown: &gtk::DropDown,
    genres: &[GenreChoice],
    selected: Option<&str>,
) {
    let mut labels = Vec::with_capacity(genres.len() + 1);
    labels.push(tr("Any"));
    labels.extend(genres.iter().map(|genre| genre.name.clone()));
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let model = gtk::StringList::new(&refs);
    dropdown.set_model(Some(&model));
    // The search matches any part of a genre's name; without an expression it finds nothing.
    dropdown.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        gtk::Expression::NONE,
        "string",
    )));
    dropdown.set_search_match_mode(gtk::StringFilterMatchMode::Substring);
    dropdown.set_selected(
        selected
            .and_then(|selected| genres.iter().position(|genre| genre.object_id == selected))
            .map_or(0, |index| index as u32 + 1),
    );
}

fn settings_from_controls(
    controls: &MixControls,
    genres: &[GenreChoice],
    source_id: sources::SourceId,
    music_folder_id: Option<String>,
) -> MixSettings {
    let albums = controls.unit.active_name().as_deref() == Some("albums");
    let mut limits = controls.limits.get();
    limits[usize::from(albums)] = controls.limit.value() as usize;
    // An empty or unreadable year leaves that end open.
    let year = |entry: &gtk::Entry| {
        entry
            .text()
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|year| (1850..=2050).contains(year))
    };
    MixSettings {
        albums,
        limit: limits[0],
        album_limit: limits[1],
        min_year: year(&controls.min_year),
        max_year: year(&controls.max_year),
        genre: selected_genre(genres, controls.genre.selected()).map(|genre_id| {
            MixGenreSelection {
                source_id,
                music_folder_id,
                genre_id,
            }
        }),
        kind: controls
            .kinds
            .iter()
            .find(|(_, tile)| tile.is_active())
            .map_or(MixKind::All, |(kind, _)| *kind),
        favorites: controls.chosen_favorites.get(),
    }
}

pub(crate) fn play_saved_mix(shell: &Rc<Shell>, placement: QueuePlacement) {
    let settings = shell.settings.current.borrow().mix.clone();
    play_mix(shell, settings, placement);
}

fn selected_genre(genres: &[GenreChoice], selected: u32) -> Option<String> {
    if selected == gtk::INVALID_LIST_POSITION || selected == 0 {
        return None;
    }
    genres
        .get((selected - 1) as usize)
        .map(|genre| genre.object_id.clone())
}

fn play_mix(shell: &Shell, settings: MixSettings, placement: QueuePlacement) {
    let queue = shell.products.playback.clone();
    let feedback = Rc::clone(&shell.control_feedback);
    glib::spawn_future_local(async move {
        let played = rufin_core::mix::play_saved_mix(&queue, settings, placement).await;
        show_mix_played(&feedback, played);
    });
}

/// Plays the Quick mix entry at `index` at `placement`, as its Home tile does.
pub(crate) fn play_quick_mix(shell: &Shell, index: u32, placement: QueuePlacement) {
    let queue = shell.products.playback.clone();
    let feedback = Rc::clone(&shell.control_feedback);
    glib::spawn_future_local(async move {
        let played = rufin_core::quick_mix::play_quick_mix(&queue, index, placement).await;
        show_mix_played(&feedback, played);
    });
}

/// Says so when a mix matched no tracks.
fn show_mix_played(
    feedback: &crate::widgets::feedback::ControlFeedbackState,
    played: Result<bool, String>,
) {
    match played {
        Ok(true) => {
            let resource = "player/mix.ui";
            let builder = crate::ui_resource::builder(resource);
            let message: gtk::Label = crate::ui_resource::object(&builder, resource, "no_matches");
            feedback.show_feedback_toast(message.text().to_string());
        }
        Ok(false) => {}
        Err(error) => warn!(%error, "failed to select Mix tracks"),
    }
}
