use std::rc::Rc;

use ::library::{GenreId, GenreSummary, PlayedFilter, RandomCriteria};
use adw::prelude::*;
use playback::{QueuePlacement, RandomPlayRequest};

use localization::tr;

use crate::settings::{RandomPlayGenreSelection, RandomPlaySettings};
use crate::shell::Shell;
use crate::shell::actions::text_button;
use crate::shell::actions::{PLAY_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON};

const MIN_LIMIT: f64 = 1.0;
const MAX_LIMIT: f64 = 500.0;
const MIN_YEAR: f64 = 1850.0;
const MAX_YEAR: f64 = 2050.0;
const DEFAULT_MIN_YEAR: f64 = 2000.0;
const DEFAULT_MAX_YEAR: f64 = 2020.0;

#[derive(Clone)]
struct RandomPlayControls {
    limit: gtk::SpinButton,
    min_year_enabled: gtk::CheckButton,
    min_year: gtk::SpinButton,
    max_year_enabled: gtk::CheckButton,
    max_year: gtk::SpinButton,
    genre: gtk::DropDown,
    played_filter: gtk::DropDown,
}

pub(super) fn present_random_play_dialog(shell: &Rc<Shell>) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let genres = selected
        .library
        .genres(selected.music_folder_id.as_ref())
        .unwrap_or_default();
    let played_filters = [
        PlayedFilter::All,
        PlayedFilter::Unplayed,
        PlayedFilter::Played,
    ];
    let saved = shell.settings.current.borrow().random_play.clone();
    let selected_genre =
        saved.selected_genre_id(&selected.source_id, selected.music_folder_id.as_ref());

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new(&tr("Play random"), "");
    header.set_title_widget(Some(&title));
    toolbar.add_top_bar(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let controls = RandomPlayControls {
        limit: count_spinner(saved.limit as f64, MIN_LIMIT, MAX_LIMIT),
        min_year_enabled: gtk::CheckButton::new(),
        min_year: count_spinner(
            saved.min_year.map_or(DEFAULT_MIN_YEAR, f64::from),
            MIN_YEAR,
            MAX_YEAR,
        ),
        max_year_enabled: gtk::CheckButton::new(),
        max_year: count_spinner(
            saved.max_year.map_or(DEFAULT_MAX_YEAR, f64::from),
            MIN_YEAR,
            MAX_YEAR,
        ),
        genre: genre_dropdown(&genres, selected_genre),
        played_filter: played_filter_dropdown(&played_filters, saved.played_filter),
    };
    controls
        .min_year_enabled
        .set_active(saved.min_year.is_some());
    controls
        .max_year_enabled
        .set_active(saved.max_year.is_some());
    controls.min_year.set_sensitive(saved.min_year.is_some());
    controls.max_year.set_sensitive(saved.max_year.is_some());
    connect_year_toggle(&controls.min_year_enabled, &controls.min_year);
    connect_year_toggle(&controls.max_year_enabled, &controls.max_year);

    content.append(&control_row(&tr("Number of songs"), &controls.limit));
    content.append(&optional_control_row(
        &tr("Minimum year"),
        &controls.min_year_enabled,
        &controls.min_year,
    ));
    content.append(&optional_control_row(
        &tr("Maximum year"),
        &controls.max_year_enabled,
        &controls.max_year,
    ));
    content.append(&control_row(&tr("Genre"), &controls.genre));
    content.append(&control_row(&tr("Play filter"), &controls.played_filter));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let play_next = text_button(PLAY_NEXT_ICON, "Play Next");
    let play_now = text_button(PLAY_ICON, "Play");
    play_now.add_css_class("suggested-action");
    let play_later = text_button(PLAY_LATER_ICON, "Play Later");
    actions.append(&play_next);
    actions.append(&play_now);
    actions.append(&play_later);
    content.append(&actions);

    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .content_width(460)
        .child(&toolbar)
        .build();

    connect_action(
        &play_next,
        shell,
        &dialog,
        &controls,
        &genres,
        &selected.source_id,
        selected.music_folder_id.as_ref(),
        QueuePlacement::Next,
    );
    connect_action(
        &play_now,
        shell,
        &dialog,
        &controls,
        &genres,
        &selected.source_id,
        selected.music_folder_id.as_ref(),
        QueuePlacement::Now,
    );
    connect_action(
        &play_later,
        shell,
        &dialog,
        &controls,
        &genres,
        &selected.source_id,
        selected.music_folder_id.as_ref(),
        QueuePlacement::Last,
    );

    shell.present_selected_dialog(&dialog);
}

fn count_spinner(default: f64, min: f64, max: f64) -> gtk::SpinButton {
    let spinner = gtk::SpinButton::with_range(min, max, 1.0);
    spinner.set_value(default);
    spinner.set_numeric(true);
    spinner.set_width_chars(5);
    spinner
}

fn connect_year_toggle(check: &gtk::CheckButton, spinner: &gtk::SpinButton) {
    let spinner = spinner.clone();
    check.connect_toggled(move |check| {
        spinner.set_sensitive(check.is_active());
    });
}

fn genre_dropdown(genres: &[GenreSummary], selected: Option<&GenreId>) -> gtk::DropDown {
    let mut labels = Vec::with_capacity(genres.len() + 1);
    labels.push(tr("Any genre"));
    labels.extend(genres.iter().map(|genre| genre.genre.name.clone()));
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let model = gtk::StringList::new(&refs);
    let dropdown = gtk::DropDown::new(Some(model), None::<gtk::Expression>);
    dropdown.set_enable_search(true);
    dropdown.set_selected(
        selected
            .and_then(|selected| genres.iter().position(|genre| &genre.genre.id == selected))
            .map_or(0, |index| index as u32 + 1),
    );
    dropdown
}

fn played_filter_dropdown(filters: &[PlayedFilter], selected: PlayedFilter) -> gtk::DropDown {
    let labels = filters
        .iter()
        .map(|filter| match filter {
            PlayedFilter::All => tr("All tracks"),
            PlayedFilter::Unplayed => tr("Only unplayed tracks"),
            PlayedFilter::Played => tr("Only played tracks"),
        })
        .collect::<Vec<_>>();
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let model = gtk::StringList::new(&refs);
    let dropdown = gtk::DropDown::new(Some(model), None::<gtk::Expression>);
    dropdown.set_sensitive(filters.len() > 1);
    dropdown.set_selected(
        filters
            .iter()
            .position(|filter| *filter == selected)
            .unwrap_or_default() as u32,
    );
    dropdown
}

fn control_row<W: IsA<gtk::Widget>>(label: &str, control: &W) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_valign(gtk::Align::Center);
    let label = gtk::Label::new(Some(label));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&label);
    row.append(control);
    row
}

fn optional_control_row<W: IsA<gtk::Widget>>(
    label: &str,
    check: &gtk::CheckButton,
    control: &W,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_valign(gtk::Align::Center);
    let label = gtk::Label::new(Some(label));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&label);
    row.append(check);
    row.append(control);
    row
}

fn connect_action(
    button: &gtk::Button,
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    controls: &RandomPlayControls,
    genres: &[GenreSummary],
    source_id: &library::SourceId,
    music_folder_id: Option<&library::MusicFolderId>,
    placement: QueuePlacement,
) {
    let shell = Rc::clone(shell);
    let dialog = dialog.downgrade();
    let controls = controls.clone();
    let genres = genres.to_vec();
    let source_id = source_id.clone();
    let music_folder_id = music_folder_id.cloned();
    button.connect_clicked(move |_| {
        let Some(settings) = settings_from_controls(
            &controls,
            &genres,
            source_id.clone(),
            music_folder_id.clone(),
        ) else {
            return;
        };
        let request = request_from_settings(
            &settings,
            &genres,
            &source_id,
            music_folder_id.as_ref(),
            placement,
        );
        shell.update_app_settings("random play settings", |current| {
            if current.random_play == settings {
                return false;
            }
            current.random_play = settings;
            true
        });
        shell.products.playback.radio.play_random(request);
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
}

fn settings_from_controls(
    controls: &RandomPlayControls,
    genres: &[GenreSummary],
    source_id: library::SourceId,
    music_folder_id: Option<library::MusicFolderId>,
) -> Option<RandomPlaySettings> {
    let (genre_id, _) = selected_genre(genres, controls.genre.selected());
    let played_filter = [
        PlayedFilter::All,
        PlayedFilter::Unplayed,
        PlayedFilter::Played,
    ]
    .get(controls.played_filter.selected() as usize)
    .copied()?;
    Some(RandomPlaySettings {
        limit: controls.limit.value_as_int().clamp(1, 500) as usize,
        min_year: controls
            .min_year_enabled
            .is_active()
            .then(|| controls.min_year.value_as_int().clamp(1850, 2050) as u16),
        max_year: controls
            .max_year_enabled
            .is_active()
            .then(|| controls.max_year.value_as_int().clamp(1850, 2050) as u16),
        genre: genre_id.map(|genre_id| RandomPlayGenreSelection {
            source_id,
            music_folder_id,
            genre_id,
        }),
        played_filter,
    })
}

fn request_from_settings(
    settings: &RandomPlaySettings,
    genres: &[GenreSummary],
    source_id: &library::SourceId,
    music_folder_id: Option<&library::MusicFolderId>,
    placement: QueuePlacement,
) -> RandomPlayRequest {
    let genre = settings
        .selected_genre_id(source_id, music_folder_id)
        .and_then(|selected| genres.iter().find(|genre| &genre.genre.id == selected));
    RandomPlayRequest {
        placement,
        criteria: RandomCriteria {
            limit: settings.limit,
            min_year: settings.min_year,
            max_year: settings.max_year,
            genre_id: genre.map(|genre| genre.genre.id.clone()),
            genre_name: genre.map(|genre| genre.genre.name.clone()),
            played_filter: settings.played_filter,
        },
    }
}

pub(crate) fn play_saved_random(shell: &Rc<Shell>, placement: QueuePlacement) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let genres = selected
        .library
        .genres(selected.music_folder_id.as_ref())
        .unwrap_or_default();
    let settings = shell.settings.current.borrow().random_play.clone();
    shell
        .products
        .playback
        .radio
        .play_random(request_from_settings(
            &settings,
            &genres,
            &selected.source_id,
            selected.music_folder_id.as_ref(),
            placement,
        ));
}

fn selected_genre(genres: &[GenreSummary], selected: u32) -> (Option<GenreId>, Option<String>) {
    if selected == gtk::INVALID_LIST_POSITION || selected == 0 {
        return (None, None);
    }
    let Some(genre) = genres.get((selected - 1) as usize) else {
        return (None, None);
    };
    (Some(genre.genre.id.clone()), Some(genre.genre.name.clone()))
}

#[cfg(test)]
mod tests {
    use library::{PlayedFilter, SourceId};
    use playback::QueuePlacement;

    use super::{RandomPlaySettings, request_from_settings};

    #[test]
    fn saved_random_criteria_accept_each_explicit_placement() {
        let settings = RandomPlaySettings {
            limit: 24,
            min_year: Some(1995),
            max_year: Some(2015),
            genre: None,
            played_filter: PlayedFilter::Unplayed,
        };
        let source_id = SourceId::new("source");

        for placement in [
            QueuePlacement::Now,
            QueuePlacement::Next,
            QueuePlacement::Last,
        ] {
            let request = request_from_settings(&settings, &[], &source_id, None, placement);
            assert_eq!(request.placement, placement);
            assert_eq!(request.criteria.limit, 24);
            assert_eq!(request.criteria.min_year, Some(1995));
            assert_eq!(request.criteria.max_year, Some(2015));
            assert_eq!(request.criteria.played_filter, PlayedFilter::Unplayed);
            assert!(request.criteria.genre_id.is_none());
            assert!(request.criteria.genre_name.is_none());
        }
    }
}
