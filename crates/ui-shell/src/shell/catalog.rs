use super::Shell;
use adw::prelude::*;
use std::{cell::RefCell, rc::Rc};
use ui_shared::route::Route;
impl Shell {
    pub(super) fn build_catalog(
        self: &Rc<Self>,
        route: &Route,
        source_id: Option<&sources::SourceId>,
    ) -> Rc<ui_library::CatalogUi> {
        let selected = self.selected_library().as_deref().cloned();
        let operations = selected
            .as_ref()
            .map(|selected| selected.operations.clone());
        let history_source_name = {
            let configured = self.source.configured.borrow();
            let source = configured
                .sources
                .iter()
                .find(|source| Some(&source.id) == configured.selected_source_id.as_ref());
            source.map(ui_shared::source_labels::configured_source_display_name)
        };
        Rc::new(ui_library::CatalogUi {
            library: self.products.library.clone(),
            runtime: self.products.runtime.clone(),
            source: self.products.source.clone(),
            queue: self.products.playback.queue.clone(),
            radio: self.products.playback.radio.clone(),
            settings: Rc::clone(&self.settings),
            artwork: Rc::clone(&self.artwork),
            downloads: Rc::clone(&self.downloads),
            media_menus: Rc::clone(&self.media_menus),
            selected,
            source_label: {
                let weak = Rc::downgrade(self);
                Rc::new(move |source_id| {
                    let Some(shell) = weak.upgrade() else {
                        return ui_shared::source_labels::source_display_label(None);
                    };
                    let configured = shell.source.configured.borrow();
                    ui_shared::source_labels::source_display_label(
                        configured
                            .sources
                            .iter()
                            .find(|source| Some(source.id.as_str()) == source_id),
                    )
                })
            },
            favorites: self
                .selected_ui
                .session()
                .map(|session| Rc::clone(&session.favorites)),
            window: self.chrome.window.downgrade(),
            can_back: self.navigation.routes.borrow().can_back(),
            current: RefCell::new(None),
            current_track_selections: RefCell::new(Vec::new()),
            track_selections: RefCell::new(Vec::new()),
            playlist_entry_selection: RefCell::new(None),
            has_current_track: ui_player::state::current_playback_track(
                self.player_ui.selected_playback().as_deref(),
            )
            .is_some(),
            history_source_name,
            update_smart_playlist_pin: {
                let weak = Rc::downgrade(self);
                Rc::new(move |summary| {
                    if let Some(shell) = weak.upgrade() {
                        super::navigation::update_sidebar_smart_playlist_pin_metadata(
                            &shell, summary,
                        );
                    }
                })
            },
            is_current: {
                let weak = Rc::downgrade(self);
                let route = route.clone();
                let source_id = source_id.cloned();
                Rc::new(move || {
                    weak.upgrade().is_some_and(|shell| {
                        shell.navigation.routes.borrow().current() == &route
                            && source_id.as_ref().is_none_or(|id| {
                                shell
                                    .selected_library()
                                    .as_deref()
                                    .is_some_and(|selected| &selected.source_id == id)
                            })
                    })
                })
            },
            route_width: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    weak.upgrade()
                        .map_or(1, |shell| super::layout::route_content_width(&shell))
                })
            },
            history_source_id: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    weak.upgrade().and_then(|shell| {
                        shell.source.configured.borrow().selected_source_id.clone()
                    })
                })
            },
            reserve_window_controls: {
                let weak = Rc::downgrade(self);
                Rc::new(move |host, spacing| {
                    if let Some(shell) = weak.upgrade() {
                        let reservation = shell.chrome.window_controls.end_width_reservation();
                        reservation.set_margin_start(spacing);
                        host.append(&reservation);
                        shell
                            .right_panel
                            .right_panel_slot
                            .bind_property("visible", host, "visible")
                            .sync_create()
                            .invert_boolean()
                            .build();
                    }
                })
            },
            refresh_home_section: {
                Rc::new(move |kind| {
                    if let Some(operations) = &operations {
                        if kind == rufin_core::settings::HomeSectionKind::NewlyAdded {
                            operations.refresh_library(
                                rufin_core::runtime::LibraryRefreshTrigger::NewlyAdded,
                            );
                        } else {
                            operations.refresh_home(kind);
                        }
                    }
                })
            },
            go_back: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.go_back();
                    }
                })
            },
            reconcile: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.reconcile_mounted_route();
                    }
                })
            },
            refresh: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.replace_current_route_when_ready();
                    }
                })
            },
            refresh_catalog: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.refresh_mounted_catalog();
                    }
                })
            },
            present_full_artwork: {
                let weak = Rc::downgrade(self);
                Rc::new(move |artwork| {
                    if let Some(shell) = weak.upgrade() {
                        shell.present_full_artwork(artwork);
                    }
                })
            },
            present_selected_dialog: {
                let weak = Rc::downgrade(self);
                Rc::new(move |dialog| {
                    if let Some(shell) = weak.upgrade() {
                        shell.present_selected_dialog(dialog);
                    }
                })
            },
            add_current_to_playlist: {
                let weak = Rc::downgrade(self);
                Rc::new(move |playlist, destination| {
                    if let Some(shell) = weak.upgrade() {
                        super::catalog_actions::add_current_to_playlist(
                            &shell,
                            playlist,
                            destination,
                        );
                    }
                })
            },
            set_favorite_category: {
                let weak = Rc::downgrade(self);
                Rc::new(move |category| {
                    if let Some(shell) = weak.upgrade() {
                        shell.set_favorite_category(category);
                    }
                })
            },
            cycle_favorite_category: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell
                            .set_favorite_category(shell.navigation.favorite_category.get().next());
                    }
                })
            },
            playlist_context_menu: {
                let weak = Rc::downgrade(self);
                Rc::new(move |target, playlist, play, position| {
                    if let Some(shell) = weak.upgrade() {
                        ui_shared::media_menus::present_playlist_context_menu(
                            target,
                            &shell.media_menus,
                            playlist,
                            play,
                            position,
                            ui_player::state::current_playback_track(
                                shell.player_ui.selected_playback().as_deref(),
                            )
                            .map(|track| track.media_uri),
                        );
                    }
                })
            },
        })
    }
}
