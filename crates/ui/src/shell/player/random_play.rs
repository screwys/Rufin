use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use ::library::{GenreChoice, PlayedFilter, ReadCancellation};
use adw::prelude::*;
use gtk::glib;
use rufin_core::playback::QueuePlacement;
use tracing::warn;

use localization::tr;

use crate::shell::shell::Shell;
use rufin_core::settings::{RandomPlayGenreSelection, RandomPlaySettings};

/// The kinds the popover offers, each with its button's id.
const KINDS: [(PlayedFilter, &str); 5] = [
    (PlayedFilter::All, "kind_all"),
    (PlayedFilter::Unplayed, "kind_unplayed"),
    (PlayedFilter::Played, "kind_played"),
    (PlayedFilter::NotPlayedRecently, "kind_not_played_recently"),
    (PlayedFilter::RecentlyAdded, "kind_recently_added"),
];

#[derive(Clone)]
struct MixControls {
    /// Tracks, or albums.
    unit: adw::ToggleGroup,
    kinds: [gtk::ToggleButton; 5],
    genre: gtk::DropDown,
    min_year: gtk::Entry,
    max_year: gtk::Entry,
    /// The count of the unit chosen.
    limit: adw::SpinRow,
    /// The counts of tracks and of albums.
    limits: Rc<Cell<[usize; 2]>>,
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
        rufin_core::radio::random_genres(
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
    let saved = shell.settings.current.borrow().random_play.clone();
    let resource = "player/random_play.ui";
    let builder = crate::ui_resource::builder(resource);
    let content: gtk::Box = crate::ui_resource::object(&builder, resource, "mix");
    let controls = MixControls {
        unit: crate::ui_resource::object(&builder, resource, "unit"),
        kinds: KINDS.map(|(_, id)| crate::ui_resource::object(&builder, resource, id)),
        genre: crate::ui_resource::object(&builder, resource, "genre"),
        min_year: crate::ui_resource::object(&builder, resource, "min_year"),
        max_year: crate::ui_resource::object(&builder, resource, "max_year"),
        limit: crate::ui_resource::object(&builder, resource, "limit"),
        limits: Rc::new(Cell::new([saved.limit, saved.album_limit])),
    };
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
    let kind = KINDS
        .iter()
        .position(|(kind, _)| *kind == saved.played_filter)
        .unwrap_or_default();
    controls.kinds[kind].set_active(true);
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
                    if current.random_play == settings {
                        return false;
                    }
                    current.random_play = settings.clone();
                    true
                });
            play_random(&shell, settings, placement);
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
) -> RandomPlaySettings {
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
    RandomPlaySettings {
        albums,
        limit: limits[0],
        album_limit: limits[1],
        min_year: year(&controls.min_year),
        max_year: year(&controls.max_year),
        genre: selected_genre(genres, controls.genre.selected()).map(|genre_id| {
            RandomPlayGenreSelection {
                source_id,
                music_folder_id,
                genre_id,
            }
        }),
        played_filter: KINDS
            .iter()
            .zip(&controls.kinds)
            .find(|(_, button)| button.is_active())
            .map_or(PlayedFilter::All, |((kind, _), _)| *kind),
    }
}

pub(crate) fn play_saved_random(shell: &Rc<Shell>, placement: QueuePlacement) {
    let settings = shell.settings.current.borrow().random_play.clone();
    play_random(shell, settings, placement);
}

fn selected_genre(genres: &[GenreChoice], selected: u32) -> Option<String> {
    if selected == gtk::INVALID_LIST_POSITION || selected == 0 {
        return None;
    }
    genres
        .get((selected - 1) as usize)
        .map(|genre| genre.object_id.clone())
}

fn play_random(shell: &Shell, settings: RandomPlaySettings, placement: QueuePlacement) {
    let queue = shell.products.playback.clone();
    let feedback = Rc::clone(&shell.control_feedback);
    glib::spawn_future_local(async move {
        match rufin_core::radio::play_saved_random(&queue, settings, placement).await {
            Ok(true) => {
                let resource = "player/random_play.ui";
                let builder = crate::ui_resource::builder(resource);
                let message: gtk::Label =
                    crate::ui_resource::object(&builder, resource, "no_matches");
                feedback.show_feedback_toast(message.text().to_string());
            }
            Ok(false) => {}
            Err(error) => warn!(%error, "failed to select Mix tracks"),
        }
    });
}
