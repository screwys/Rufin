use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{
    Library, MusicFolderId, PlaylistDetail, PlaylistEdit, PlaylistId, PlaylistSummary, RadioSeed,
    SmartPlaylistDetail, SmartPlaylistId, SmartPlaylistSummary,
};
use adw::prelude::*;
use artwork::ArtworkBinding;

use crate::LibraryListKey;
use crate::format_duration_units;
use crate::localization::{bind_label_text_with, localized_label};
use crate::shell::Shell;
use crate::shell::actions::{ADD_ICON, EDIT_ICON};
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::{LatestMountedRouteRead, MountedRoute, SelectedRouteIdentity};
use localization::{msgid, tr, track_count_text};
use playback::RadioPlayRequest;

use super::collection_context::{
    present_playlist_context_menu, present_smart_playlist_context_menu,
};
use super::collections::{CollectionPlay, library_route_inset};
use super::detail_showcase::{
    PlaylistDetailShowcase, detail_action_button, detail_action_row, detail_delete_button,
    detail_genre_pill_button, detail_playback_controls, detail_radio_button, detail_title_label,
    playlist_detail_showcase,
};
use super::library_fields::smart_playlist_display_name;
use super::playlist_entry_model::{
    PlaylistEntryProjectionRequest, PreparedPlaylistEntries, prepare_playlist_entry_projection,
};
use super::route::Route;
use super::route_layout::{PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_route_inner_width};
use super::track_model::{
    PreparedTrackProjection, TrackProjectionRequest, prepare_track_projection,
};

const PLAYLIST_DETAIL_COMPACT_WIDTH: i32 = 760;
const PLAYLIST_DETAIL_COVER_ONLY_WIDTH: i32 = 420;
const PLAYLIST_DETAIL_TINY_COVER_SIZE: i32 = 150;
const PLAYLIST_DETAIL_WIDE_COVER_SIZE: i32 = 208;

#[derive(Clone)]
struct SmartPlaylistDetailReadRequest {
    identity: SelectedRouteIdentity,
    tracks: TrackProjectionRequest,
}

struct PreparedSmartPlaylistDetail {
    summary: SmartPlaylistSummary,
    tracks: PreparedTrackProjection,
}

#[derive(Clone)]
struct PlaylistDetailReadRequest {
    identity: SelectedRouteIdentity,
    entries: PlaylistEntryProjectionRequest,
}

struct PreparedPlaylistDetail {
    summary: PlaylistSummary,
    entries: PreparedPlaylistEntries,
}

pub(crate) fn playlist_detail_compact_for_width(width: i32) -> bool {
    width < PLAYLIST_DETAIL_COMPACT_WIDTH
}

pub(crate) fn playlist_cover_size(width: i32) -> i32 {
    if width < PLAYLIST_DETAIL_COVER_ONLY_WIDTH {
        width.clamp(96, PLAYLIST_DETAIL_TINY_COVER_SIZE)
    } else if playlist_detail_compact_for_width(width) {
        PLAYLIST_DETAIL_TINY_COVER_SIZE
            + ((width - PLAYLIST_DETAIL_COVER_ONLY_WIDTH)
                * (PLAYLIST_DETAIL_WIDE_COVER_SIZE - PLAYLIST_DETAIL_TINY_COVER_SIZE)
                / (PLAYLIST_DETAIL_COMPACT_WIDTH - PLAYLIST_DETAIL_COVER_ONLY_WIDTH))
    } else {
        PLAYLIST_DETAIL_WIDE_COVER_SIZE
    }
}

impl Shell {
    fn playlist_detail_kind_row(
        self: &Rc<Self>,
        genres: &[Arc<::library::Genre>],
        radio_playlist: Option<PlaylistId>,
    ) -> gtk::Box {
        let kind = localized_label("Playlist");
        kind.add_css_class("eyebrow");
        kind.set_xalign(0.0);
        kind.set_halign(gtk::Align::Start);
        kind.set_valign(gtk::Align::Center);
        kind.set_margin_end(6);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        row.add_css_class("album-detail-kind-row");
        row.add_css_class("album-detail-genre-row");
        row.set_valign(gtk::Align::Center);
        row.set_halign(gtk::Align::Start);
        row.append(&kind);
        if let Some(playlist_id) = radio_playlist {
            let radio = detail_radio_button();
            let controller = self.products.playback.radio.clone();
            radio.connect_clicked(move |_| {
                controller.play_radio(RadioPlayRequest::now(RadioSeed::Playlist(
                    playlist_id.clone(),
                )));
            });
            row.append(&radio);
        }

        for genre in genres {
            let button = detail_genre_pill_button(genre.name.trim());
            let shell = Rc::clone(self);
            let genre_id = genre.id.clone();
            button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(genre_id.clone())));
            row.append(&button);
        }
        row
    }

    pub(crate) fn smart_playlist_detail_route(
        self: &Rc<Self>,
        smart_playlist_id: SmartPlaylistId,
        detail: Option<Arc<SmartPlaylistDetail>>,
        loaded: Arc<Library>,
        music_folder_id: Option<MusicFolderId>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Smart Playlist"), msgid("This isn't available")),
            );
        };
        let initial_summary = detail.summary.clone();
        let header = Rc::new(RefCell::new(initial_summary.clone()));
        let seed = stable_seed(smart_playlist_id.as_str());
        let context_id = format!("smart-playlist:{}", smart_playlist_id.as_str());
        let (tracks_widget, tracks, tracks_toolbar) = self.scrolling_track_projection(
            detail.tracks.clone(),
            LibraryListKey::SmartPlaylistTracks,
            "smart-playlist-detail",
            context_id.clone(),
        );
        let content_width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 18);
        wrapper.add_css_class("route-content");
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let artwork = ArtworkBinding::smart_playlist_slots(
            &initial_summary.smart_playlist,
            &initial_summary.representative_albums,
        );
        let cover = self.cover_group_projection_for_artwork(
            &artwork,
            playlist_cover_size(content_width),
            playlist_cover_size(i32::MAX),
        );
        cover.widget().add_css_class("playlist-detail-cover");
        let title = detail_title_label(&smart_playlist_display_name(
            &initial_summary.smart_playlist,
        ));
        let kind_row = self.playlist_detail_kind_row(&[], None);
        let summary = PlaylistDetailSummary::new(
            initial_summary.track_count,
            initial_summary.duration_seconds,
        );
        let actions = detail_action_row();
        actions.set_halign(gtk::Align::Start);

        let controller = self.products.playback.queue.clone();
        let play_tracks = tracks.clone();
        let play_context_id = context_id.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled_start| {
            if let Some(request) =
                play_tracks.source_play_request(placement, &play_context_id, shuffled_start)
            {
                controller.play_loaded(request);
            }
        });
        let cover_controls = detail_playback_controls(
            &actions,
            msgid("Play smart playlist"),
            None,
            false,
            Rc::clone(&play),
        );

        let rename = detail_action_button(EDIT_ICON, "Rename");
        let shell = Rc::clone(self);
        let rename_header = Rc::clone(&header);
        rename.connect_clicked(move |_| {
            shell.rename_smart_playlist_dialog((*rename_header.borrow().smart_playlist).clone());
        });
        actions.append(&rename);
        let delete = detail_delete_button("Delete");
        let delete_shell = Rc::clone(self);
        let delete_header = Rc::clone(&header);
        delete.connect_clicked(move |_| {
            let playlist_id = delete_header.borrow().smart_playlist.id.clone();
            if let Some(source) = delete_shell.selected_source_operations() {
                source.delete_smart_playlist(playlist_id);
            }
            delete_shell.navigate(Route::SmartPlaylists);
        });
        actions.append(&delete);

        let menu_shell = Rc::clone(self);
        let menu_header = Rc::clone(&header);
        let menu_play = Rc::clone(&play);
        let context_menu: crate::interactions::ContextMenuOpen =
            Rc::new(move |target, position| {
                let playlist = menu_header.borrow().clone();
                present_smart_playlist_context_menu(
                    target,
                    &menu_shell,
                    playlist,
                    Some(Rc::clone(&menu_play)),
                    position,
                );
            });

        let showcase = playlist_detail_showcase(
            self,
            PlaylistDetailShowcase {
                seed,
                initial_width: content_width,
                cover: cover.clone(),
                cover_controls,
                context_menu: Some(context_menu),
                kind_row: kind_row.upcast(),
                title: title.clone().upcast(),
                summary: summary.widget(),
                actions: actions.upcast(),
            },
        );
        wrapper.append(&library_route_inset(showcase));

        let tracks_stack = gtk::Stack::new();
        tracks_stack.set_hexpand(true);
        tracks_stack.set_vexpand(true);
        tracks_stack.add_named(&tracks_widget, Some("tracks"));
        tracks_stack.add_named(
            &library_route_inset(self.placeholder_view("Tracks", msgid("No matching tracks"))),
            Some("empty"),
        );
        tracks_stack.set_visible_child_name(if tracks.source_is_empty() {
            "empty"
        } else {
            "tracks"
        });
        wrapper.append(&tracks_stack);

        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("detail"));
        route_stack.add_named(
            &self.placeholder_view(msgid("Smart Playlist"), msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("detail");

        let identity = self.mounted_route_read_identity(
            Route::SmartPlaylistDetail(smart_playlist_id.clone()),
            &loaded,
            music_folder_id.clone(),
        );
        let apply = {
            let shell = Rc::clone(self);
            let route_stack = route_stack.clone();
            let header = Rc::clone(&header);
            let title = title.clone();
            let summary = summary.clone();
            let cover = cover.clone();
            let tracks_stack = tracks_stack.clone();
            let tracks = tracks.clone();
            Rc::new(
                move |request: SmartPlaylistDetailReadRequest,
                      result: Result<Option<PreparedSmartPlaylistDetail>, String>| {
                    if !shell.mounted_route_read_is_current(&request.identity) {
                        return;
                    }
                    let next = match result {
                        Ok(next) => next,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "failed to read the mounted Smart Playlist detail"
                            );
                            route_stack.set_visible_child_name("missing");
                            return;
                        }
                    };
                    let Some(next) = next else {
                        route_stack.set_visible_child_name("missing");
                        return;
                    };
                    if !tracks.replace_prepared(next.tracks) {
                        return;
                    }
                    title.set_text(&smart_playlist_display_name(&next.summary.smart_playlist));
                    summary.set(next.summary.track_count, next.summary.duration_seconds);
                    summary.widget().set_visible(true);
                    cover.replace(
                        &shell,
                        &ArtworkBinding::smart_playlist_slots(
                            &next.summary.smart_playlist,
                            &next.summary.representative_albums,
                        ),
                    );
                    tracks_stack.set_visible_child_name(if tracks.source_is_empty() {
                        "empty"
                    } else {
                        "tracks"
                    });
                    header.replace(next.summary.clone());
                    route_stack.set_visible_child_name("detail");
                },
            )
        };
        let load = {
            let loaded = Arc::clone(&loaded);
            let smart_playlist_id = smart_playlist_id.clone();
            let music_folder_id = music_folder_id.clone();
            Arc::new(move |request: &SmartPlaylistDetailReadRequest| {
                load_smart_playlist_detail(&loaded, &smart_playlist_id, music_folder_id.as_ref())
                    .and_then(|detail| {
                        detail
                            .map(|detail| {
                                prepare_track_projection(
                                    detail.tracks.clone(),
                                    request.tracks.clone(),
                                )
                                .map(|tracks| PreparedSmartPlaylistDetail {
                                    summary: detail.summary.clone(),
                                    tracks,
                                })
                                .map_err(|error| error.to_string())
                            })
                            .transpose()
                    })
            })
        };
        let read = LatestMountedRouteRead::new_with_request(apply, load, "Smart Playlist detail");
        {
            let read = Rc::downgrade(&read);
            let identity = identity.clone();
            tracks.connect_search_request(move |tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with_if_running(SmartPlaylistDetailReadRequest {
                    identity: identity.clone(),
                    tracks,
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let tracks = tracks.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move || {
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::SmartPlaylistTracks);
                tracks.apply_library_list_settings(LibraryListKey::SmartPlaylistTracks, &settings);
                tracks_toolbar.apply(LibraryListKey::SmartPlaylistTracks, &settings);
                let request = SmartPlaylistDetailReadRequest {
                    identity: identity.clone(),
                    tracks: tracks.projection_request(),
                };
                read.request_with_if_running(request);
            })
        };
        let update = {
            let tracks = tracks.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                let replacements = update.change.tracks.as_slice();
                let request = tracks.projection_request();
                let replacement_changes_visible_facts = replacements.iter().any(|replacement| {
                    !replacement.activity_only || request.settings.uses_track_activity()
                });
                if update.change.smart_playlists.contains(&smart_playlist_id)
                    || replacement_changes_visible_facts
                {
                    read.request_with(SmartPlaylistDetailReadRequest {
                        identity: identity.clone(),
                        tracks: request,
                    });
                }
            })
        };
        MountedRoute::new(route_stack.upcast(), resume)
            .with_item_navigation(tracks.item_navigation())
            .with_library_update(update)
    }

    pub(crate) fn playlist_detail_route(
        self: &Rc<Self>,
        playlist_id: PlaylistId,
        detail: Option<PlaylistDetail>,
        initial_positions: Vec<u32>,
        loaded: Arc<Library>,
    ) -> MountedRoute {
        let settings = self.settings.current.borrow().clone();
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view("Playlist", msgid("This isn't available")),
            );
        };
        let header = Rc::new(RefCell::new(detail.summary.clone()));
        let applied_playlist_artwork = Rc::new(Cell::new(settings.prefer_server_playlist_covers));
        let seed = stable_seed(playlist_id.as_str());
        let content_width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 20);
        wrapper.add_css_class("route-content");
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let cover = self.cover_group_projection_for_artwork(
            &playlist_artwork(&detail.summary, settings.prefer_server_playlist_covers),
            playlist_cover_size(content_width),
            playlist_cover_size(i32::MAX),
        );
        cover.widget().add_css_class("playlist-detail-cover");
        let title = detail_title_label(&detail.summary.playlist.name);
        let kind_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        kind_slot.append(
            &self.playlist_detail_kind_row(&detail.summary.genres, Some(playlist_id.clone())),
        );
        let summary =
            PlaylistDetailSummary::new(detail.summary.track_count, detail.summary.duration_seconds);
        let entries =
            self.playlist_entries_view(playlist_id.clone(), detail.entries, initial_positions);

        let actions = detail_action_row();
        actions.set_halign(gtk::Align::Start);
        let controller = self.products.playback.queue.clone();
        let play_entries = entries.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled_start| {
            if let Some(request) = play_entries.source_play_request(placement, shuffled_start) {
                controller.play_loaded(request);
            }
        });
        let cover_controls = detail_playback_controls(
            &actions,
            msgid("Play playlist"),
            None,
            false,
            Rc::clone(&play),
        );

        let rename = detail_action_button(EDIT_ICON, "Rename");
        let shell = Rc::clone(self);
        let rename_header = Rc::clone(&header);
        let rename_id = playlist_id.clone();
        rename.connect_clicked(move |_| {
            shell.rename_playlist_dialog(
                rename_id.clone(),
                rename_header.borrow().playlist.name.clone(),
            );
        });
        actions.append(&rename);

        let add_current = detail_action_button(ADD_ICON, "Add current");
        add_current.set_sensitive(
            self.selected_playback()
                .as_deref()
                .and_then(|player| player.transport.current.as_ref())
                .is_some(),
        );
        let shell = Rc::clone(self);
        let add_id = playlist_id.clone();
        add_current.connect_clicked(move |_| {
            let track_id = shell
                .selected_playback()
                .as_deref()
                .and_then(|player| player.transport.current.as_ref())
                .map(|entry| entry.track.id.clone());
            if let Some(track_id) = track_id {
                if let Some(source) = shell.selected_source_operations() {
                    source.edit_playlist(PlaylistEdit::AddTracks {
                        playlist_id: add_id.clone(),
                        track_ids: vec![track_id],
                    });
                }
            }
        });
        actions.append(&add_current);

        let delete = detail_delete_button("Delete");
        let source = self.selected_source_operations();
        let delete_shell = Rc::clone(self);
        let delete_header = Rc::clone(&header);
        let delete_id = playlist_id.clone();
        delete.connect_clicked(move |_| {
            let dialog = adw::AlertDialog::builder()
                .heading(tr("Delete Playlist"))
                .body(format!(
                    "Delete \"{}\"?",
                    delete_header.borrow().playlist.name
                ))
                .build();
            dialog.add_response("cancel", &tr("Cancel"));
            dialog.add_response("delete", &tr("Delete"));
            dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let source = source.clone();
            let shell = Rc::clone(&delete_shell);
            let playlist_id = delete_id.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "delete"
                    && let Some(source) = source.as_ref()
                {
                    source.edit_playlist(PlaylistEdit::Delete {
                        playlist_id: playlist_id.clone(),
                    });
                    shell.navigate(Route::Playlists);
                }
            });
            delete_shell.present_selected_dialog(&dialog);
        });
        actions.append(&delete);

        let menu_shell = Rc::clone(self);
        let menu_header = Rc::clone(&header);
        let menu_play = Rc::clone(&play);
        let context_menu: crate::interactions::ContextMenuOpen =
            Rc::new(move |target, position| {
                let playlist = menu_header.borrow().clone();
                present_playlist_context_menu(
                    target,
                    &menu_shell,
                    playlist,
                    Some(Rc::clone(&menu_play)),
                    position,
                );
            });

        let showcase = playlist_detail_showcase(
            self,
            PlaylistDetailShowcase {
                seed,
                initial_width: content_width,
                cover: cover.clone(),
                cover_controls,
                context_menu: Some(context_menu),
                kind_row: kind_slot.clone().upcast(),
                title: title.clone().upcast(),
                summary: summary.widget(),
                actions: actions.upcast(),
            },
        );
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&entries.widget());

        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &self.placeholder_view("Playlist", msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");

        let music_folder_id = self
            .selected_library()
            .as_deref()
            .and_then(|selected| selected.music_folder_id.clone());
        let identity = self.mounted_route_read_identity(
            Route::PlaylistDetail(playlist_id.clone()),
            &loaded,
            music_folder_id,
        );
        let apply =
            {
                let shell = Rc::clone(self);
                let route_stack = route_stack.clone();
                let header = Rc::clone(&header);
                let title = title.clone();
                let summary = summary.clone();
                let entries = entries.clone();
                let cover = cover.clone();
                let kind_slot = kind_slot.clone();
                let applied_playlist_artwork = Rc::clone(&applied_playlist_artwork);
                let playlist_id = playlist_id.clone();
                Rc::new(
                move |request: PlaylistDetailReadRequest,
                      result: Result<Option<PreparedPlaylistDetail>, String>| {
                if !shell.mounted_route_read_is_current(&request.identity) {
                    return;
                }
                let next = match result {
                    Ok(next) => next,
                    Err(error) => {
                        tracing::warn!(%error, "failed to read the mounted Playlist detail");
                        return;
                    }
                };
                let Some(next) = next else {
                    route_stack.set_visible_child_name("missing");
                    return;
                };
                if !entries.replace_prepared(next.entries) {
                    return;
                }
                title.set_text(&next.summary.playlist.name);
                summary.set(next.summary.track_count, next.summary.duration_seconds);
                let prefer_server = shell
                    .settings
                    .current
                    .borrow()
                    .prefer_server_playlist_covers;
                cover.replace(&shell, &playlist_artwork(&next.summary, prefer_server));
                applied_playlist_artwork.set(prefer_server);
                while let Some(child) = kind_slot.first_child() {
                    kind_slot.remove(&child);
                }
                kind_slot.append(
                    &shell
                        .playlist_detail_kind_row(&next.summary.genres, Some(playlist_id.clone())),
                );
                header.replace(next.summary);
                route_stack.set_visible_child_name("content");
                },
            )
            };
        let load = {
            let loaded = Arc::clone(&loaded);
            let playlist_id = playlist_id.clone();
            Arc::new(move |request: &PlaylistDetailReadRequest| {
                load_playlist_detail(&loaded, &playlist_id).and_then(|detail| {
                    detail
                        .map(|detail| {
                            prepare_playlist_entry_projection(
                                detail.entries,
                                request.entries.clone(),
                            )
                            .map(|entries| PreparedPlaylistDetail {
                                summary: detail.summary,
                                entries,
                            })
                        })
                        .transpose()
                })
            })
        };
        let read = LatestMountedRouteRead::new_with_request(apply, load, "Playlist detail");
        {
            let read = Rc::downgrade(&read);
            let identity = identity.clone();
            entries.connect_search_request(move |entries| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with_if_running(PlaylistDetailReadRequest {
                    identity: identity.clone(),
                    entries,
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let header = Rc::clone(&header);
            let applied_playlist_artwork = Rc::clone(&applied_playlist_artwork);
            let cover = cover.clone();
            let entries = entries.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move || {
                let prefer_server = shell
                    .settings
                    .current
                    .borrow()
                    .prefer_server_playlist_covers;
                if applied_playlist_artwork.get() != prefer_server {
                    cover.replace(&shell, &playlist_artwork(&header.borrow(), prefer_server));
                    applied_playlist_artwork.set(prefer_server);
                }
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::PlaylistTracks);
                entries.apply_library_list_settings(LibraryListKey::PlaylistTracks, &settings);
                read.request_with_if_running(PlaylistDetailReadRequest {
                    identity: identity.clone(),
                    entries: entries.projection_request(),
                });
            })
        };
        let update = {
            let playlist_id = playlist_id.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            let entries = entries.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                if !update.change.playlists.contains(&playlist_id) {
                    return;
                }
                read.request_with(PlaylistDetailReadRequest {
                    identity: identity.clone(),
                    entries: entries.projection_request(),
                });
            })
        };
        MountedRoute::new(route_stack.upcast(), resume)
            .with_item_navigation(entries.item_navigation())
            .with_library_update(update)
    }
}

fn playlist_artwork(playlist: &PlaylistSummary, prefer_server: bool) -> Vec<ArtworkBinding> {
    ArtworkBinding::playlist_slots(
        &playlist.playlist,
        &playlist.representative_albums,
        prefer_server,
    )
}

pub(crate) fn load_smart_playlist_detail(
    loaded: &Arc<Library>,
    smart_playlist_id: &SmartPlaylistId,
    music_folder_id: Option<&MusicFolderId>,
) -> Result<Option<Arc<SmartPlaylistDetail>>, String> {
    loaded
        .smart_playlist_detail(smart_playlist_id, music_folder_id)
        .map_err(|error| error.to_string())
}

pub(crate) fn load_playlist_detail(
    loaded: &Arc<Library>,
    playlist_id: &PlaylistId,
) -> Result<Option<PlaylistDetail>, String> {
    loaded
        .playlist_detail(playlist_id)
        .map_err(|error| error.to_string())
}

#[derive(Clone)]
struct PlaylistDetailSummary {
    row: gtk::Box,
    track_count: gtk::Label,
    track_count_value: Rc<Cell<u32>>,
    duration: gtk::Label,
}

impl PlaylistDetailSummary {
    fn new(track_count: u32, duration_seconds: u32) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_halign(gtk::Align::Start);
        let (track_count_item, track_count_label) = playlist_detail_summary_item(
            "rufin-tracks-symbolic",
            &track_count_text(track_count.into()),
        );
        let track_count_value = Rc::new(Cell::new(track_count));
        let track_count_for_locale = Rc::clone(&track_count_value);
        bind_label_text_with(&track_count_label, move || {
            track_count_text(u64::from(track_count_for_locale.get()))
        });
        let (duration_item, duration_label) = playlist_detail_summary_item(
            "rufin-preferences-system-time-symbolic",
            &format_duration_units(duration_seconds),
        );
        row.append(&track_count_item);
        row.append(&duration_item);
        Self {
            row,
            track_count: track_count_label,
            track_count_value,
            duration: duration_label,
        }
    }

    fn widget(&self) -> gtk::Widget {
        self.row.clone().upcast()
    }

    fn set(&self, track_count: u32, duration_seconds: u32) {
        self.track_count_value.set(track_count);
        self.track_count
            .set_text(&track_count_text(track_count.into()));
        self.duration
            .set_text(&format_duration_units(duration_seconds));
    }
}

fn playlist_detail_summary_item(icon_name: &str, text: &str) -> (gtk::Box, gtk::Label) {
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("muted");
    icon.set_pixel_size(14);
    item.append(&icon);
    let label = gtk::Label::new(Some(text));
    label.add_css_class("muted");
    label.set_xalign(0.0);
    item.append(&label);
    (item, label)
}

#[cfg(test)]
mod tests {
    use crate::format_duration_units;
    use crate::routes::route_layout::{detail_showcase_cover_only, detail_showcase_cover_size};

    use super::playlist_cover_size;

    #[test]
    fn playlist_detail_duration_uses_units() {
        assert_eq!(format_duration_units(57), "57s");
        assert_eq!(format_duration_units(4_497), "1h 14m 57s");
    }

    #[test]
    fn mounted_detail_covers_track_width_without_resize_jumps() {
        let media_render_size = detail_showcase_cover_size(i32::MAX);
        let collection_render_size = playlist_cover_size(i32::MAX);
        let mut previous_media = detail_showcase_cover_size(96);
        let mut previous_collection = playlist_cover_size(96);
        for width in 97..=900 {
            let media = detail_showcase_cover_size(width);
            let collection = playlist_cover_size(width);
            assert!(media >= previous_media && media - previous_media <= 1);
            assert!(collection >= previous_collection && collection - previous_collection <= 1);
            assert!(media <= media_render_size);
            assert!(collection <= collection_render_size);
            if detail_showcase_cover_only(width) {
                assert!(media <= width);
                assert!(collection <= width);
            }
            previous_media = media;
            previous_collection = collection;
        }
    }
}
