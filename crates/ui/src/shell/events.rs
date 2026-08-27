use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::{EditableExt, FileExt, WidgetExt};
use gtk::{gio, glib};
use localization::tr;
use tracing::warn;

use crate::player::fullscreen::{FullscreenPlaybackRefresh, fullscreen_playback_refresh};
use crate::player::state::current_playback_media_id;
use crate::player::{now_playing_notification_can_send, now_playing_notification_should_withdraw};
use crate::preferences::dialogs::release_notes::apply_release_update;
use crate::preferences::source::source_progress_text;
use crate::routes::playlist_picker::refresh_context_playlist_picker;
use crate::routes::route::Route;
use crate::runtime::source::{
    ConfiguredSources, DiscoveryStatus, DiscoveryUpdate, LocalFolder, SourceOperation,
    SourceProgress,
};
use crate::runtime::{
    CatalogChange, CatalogPublication, ProductReceivers, SourceEvent, SourceNotice,
    SourceNoticeKind, WaveformProjection,
};

use super::Shell;
use super::navigation::update_sidebar_pin_playback;
use super::route::route_current_track;

pub(crate) fn install_product_event_receivers(shell: &Rc<Shell>, receivers: ProductReceivers) {
    let ProductReceivers {
        source,
        source_discovery,
        downloads,
        playback,
        visualizer,
        waveform,
        lyrics,
        release_updates,
    } = receivers;

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(event) = source.recv().await {
            apply_source_event(&event_shell, event);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(update) = source_discovery.recv().await {
            apply_source_discovery(&event_shell, update);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(event) = downloads.recv().await {
            event_shell.apply_download_event(event);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(publication) = playback.recv().await {
            apply_playback_publication(&event_shell, publication);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(frame) = visualizer.recv().await {
            let matches = event_shell
                .selected_library()
                .as_deref()
                .is_some_and(|selected| {
                    selected.source_key == frame.source_key
                        && selected.source_session_epoch == frame.source_session_epoch
                })
                && event_shell
                    .selected_playback()
                    .as_deref()
                    .and_then(|player| player.transport.current.as_ref())
                    .is_some_and(|current| current.id.run == Some(frame.run));
            if matches {
                event_shell.apply_visualizer_levels(frame.levels);
            }
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(waveform) = waveform.recv().await {
            apply_waveform(&event_shell, waveform);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(event) = lyrics.recv().await {
            apply_lyrics_event(&event_shell, event);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(update) = release_updates.recv().await {
            apply_release_update(&event_shell, update);
        }
    });
}

fn apply_source_event(shell: &Rc<Shell>, event: SourceEvent) {
    match event {
        SourceEvent::Configured(configured) => apply_configured_sources(shell, configured),
        SourceEvent::Selected {
            configured,
            selected,
            playback,
        } => {
            apply_selected_source(shell, configured, selected, *playback);
        }
        SourceEvent::CatalogReplaced {
            configured,
            selected,
        } => {
            apply_selected_library_replacement(shell, configured, selected);
        }
        SourceEvent::Operation(operation) => apply_source_operation(shell, operation),
        SourceEvent::ArtworkPreparation {
            source_key,
            revision,
            progress,
        } => apply_source_artwork_preparation(shell, source_key, revision, progress),
        SourceEvent::CatalogPublished(publication) => apply_catalog_publication(shell, publication),
        SourceEvent::Notice(notice) => apply_source_notice(shell, notice),
        SourceEvent::ReleaseSelected { acknowledged } => {
            release_selected_source(shell);
            let _ = acknowledged.try_send(());
        }
    }
}

fn apply_playback_publication(shell: &Rc<Shell>, publication: crate::runtime::PlaybackPublication) {
    let matches_selected = shell.selected_library().as_deref().is_some_and(|selected| {
        selected.source_key == publication.source_key
            && selected.source_session_epoch == publication.source_session_epoch
    });
    if matches_selected {
        apply_playback_projection(shell, publication.projection);
    }
}

fn release_selected_source(shell: &Rc<Shell>) {
    let released_source = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.artwork.source_id.clone());
    shell.clear_mounted_routes();
    shell.release_selected_navigation();
    shell.reset_cover_pipeline_state();
    shell.close_fullscreen_player();
    shell.detach_selected_ui_roots();
    if let Some(source_id) = released_source.as_ref() {
        shell.release_artwork_textures(source_id);
    }
    let released = shell.selected_ui.take();
    drop(released);
    shell.right_panel.queue_search.set_text("");
    shell.sync_bottom_player_favorite();
    shell.rebuild_sidebar_navigation();
    shell.update_bottom_player();
    shell.render_queue_panel();
    shell.render_lyrics_panel();
    shell.apply_visualizer_levels(Vec::new());
    shell.clear_fullscreen_player_cover();
    shell.withdraw_now_playing_notification();
    shell.update_media_controls();
}

#[derive(Clone)]
struct SourcePresentation {
    first_run: bool,
    selected: Option<crate::runtime::SelectedLibrary>,
}

fn apply_configured_sources(
    shell: &Rc<Shell>,
    configured: crate::runtime::source::ConfiguredSources,
) {
    let previous = current_source_presentation(shell);
    let next = SourcePresentation {
        first_run: configured.first_run,
        selected: previous.selected.clone(),
    };
    *shell.source.configured.borrow_mut() = configured;
    finish_source_assignment(shell, previous, next);
    shell.update_bottom_player();
}

fn current_source_presentation(shell: &Shell) -> SourcePresentation {
    SourcePresentation {
        first_run: shell.source.configured.borrow().first_run,
        selected: shell.selected_library().as_deref().cloned(),
    }
}

fn finish_source_assignment(
    shell: &Rc<Shell>,
    previous: SourcePresentation,
    next: SourcePresentation,
) {
    let previous_source_id = previous
        .selected
        .as_ref()
        .map(|selected| selected.source_key);
    let next_source_id = next.selected.as_ref().map(|selected| selected.source_key);
    let previous_epoch = previous
        .selected
        .as_ref()
        .map(|selected| selected.source_session_epoch);
    let next_epoch = next
        .selected
        .as_ref()
        .map(|selected| selected.source_session_epoch);
    let previous_loaded = previous
        .selected
        .as_ref()
        .map(|selected| Arc::clone(&selected.database));
    let next_loaded = next
        .selected
        .as_ref()
        .map(|selected| Arc::clone(&selected.database));
    let previous_scope = previous.selected.as_ref().map(|selected| {
        (
            selected.music_folder_key,
            selected.music_folder_object_id.clone(),
        )
    });
    let next_scope = next.selected.as_ref().map(|selected| {
        (
            selected.music_folder_key,
            selected.music_folder_object_id.clone(),
        )
    });
    let source_changed = previous_source_id != next_source_id;
    let session_changed = previous_epoch != next_epoch;
    let library_changed = match (&previous_loaded, &next_loaded) {
        (Some(previous), Some(next)) => !Arc::ptr_eq(previous, next),
        (None, None) => false,
        _ => true,
    };
    let scope_changed = previous_scope != next_scope;
    let entered_first_run = next.first_run && !previous.first_run;
    let left_first_run = previous.first_run && !next.first_run;

    if entered_first_run {
        shell.close_preferences_dialog();
        shell.source.discovery_started.set(false);
        shell.source.discovery_running.set(false);
        shell.source.discovered_servers.borrow_mut().clear();
        *shell.source.discovery_status.borrow_mut() = DiscoveryStatus::Idle;
    }
    if left_first_run {
        shell.release_first_run_setup();
    }

    refresh_context_playlist_picker(shell);
    shell.sync_bottom_player_favorite();
    shell.import_remote_playlist_pins_once();
    let rebuild_sidebar = source_changed || session_changed || library_changed || scope_changed;

    if next.selected.is_none() {
        if rebuild_sidebar {
            shell.rebuild_sidebar_navigation();
        }
        shell.clear_mounted_routes();
        shell.update_layout();
        shell.render_current_route();
        return;
    }

    if session_changed && !source_changed {
        shell.reset_cover_pipeline_state();
    }

    if source_changed {
        shell.clear_mounted_routes();
        shell.reset_cover_pipeline_state();
        shell.reset_navigation_to_home();
    } else if session_changed || library_changed || scope_changed {
        // A mounted Home owns its current snapshot for the duration of the
        // visit. Other routes keep their current page until its replacement
        // projection is ready.
        if !matches!(shell.navigation.routes.borrow().current(), Route::Home) {
            shell.replace_current_route_when_ready();
        }
    }

    if rebuild_sidebar {
        shell.rebuild_sidebar_navigation();
    }

    if !shell.startup.route_revealed.get()
        && !shell.source.operation.borrow().blocks_library()
        && !shell.source.login_screen_active()
    {
        shell.schedule_startup_route_reveal();
    }
}

fn apply_selected_source(
    shell: &Rc<Shell>,
    configured: crate::runtime::source::ConfiguredSources,
    selected: crate::runtime::SelectedLibrary,
    playback: playback::PlaybackProjection,
) {
    let previous_source = current_source_presentation(shell);
    let next_source = SourcePresentation {
        first_run: configured.first_run,
        selected: Some(selected.clone()),
    };
    let previous_player = shell.selected_playback().as_deref().cloned();
    let playback::PlaybackProjection { view, notices } = playback;

    *shell.source.configured.borrow_mut() = configured;
    shell
        .selected_ui
        .install(super::selected_ui::SelectedUiSession::new(
            selected,
            view.clone(),
        ));
    shell.attach_selected_ui_roots();

    finish_source_assignment(shell, previous_source, next_source);
    finish_playback_projection(shell, previous_player, view, notices, true);
}

fn apply_selected_library_replacement(
    shell: &Rc<Shell>,
    configured: crate::runtime::source::ConfiguredSources,
    selected: crate::runtime::SelectedLibrary,
) {
    let previous = current_source_presentation(shell);
    let next = SourcePresentation {
        first_run: configured.first_run,
        selected: Some(selected.clone()),
    };
    *shell.source.configured.borrow_mut() = configured;
    shell.selected_ui.replace_library(selected);
    finish_source_assignment(shell, previous, next);
}

fn apply_catalog_publication(shell: &Rc<Shell>, publication: CatalogPublication) {
    let matches_selected = shell.selected_library().as_deref().is_some_and(|selected| {
        selected.source_key == publication.source_key
            && selected.source_session_epoch == publication.source_session_epoch
    });
    if !matches_selected {
        return;
    }
    if let Some(favorite) = publication.favorite {
        shell.apply_favorite_settlement(favorite.target, favorite.requested, favorite.effective);
        if favorite.requested != favorite.effective {
            shell.show_feedback_toast(tr("Could not update favorites"));
        }
        shell.apply_favorite_settlement_to_mounted_route(favorite);
        return;
    }
    let current = shell.navigation.routes.borrow().current().clone();
    match publication.change {
        CatalogChange::Broad => {
            refresh_context_playlist_picker(shell);
            super::navigation::refresh_sidebar_pins(shell);
            if current == Route::Home {
                shell.refresh_mounted_home();
            } else {
                shell.refresh_mounted_catalog();
            }
        }
        CatalogChange::Home => {
            if current == Route::Home {
                shell.refresh_mounted_home();
            }
        }
        CatalogChange::Playlists => {
            refresh_context_playlist_picker(shell);
            super::navigation::refresh_sidebar_pins(shell);
            if matches!(current, Route::Playlists | Route::PlaylistDetail(_)) {
                shell.refresh_mounted_catalog();
            }
        }
        CatalogChange::Album(album) => {
            if current == Route::AlbumDetail(album)
                || matches!(
                    current,
                    Route::ArtistDetail(_)
                        | Route::AlbumArtistDetail(_)
                        | Route::ArtistDiscography(_)
                        | Route::AlbumArtistDiscography(_)
                )
            {
                shell.refresh_mounted_catalog();
            }
        }
    }
}

fn apply_source_notice(shell: &Rc<Shell>, notice: SourceNotice) {
    let matches_selected = shell.selected_library().as_deref().is_some_and(|selected| {
        selected.source_key == notice.source_key
            && selected.source_session_epoch == notice.source_session_epoch
    });
    if !matches_selected {
        return;
    }
    shell.show_feedback_toast(match notice.kind {
        SourceNoticeKind::ServerUnreachable => tr("Server is unreachable"),
        SourceNoticeKind::FavoriteRejected => tr("Could not update favorites"),
    });
}

fn apply_source_operation(shell: &Rc<Shell>, operation: SourceOperation) {
    let previous_operation = shell.source.operation.borrow().clone();
    let previously_blocked = previous_operation.blocks_library();
    let started_blocking = source_operation_started_blocking(&previous_operation, &operation);
    let completed_add = source_add_completed(&previous_operation, &operation);
    *shell.source.operation.borrow_mut() = operation.clone();
    if operation.blocks_library() {
        shell.source.artwork_preparation_revision.set(None);
        shell.chrome.source_refresh_feedback.set_visible(false);
    }
    apply_source_refresh_feedback(shell, &previous_operation, &operation);

    match &operation {
        SourceOperation::Adding { .. } => {
            let first_run = shell.source.configured.borrow().first_run;
            if first_run {
                shell.cancel_startup_route_reveal();
                if !shell.first_run_setup_mounted() {
                    shell.update_layout();
                    shell.render_current_route();
                }
                shell.update_add_server_dialog();
            } else {
                if started_blocking {
                    shell.close_preferences_dialog();
                }
                if !shell.startup.route_revealed.get() {
                    shell.render_startup_loading_view();
                } else {
                    shell.enter_startup_loading();
                }
            }
        }
        SourceOperation::Switching { .. } => {
            if started_blocking {
                shell.close_preferences_dialog();
            }
            shell.startup.route_revealed.set(false);
            if previously_blocked {
                shell.render_startup_loading_view();
            } else {
                shell.enter_startup_loading();
            }
        }
        SourceOperation::Refreshing { .. } => {}
        SourceOperation::Failed {
            message, add_form, ..
        } => {
            warn!(error = %message, "source operation failed");
            if *add_form {
                let first_run = shell.source.configured.borrow().first_run;
                let setup_was_mounted = shell.first_run_setup_mounted();
                if first_run {
                    shell.cancel_startup_route_reveal();
                    if !setup_was_mounted {
                        shell.update_layout();
                        shell.render_current_route();
                    }
                }
                if first_run {
                    shell.update_add_server_dialog();
                } else {
                    shell.restore_add_server_dialog_after_failure();
                }
                if !first_run && previously_blocked {
                    shell.schedule_startup_route_reveal();
                }
            } else if previously_blocked {
                shell.schedule_startup_route_reveal();
            }
        }
        SourceOperation::Idle => {
            if completed_add {
                shell.complete_add_server_dialog();
            }
            shell.update_add_server_dialog();
            if shell.selected_library().is_some()
                && !shell.startup.route_revealed.get()
                && !shell.source.login_screen_active()
            {
                shell.schedule_startup_route_reveal();
            }
        }
    }
}

fn apply_source_refresh_feedback(
    shell: &Shell,
    previous: &SourceOperation,
    operation: &SourceOperation,
) {
    match operation {
        SourceOperation::Refreshing {
            source_id,
            progress,
        } => {
            shell.source.artwork_preparation_revision.set(None);
            let continues = matches!(
                previous,
                SourceOperation::Refreshing {
                    source_id: previous_source_id,
                    progress: previous_progress,
                } if previous_source_id == source_id && previous_progress.stage == progress.stage
            );
            let generation = shell
                .source
                .refresh_feedback_generation
                .get()
                .wrapping_add(1);
            shell.source.refresh_feedback_generation.set(generation);
            shell
                .chrome
                .source_refresh_feedback
                .remove_css_class("error");
            shell
                .chrome
                .source_refresh_feedback_label
                .set_text(&source_progress_text(progress));
            let next_fraction = source_progress_fraction(progress);
            let fraction = if continues {
                shell
                    .chrome
                    .source_refresh_feedback_progress
                    .fraction()
                    .max(next_fraction)
            } else {
                next_fraction
            };
            shell
                .chrome
                .source_refresh_feedback_progress
                .set_fraction(fraction);
            shell.chrome.source_refresh_feedback.set_visible(true);
        }
        SourceOperation::Idle if matches!(previous, SourceOperation::Refreshing { .. }) => {
            finish_source_refresh_feedback(shell, None, Duration::from_millis(1_200));
        }
        SourceOperation::Failed {
            message,
            add_form: false,
            ..
        } if matches!(previous, SourceOperation::Refreshing { .. }) => {
            finish_source_refresh_feedback(shell, Some(message), Duration::from_secs(5));
        }
        SourceOperation::Adding { .. } | SourceOperation::Switching { .. }
            if matches!(previous, SourceOperation::Refreshing { .. }) =>
        {
            shell.source.refresh_feedback_generation.set(
                shell
                    .source
                    .refresh_feedback_generation
                    .get()
                    .wrapping_add(1),
            );
            shell.chrome.source_refresh_feedback.set_visible(false);
        }
        _ => {}
    }
}

fn apply_source_artwork_preparation(
    shell: &Rc<Shell>,
    source_key: library::SourceKey,
    revision: u64,
    progress: Option<SourceProgress>,
) {
    let matches_selected = shell
        .selected_library()
        .as_deref()
        .is_some_and(|selected| selected.source_key == source_key);
    if !matches_selected
        || matches!(
            *shell.source.operation.borrow(),
            SourceOperation::Refreshing { .. }
        )
    {
        return;
    }
    match progress {
        Some(progress) => {
            shell
                .source
                .artwork_preparation_revision
                .set(Some(revision));
            shell
                .chrome
                .source_refresh_feedback
                .remove_css_class("error");
            shell
                .chrome
                .source_refresh_feedback_label
                .set_text(&source_progress_text(&progress));
            shell
                .chrome
                .source_refresh_feedback_progress
                .set_fraction(source_progress_fraction(&progress));
            shell.chrome.source_refresh_feedback.set_visible(true);
        }
        None if shell.source.artwork_preparation_revision.get() == Some(revision) => {
            shell.source.artwork_preparation_revision.set(None);
            finish_source_refresh_feedback(shell, None, Duration::from_millis(1_200));
        }
        None => {}
    }
}

fn finish_source_refresh_feedback(shell: &Shell, error: Option<&str>, delay: Duration) {
    let generation = shell
        .source
        .refresh_feedback_generation
        .get()
        .wrapping_add(1);
    shell.source.refresh_feedback_generation.set(generation);
    if let Some(error) = error {
        shell.chrome.source_refresh_feedback.add_css_class("error");
        shell.chrome.source_refresh_feedback_label.set_text(error);
    }
    shell
        .chrome
        .source_refresh_feedback_progress
        .set_fraction(1.0);
    shell.chrome.source_refresh_feedback.set_visible(true);
    let feedback = shell.chrome.source_refresh_feedback.clone();
    let active_generation = Rc::clone(&shell.source.refresh_feedback_generation);
    glib::timeout_add_local_once(delay, move || {
        if active_generation.get() == generation {
            feedback.set_visible(false);
        }
    });
}

fn source_progress_fraction(progress: &SourceProgress) -> f64 {
    match progress.total {
        Some(0) => 1.0,
        Some(total) => progress.completed.min(total) as f64 / total as f64,
        None if progress.completed == 0 => 0.0,
        None => (progress.completed as f64 / (progress.completed as f64 + 128.0)).min(0.9),
    }
}

fn source_operation_started_blocking(previous: &SourceOperation, next: &SourceOperation) -> bool {
    !previous.blocks_library() && next.blocks_library()
}

fn source_add_completed(previous: &SourceOperation, next: &SourceOperation) -> bool {
    matches!(previous, SourceOperation::Adding { .. }) && matches!(next, SourceOperation::Idle)
}

fn apply_source_discovery(shell: &Rc<Shell>, update: DiscoveryUpdate) {
    *shell.source.discovered_servers.borrow_mut() = update.servers.to_vec();
    *shell.source.discovery_status.borrow_mut() = update.status.clone();
    shell
        .source
        .discovery_running
        .set(matches!(update.status, DiscoveryStatus::Searching));
    if shell.source.configured.borrow().first_run && !shell.first_run_setup_mounted() {
        shell.render_current_route();
    }
    shell.update_add_server_discovery();
}

fn apply_playback_projection(shell: &Rc<Shell>, projection: playback::PlaybackProjection) {
    let previous_player = shell.selected_playback().as_deref().cloned();
    let playback::PlaybackProjection { view, notices } = projection;
    let queue_page_changed = previous_player.as_ref().is_none_or(|previous| {
        previous.queue.revision != view.queue.revision || previous.queue.total != view.queue.total
    });
    shell.selected_ui.replace_player(view.clone());
    finish_playback_projection(shell, previous_player, view, notices, queue_page_changed);
}

fn finish_playback_projection(
    shell: &Rc<Shell>,
    previous_player: Option<playback::PlaybackView>,
    next_player: playback::PlaybackView,
    notices: Vec<playback::PlaybackNotice>,
    queue_page_changed: bool,
) {
    if let Some(folder) = unavailable_local_folder_for_failed_playback(
        previous_player.as_ref(),
        &next_player,
        &shell.source.configured.borrow(),
        shell
            .selected_library()
            .as_deref()
            .map(|selected| &selected.artwork.source_id),
    ) {
        show_local_folder_recovery(shell, folder);
    }
    let previous_media = previous_player
        .as_ref()
        .and_then(|player| player.transport.current.as_ref())
        .map(|current| &current.id);
    let next_media = next_player
        .transport
        .current
        .as_ref()
        .map(|current| &current.id);
    let media_changed = previous_media != next_media;
    let notification_became_sendable = !now_playing_notification_can_send(
        &shell.settings.current.borrow(),
        previous_player.as_ref(),
    ) && now_playing_notification_can_send(
        &shell.settings.current.borrow(),
        Some(&next_player),
    );
    let lyrics_timing_changed = media_changed
        || previous_player
            .as_ref()
            .map(|player| player.transport.state)
            != Some(next_player.transport.state)
        || previous_player
            .as_ref()
            .map(|player| player.transport.position_millis)
            != Some(next_player.transport.position_millis);
    let fullscreen_refresh = fullscreen_playback_refresh(previous_player.as_ref(), &next_player);
    let static_playback_changed = matches!(fullscreen_refresh, FullscreenPlaybackRefresh::Static);
    let position_only =
        bottom_player_can_update_position_only(previous_player.as_ref(), &next_player);
    let media_controls_static_changed =
        media_controls_static_state_changed(previous_player.as_ref(), &next_player);
    let queue_panel_changed = queue_panel_refresh_needed(
        queue_page_changed,
        previous_player
            .as_ref()
            .and_then(|player| player.queue.current_occurrence.as_ref()),
        next_player.queue.current_occurrence.as_ref(),
    );

    if queue_panel_changed && shell.selected_queue().is_some() {
        shell.request_queue_page();
    }

    let previous_route_track = route_current_track(previous_player.as_ref());
    let next_route_track = route_current_track(Some(&next_player));
    if previous_route_track != next_route_track {
        shell.refresh_current_route_now_playing_selections();
        update_sidebar_pin_playback(shell);
    }
    if static_playback_changed {
        shell.sync_bottom_player_favorite();
    }
    shell.maybe_clear_player_seek_preview(&next_player, media_changed);
    if static_playback_changed {
        shell.update_bottom_player();
    } else if position_only {
        shell.update_bottom_player_position();
    } else {
        shell.update_bottom_player_transport();
    }

    let mut media_controls_discontinuity = None;
    let mut notification_started_run = None;
    for notice in notices {
        match notice {
            playback::PlaybackNotice::PositionDiscontinuity(discontinuity) => {
                media_controls_discontinuity = Some(discontinuity);
            }
            playback::PlaybackNotice::RunStarted(run) => {
                notification_started_run = Some(run);
            }
        }
    }

    if now_playing_notification_should_withdraw(
        &shell.settings.current.borrow(),
        Some(&next_player),
    ) {
        shell.withdraw_now_playing_notification();
    }
    if media_changed {
        if let Some(lyrics) = shell.selected_lyrics() {
            lyrics.offset_millis.set(0);
            lyrics.right_pane.clear_follow_scroll_pause();
            lyrics.fullscreen_pane.clear_follow_scroll_pause();
        }
        shell.cancel_scheduled_lyrics_highlight();
        shell.render_lyrics_panel();
    }
    if notification_started_run.is_some_and(|run| {
        next_player
            .transport
            .current
            .as_ref()
            .is_some_and(|media| media.id.run == Some(run))
    }) || notification_became_sendable
    {
        shell.notify_now_playing(Some(&next_player));
    }
    match fullscreen_refresh {
        FullscreenPlaybackRefresh::Static => shell.update_fullscreen_player(),
        FullscreenPlaybackRefresh::Visualizer | FullscreenPlaybackRefresh::None => {}
    }
    if fullscreen_refresh != FullscreenPlaybackRefresh::None {
        shell.sync_visualizer_state();
    }
    if lyrics_timing_changed {
        shell.update_lyrics_highlight();
    }
    if media_controls_static_changed {
        shell.update_media_controls_after(media_controls_discontinuity);
    } else {
        shell.update_media_controls_position_after(
            next_player
                .transport
                .current
                .as_ref()
                .map(|_| next_player.transport.position_millis),
            media_controls_discontinuity,
        );
    }
    if queue_panel_changed {
        shell.schedule_queue_panel_render();
    }
}

fn unavailable_local_folder_for_failed_playback(
    previous: Option<&playback::PlaybackView>,
    next: &playback::PlaybackView,
    configured: &ConfiguredSources,
    selected_source_id: Option<&sources::SourceId>,
) -> Option<String> {
    let error = next.transport.error.as_ref()?;
    if previous.and_then(|player| player.transport.error.as_ref()) == Some(error) {
        return None;
    }
    let source_id = selected_source_id?;
    if !configured
        .sources
        .iter()
        .any(|source| &source.id == source_id && source.kind == "local")
    {
        return None;
    }
    let media_uri = next
        .transport
        .current
        .as_ref()?
        .track
        .media_uri
        .as_deref()?;
    let file = gio::File::for_uri(media_uri);
    let source_path = file.path()?;
    let source_path = source_path.to_str()?;
    unavailable_local_folder_for_path(&configured.local_folders, source_path)
}

fn unavailable_local_folder_for_path(folders: &[LocalFolder], source_path: &str) -> Option<String> {
    let source_path = Path::new(source_path);
    folders
        .iter()
        .find(|folder| {
            let root = Path::new(&folder.path);
            source_path.starts_with(root) && std::fs::read_dir(root).is_err()
        })
        .map(|folder| folder.path.clone())
}

fn show_local_folder_recovery(shell: &Rc<Shell>, folder: String) {
    let toast = adw::Toast::new(&tr("Local music folder is unavailable"));
    toast.set_button_label(Some(&tr("Locate Folder")));
    toast.set_timeout(0);
    let recovery_shell = Rc::clone(shell);
    toast.connect_button_clicked(move |toast| {
        toast.dismiss();
        crate::preferences::locate_local_folder(&recovery_shell, folder.clone());
    });
    shell.chrome.toast_overlay.add_toast(toast);
}

fn queue_panel_refresh_needed(
    page_changed: bool,
    previous_current: Option<&playback::OccurrenceId>,
    next_current: Option<&playback::OccurrenceId>,
) -> bool {
    page_changed || previous_current != next_current
}

fn bottom_player_can_update_position_only(
    previous: Option<&playback::PlaybackView>,
    next: &playback::PlaybackView,
) -> bool {
    previous.is_some_and(|previous| {
        previous.transport.source_id == next.transport.source_id
            && previous.transport.current == next.transport.current
            && previous.transport.effective_state() == next.transport.effective_state()
            && previous.transport.duration_millis == next.transport.duration_millis
            && previous.transport.buffering_percent == next.transport.buffering_percent
            && previous.transport.error == next.transport.error
            && previous.controls == next.controls
    })
}

fn media_controls_static_state_changed(
    previous: Option<&playback::PlaybackView>,
    next: &playback::PlaybackView,
) -> bool {
    previous.is_none_or(|previous| {
        previous.transport.current != next.transport.current
            || previous.transport.effective_state() != next.transport.effective_state()
            || previous.transport.duration_millis != next.transport.duration_millis
            || previous.controls.repeat_mode != next.controls.repeat_mode
            || previous.controls.shuffle_enabled != next.controls.shuffle_enabled
            || previous.controls.auto_dj_enabled != next.controls.auto_dj_enabled
            || previous.controls.volume != next.controls.volume
            || previous.queue.next_occurrence != next.queue.next_occurrence
    })
}

fn apply_waveform(shell: &Rc<Shell>, waveform: WaveformProjection) {
    shell.selected_ui.set_waveform(waveform);
    shell.update_bottom_player_transport();
}

fn apply_lyrics_event(shell: &Rc<Shell>, event: lyrics::LyricsEvent) {
    match event {
        lyrics::LyricsEvent::Current(projection) => shell.apply_current_lyrics(projection),
        lyrics::LyricsEvent::SearchFinished {
            media_id,
            query,
            result,
        } => match result {
            Ok(results) => shell.apply_lyrics_search_results(
                media_id,
                query.artist_name,
                query.track_name,
                results,
            ),
            Err(error) => shell.apply_lyrics_search_failed(
                media_id,
                query.artist_name,
                query.track_name,
                error,
            ),
        },
        lyrics::LyricsEvent::Saved { media_id, path } => {
            if current_playback_media_id(shell.selected_playback().as_deref()).as_ref()
                == Some(&media_id)
            {
                shell.apply_lyrics_saved(media_id, path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use playback::{
        ControlsView, PlaybackView, QueueSummaryView, RepeatMode, TransportStatus, TransportView,
    };
    use sources::SourceId;

    use super::{
        bottom_player_can_update_position_only, media_controls_static_state_changed,
        queue_panel_refresh_needed, source_add_completed, source_operation_started_blocking,
        source_progress_fraction, unavailable_local_folder_for_path,
    };
    use crate::runtime::source::{
        LocalFolder, SourceOperation, SourceProgress, SourceProgressStage,
    };

    fn adding() -> SourceOperation {
        SourceOperation::Adding {
            progress: SourceProgress {
                stage: SourceProgressStage::Connecting,
                completed: 0,
                total: None,
            },
        }
    }

    #[test]
    fn a_failed_add_keeps_its_retry_form_until_an_add_reaches_idle() {
        assert!(!source_add_completed(
            &adding(),
            &SourceOperation::Failed {
                source_id: None,
                message: "Connection failed".to_string(),
                add_form: true,
            }
        ));
        assert!(source_add_completed(&adding(), &SourceOperation::Idle));
    }

    #[test]
    fn source_progress_does_not_restart_the_blocking_transition() {
        assert!(source_operation_started_blocking(
            &SourceOperation::Idle,
            &adding()
        ));
        assert!(!source_operation_started_blocking(&adding(), &adding()));
        assert!(source_operation_started_blocking(
            &SourceOperation::Idle,
            &SourceOperation::Switching {
                target: SourceId::new("target"),
                progress: SourceProgress {
                    stage: SourceProgressStage::Connecting,
                    completed: 0,
                    total: None,
                },
            }
        ));
    }

    #[test]
    fn source_progress_follows_the_visible_stage_ratio() {
        let fraction = |stage, completed, total| {
            source_progress_fraction(&SourceProgress {
                stage,
                completed,
                total,
            })
        };
        assert_eq!(fraction(SourceProgressStage::Connecting, 0, None), 0.0);
        assert_eq!(fraction(SourceProgressStage::Files, 10, Some(10)), 1.0);
        assert_eq!(fraction(SourceProgressStage::Tracks, 0, Some(10)), 0.0);
        assert_eq!(fraction(SourceProgressStage::Tracks, 5, Some(10)), 0.5);
        assert_eq!(
            fraction(SourceProgressStage::Tracks, 52, Some(2_584)),
            52.0 / 2_584.0
        );
        assert_eq!(fraction(SourceProgressStage::Finalizing, 1, Some(1)), 1.0);
    }

    #[test]
    fn queue_panel_refresh_ignores_unchanged_playback_ticks() {
        let current = playback::OccurrenceId::new("current");
        let next = playback::OccurrenceId::new("next");

        assert!(!queue_panel_refresh_needed(
            false,
            Some(&current),
            Some(&current)
        ));
        assert!(queue_panel_refresh_needed(
            false,
            Some(&current),
            Some(&next)
        ));
        assert!(queue_panel_refresh_needed(
            true,
            Some(&current),
            Some(&current)
        ));
    }

    #[test]
    fn local_folder_recovery_requires_the_tracks_root_to_be_unavailable() {
        let directory = tempfile::tempdir().expect("temporary local folder parent");
        let root = directory.path().join("Music");
        std::fs::create_dir(&root).expect("create Local root");
        let root_text = root.to_string_lossy().into_owned();
        let folders = [LocalFolder {
            path: root_text.clone(),
        }];
        let track = root.join("ArtistRow").join("TrackRow.flac");

        assert_eq!(
            unavailable_local_folder_for_path(&folders, &track.to_string_lossy()),
            None
        );

        std::fs::remove_dir(&root).expect("make Local root unavailable");
        assert_eq!(
            unavailable_local_folder_for_path(&folders, &track.to_string_lossy()),
            Some(root_text)
        );
    }

    #[test]
    fn playback_ticks_only_update_position_owned_surfaces() {
        let previous = idle_playback_view();
        let mut tick = previous.clone();
        tick.transport.position_millis = 500;

        assert!(bottom_player_can_update_position_only(
            Some(&previous),
            &tick
        ));
        assert!(!media_controls_static_state_changed(Some(&previous), &tick));

        let mut state_change = tick.clone();
        state_change.transport.state = TransportStatus::Playing;
        state_change.transport.desired_playing = true;
        assert!(!bottom_player_can_update_position_only(
            Some(&tick),
            &state_change
        ));
        assert!(media_controls_static_state_changed(
            Some(&tick),
            &state_change
        ));

        let mut duration_change = tick.clone();
        duration_change.transport.duration_millis = 2_000;
        assert!(media_controls_static_state_changed(
            Some(&tick),
            &duration_change
        ));

        let mut queue_change = tick.clone();
        queue_change.queue.next_occurrence = Some(playback::OccurrenceId::new("next"));
        assert!(bottom_player_can_update_position_only(
            Some(&tick),
            &queue_change
        ));
        assert!(media_controls_static_state_changed(
            Some(&tick),
            &queue_change
        ));
    }

    fn idle_playback_view() -> PlaybackView {
        PlaybackView {
            queue: QueueSummaryView {
                revision: 0,
                total: 0,
                current_occurrence: None,
                current_index: None,
                current_position: None,
                next_occurrence: None,
            },
            transport: TransportView {
                source_id: library::SourceKey::from_raw(1),
                current: None,
                state: TransportStatus::Stopped,
                desired_playing: false,
                position_millis: 0,
                duration_millis: 0,
                can_seek: false,
                buffering_percent: None,
                error: None,
            },
            controls: ControlsView {
                repeat_mode: RepeatMode::Off,
                shuffle_enabled: false,
                auto_dj_enabled: false,
                volume: 1.0,
                muted: false,
                audio_output: None,
                playback_output: playback::PlaybackOutput::Local,
            },
        }
    }
}
