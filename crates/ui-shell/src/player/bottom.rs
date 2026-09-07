use crate::shell::player_menus::{
    install_current_track_context_menu, present_current_track_context_menu,
};

use ui_player::playback_settings::configure_playback_settings_popover;
use ui_shared::media_drag::{MediaDragSource, install_compact_media_drag_source};

use crate::shell::Shell;
use adw::prelude::*;
use std::rc::Rc;
use ui_shared::{
    detail_links::{DetailLinks, track_artist_links},
    interactions::add_widget_click,
    route::Route,
};
impl Shell {
    pub(crate) fn refresh_bottom_player_owner_details(self: &Rc<Self>) {
        let player = self.player_ui.selected_playback();
        let Some(current) = player
            .as_deref()
            .and_then(|player| player.transport.current.as_ref())
        else {
            self.player_ui
                .apply_current_control_details(None, false, None, false, None);
            return;
        };
        let media_uri = current.media_uri.clone();
        let database = self.products.library.clone();
        let request_uri = media_uri.clone();
        let task = self.products.runtime.spawn(async move {
            let cancellation = library::ReadCancellation::new();
            let track = database
                .track_row_by_uri(&request_uri, &cancellation)
                .await?;
            let state = if track.is_none() {
                database
                    .user_media_state(&request_uri, &cancellation)
                    .await?
            } else {
                None
            };
            Ok::<_, library::LibraryError>((track, state))
        });
        let shell = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let Ok(Ok((track, state))) = task.await else {
                return;
            };
            let Some(shell) = shell.upgrade() else { return };
            let (favorite, rating) = track
                .as_ref()
                .map(|track| {
                    (
                        track.favorite,
                        track.rating.and_then(|value| u8::try_from(value).ok()),
                    )
                })
                .unwrap_or_else(|| {
                    state
                        .map(|(favorite, rating)| (favorite.unwrap_or(false), rating))
                        .unwrap_or_default()
                });
            let favorite = shell.projected_track_favorite(&media_uri, favorite);
            let half_stars = shell.half_stars_enabled(
                &media_uri,
                track.as_ref().map(|track| track.source_id.as_str()),
            );
            let links = track.as_ref().map(|track| {
                (
                    track_artist_links(track),
                    DetailLinks::route(
                        &track.album,
                        track.album_media_uri.clone().map(Route::AlbumDetail),
                    ),
                )
            });
            shell.player_ui.apply_current_control_details(
                Some(&media_uri),
                favorite,
                rating,
                half_stars,
                links,
            );
        });
    }
}
fn install_now_playing_drag_source(
    target: &impl IsA<gtk::Widget>,
    artwork: &gtk::Picture,
    shell: &Rc<Shell>,
) {
    let drag_shell = Rc::downgrade(shell);
    install_compact_media_drag_source(target, artwork, move || {
        let shell = drag_shell.upgrade()?;
        let player = shell.player_ui.selected_playback()?;
        let current = player.transport.current.as_ref()?;
        let source = MediaDragSource::media_uris([current.media_uri.clone()]);
        Some((source, current.title.clone()))
    });
}
pub(crate) fn connect_player_controls(shell: &Rc<Shell>) {
    ui_player::bottom::connect_player_controls(&shell.player_ui);
    let controls = &shell.player_ui.views.player_controls;
    let drag_artwork = controls.cover.drag_paintable_source();
    install_now_playing_drag_source(&controls.cover.area, &drag_artwork, shell);
    install_now_playing_drag_source(&controls.title, &drag_artwork, shell);
    let title_shell = Rc::clone(shell);
    add_widget_click(controls.title.upcast_ref(), move || {
        title_shell.player_ui.navigate_current_track_album();
    });
    install_current_track_context_menu(&shell.player_ui.views.player_controls.cover.area, shell);
    let menu_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .menu_button
        .connect_clicked(move |button| present_current_track_context_menu(button, &menu_shell));
    let fullscreen_shell = Rc::clone(shell);
    add_widget_click(
        shell
            .player_ui
            .views
            .player_controls
            .cover
            .area
            .upcast_ref(),
        move || {
            fullscreen_shell.player_ui.toggle_fullscreen_player();
        },
    );

    let random_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .random_button
        .connect_clicked(move |_| super::random_play::present_random_play_dialog(&random_shell));

    let queue_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .queue_button
        .connect_clicked(move |_| queue_shell.toggle_right_panel());

    let favorite_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .favorite_button
        .connect_clicked(move |_| favorite_shell.toggle_current_track_favorite());

    let rating_shell = Rc::clone(shell);
    shell
        .player_ui
        .views
        .player_controls
        .rating
        .connect_commit(move |rating| rating_shell.set_current_track_rating(rating));

    configure_playback_settings_popover(
        &shell.player_ui.views.player_controls.settings_button,
        &shell.player_ui,
        {
            let weak = Rc::downgrade(shell);
            Rc::new(move |lyrics, visualizer| {
                if let Some(shell) = weak.upgrade() {
                    shell.set_right_panel_media_visibility(lyrics, visualizer);
                }
            })
        },
    );
}
