use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ::library::{GenreRow, GenreSort, PlayedFilter, RandomCriteria, ReadCancellation};
use adw::prelude::*;
use gtk::glib;
use playback::{QueuePlacement, RandomPlayRequest};
use tracing::warn;

use localization::tr;

use crate::settings::{RandomPlayGenreSelection, RandomPlaySettings};
use crate::shell::Shell;

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
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let cancellation = ReadCancellation::new();
    let runtime = selected.runtime.clone();
    let task = runtime.spawn(async move {
        let order = database
            .genre_route_page(
                source,
                folder,
                "",
                GenreSort::Title,
                false,
                library::RouteSeedWindow::top(),
                &cancellation,
            )
            .await?
            .0;
        let mut genres = Vec::with_capacity(order.len());
        for keys in order.chunks(128) {
            genres.extend(
                database
                    .genre_rows(source, keys, folder, &cancellation)
                    .await?,
            );
        }
        Ok::<_, library::LibraryError>(genres)
    });
    let shell = Rc::downgrade(shell);
    glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        match task.await.ok().and_then(Result::ok) {
            Some(genres) => present_random_play_dialog_loaded(&shell, selected, genres),
            None => warn!("failed to load Random Play genres"),
        }
    });
}

fn present_random_play_dialog_loaded(
    shell: &Rc<Shell>,
    selected: crate::runtime::SelectedLibrary,
    genres: Vec<GenreRow>,
) {
    let played_filters = [
        PlayedFilter::All,
        PlayedFilter::Unplayed,
        PlayedFilter::Played,
    ];
    let saved = shell.settings.current.borrow().random_play.clone();
    let selected_genre = saved.selected_genre_id(
        &selected.source_id,
        selected.music_folder_object_id.as_deref(),
    );

    let resource = crate::ui_resource::RANDOM_PLAY_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    let dialog: adw::Dialog = crate::ui_resource::object(&builder, resource, "random_play_dialog");
    let controls = RandomPlayControls {
        limit: crate::ui_resource::object(&builder, resource, "limit"),
        min_year_enabled: crate::ui_resource::object(&builder, resource, "min_year_enabled"),
        min_year: crate::ui_resource::object(&builder, resource, "min_year"),
        max_year_enabled: crate::ui_resource::object(&builder, resource, "max_year_enabled"),
        max_year: crate::ui_resource::object(&builder, resource, "max_year"),
        genre: crate::ui_resource::object(&builder, resource, "genre"),
        played_filter: crate::ui_resource::object(&builder, resource, "played_filter"),
    };
    configure_spinner(&controls.limit, saved.limit as f64, MIN_LIMIT, MAX_LIMIT);
    configure_spinner(
        &controls.min_year,
        saved.min_year.map_or(DEFAULT_MIN_YEAR, f64::from),
        MIN_YEAR,
        MAX_YEAR,
    );
    configure_spinner(
        &controls.max_year,
        saved.max_year.map_or(DEFAULT_MAX_YEAR, f64::from),
        MIN_YEAR,
        MAX_YEAR,
    );
    configure_genre_dropdown(&controls.genre, &genres, selected_genre);
    configure_played_filter_dropdown(
        &controls.played_filter,
        &played_filters,
        saved.played_filter,
    );
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

    let play_next: gtk::Button = crate::ui_resource::object(&builder, resource, "play_next");
    let play_now: gtk::Button = crate::ui_resource::object(&builder, resource, "play_now");
    let play_later: gtk::Button = crate::ui_resource::object(&builder, resource, "play_later");
    drop(builder);

    connect_action(
        &play_next,
        shell,
        &dialog,
        &controls,
        &genres,
        &selected,
        QueuePlacement::Next,
    );
    connect_action(
        &play_now,
        shell,
        &dialog,
        &controls,
        &genres,
        &selected,
        QueuePlacement::Now,
    );
    connect_action(
        &play_later,
        shell,
        &dialog,
        &controls,
        &genres,
        &selected,
        QueuePlacement::Last,
    );

    shell.present_selected_dialog(&dialog);
}

fn configure_spinner(spinner: &gtk::SpinButton, default: f64, min: f64, max: f64) {
    spinner.set_range(min, max);
    spinner.set_increments(1.0, 10.0);
    spinner.set_climb_rate(1.0);
    spinner.set_value(default);
}

fn connect_year_toggle(check: &gtk::CheckButton, spinner: &gtk::SpinButton) {
    let spinner = spinner.clone();
    check.connect_toggled(move |check| {
        spinner.set_sensitive(check.is_active());
    });
}

fn configure_genre_dropdown(dropdown: &gtk::DropDown, genres: &[GenreRow], selected: Option<&str>) {
    let mut labels = Vec::with_capacity(genres.len() + 1);
    labels.push(tr("Any genre"));
    labels.extend(genres.iter().map(|genre| genre.name.clone()));
    let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
    let model = gtk::StringList::new(&refs);
    dropdown.set_model(Some(&model));
    dropdown.set_selected(
        selected
            .and_then(|selected| genres.iter().position(|genre| genre.object_id == selected))
            .map_or(0, |index| index as u32 + 1),
    );
}

fn configure_played_filter_dropdown(
    dropdown: &gtk::DropDown,
    filters: &[PlayedFilter],
    selected: PlayedFilter,
) {
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
    dropdown.set_model(Some(&model));
    dropdown.set_sensitive(filters.len() > 1);
    dropdown.set_selected(
        filters
            .iter()
            .position(|filter| *filter == selected)
            .unwrap_or_default() as u32,
    );
}

fn connect_action(
    button: &gtk::Button,
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    controls: &RandomPlayControls,
    genres: &[GenreRow],
    selected: &crate::runtime::SelectedLibrary,
    placement: QueuePlacement,
) {
    let shell = Rc::clone(shell);
    let dialog = dialog.downgrade();
    let controls = controls.clone();
    let genres = genres.to_vec();
    let selected = selected.clone();
    button.connect_clicked(move |_| {
        let Some(settings) = settings_from_controls(
            &controls,
            &genres,
            selected.source_id.clone(),
            selected.music_folder_object_id.clone(),
        ) else {
            return;
        };
        let request = request_from_settings(
            &settings,
            &genres,
            &selected.source_id,
            selected.music_folder_object_id.as_deref(),
            placement,
        );
        shell.update_app_settings("random play settings", |current| {
            if current.random_play == settings {
                return false;
            }
            current.random_play = settings;
            true
        });
        execute_random(&shell, selected.clone(), request);
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
}

fn settings_from_controls(
    controls: &RandomPlayControls,
    genres: &[GenreRow],
    source_id: sources::SourceId,
    music_folder_id: Option<String>,
) -> Option<RandomPlaySettings> {
    let genre_id = selected_genre(genres, controls.genre.selected());
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
    genres: &[GenreRow],
    source_id: &sources::SourceId,
    music_folder_id: Option<&str>,
    placement: QueuePlacement,
) -> RandomPlayRequest {
    let genre = settings
        .selected_genre_id(source_id, music_folder_id)
        .and_then(|selected| genres.iter().find(|genre| genre.object_id == selected));
    RandomPlayRequest {
        placement,
        requested: settings.limit,
        criteria: RandomCriteria {
            min_year: settings.min_year.map(i64::from),
            max_year: settings.max_year.map(i64::from),
            genre: genre.map(|genre| genre.genre_key),
            played: settings.played_filter,
            require_media: false,
            variation: random_variation(),
        },
    }
}

pub(crate) fn play_saved_random(shell: &Rc<Shell>, placement: QueuePlacement) {
    let Some(selected) = shell.selected_library().as_deref().cloned() else {
        return;
    };
    let settings = shell.settings.current.borrow().random_play.clone();
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let genre_object_id = settings
        .selected_genre_id(
            &selected.source_id,
            selected.music_folder_object_id.as_deref(),
        )
        .map(str::to_owned);
    let runtime = selected.runtime.clone();
    let queue = shell.products.playback.queue.clone();
    runtime.spawn(async move {
        let cancellation = ReadCancellation::new();
        let genre = match genre_object_id {
            Some(object_id) => database
                .genre_key_by_object(source, &object_id, &cancellation)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let request = RandomPlayRequest {
            placement,
            requested: settings.limit,
            criteria: RandomCriteria {
                min_year: settings.min_year.map(i64::from),
                max_year: settings.max_year.map(i64::from),
                genre,
                played: settings.played_filter,
                require_media: false,
                variation: random_variation(),
            },
        };
        execute_random_task(selected, request, queue).await;
    });
}

fn selected_genre(genres: &[GenreRow], selected: u32) -> Option<String> {
    if selected == gtk::INVALID_LIST_POSITION || selected == 0 {
        return None;
    }
    genres
        .get((selected - 1) as usize)
        .map(|genre| genre.object_id.clone())
}

fn execute_random(
    shell: &Shell,
    selected: crate::runtime::SelectedLibrary,
    request: RandomPlayRequest,
) {
    let runtime = selected.runtime.clone();
    let queue = shell.products.playback.queue.clone();
    runtime.spawn(execute_random_task(selected, request, queue));
}

async fn execute_random_task(
    selected: crate::runtime::SelectedLibrary,
    request: RandomPlayRequest,
    queue: playback::QueueHandle,
) {
    let cancellation = ReadCancellation::new();
    let order = match selected
        .database
        .random_candidates(
            selected.source_key,
            selected.music_folder_key,
            &request.criteria,
            &[],
            request.requested,
            &cancellation,
        )
        .await
    {
        Ok(order) => Arc::<[String]>::from(order),
        Err(error) => {
            warn!(%error, "failed to select Random Play tracks");
            return;
        }
    };
    let media = selected
        .database
        .queue_items_for_uris(&order, &cancellation)
        .await
        .unwrap_or_default();
    if let Some(request) = playback::PlayRequest::random(media.into(), request.placement) {
        queue.play(request);
    }
}

fn random_variation() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as i64)
}

#[cfg(test)]
mod tests {
    use library::PlayedFilter;
    use playback::QueuePlacement;
    use sources::SourceId;

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
            assert_eq!(request.requested, 24);
            assert_eq!(request.criteria.min_year, Some(1995));
            assert_eq!(request.criteria.max_year, Some(2015));
            assert_eq!(request.criteria.played, PlayedFilter::Unplayed);
            assert!(request.criteria.genre.is_none());
        }
    }
}
