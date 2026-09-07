use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use artwork::ArtworkBinding;
use downloads::DownloadSubject;
use gtk::glib;
use library::{
    Database, PlaylistKey, PlaylistRow, ReadCancellation, SmartPlaylistKey, SmartPlaylistRow,
};
use localization::{msgid, tr, track_count_text};

use crate::format_duration_units;
use crate::player::state::current_playback_track;
use crate::shell::Shell;
use crate::shell::actions::{ADD_ICON, EDIT_ICON};
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::MountedRoute;
use crate::{LibraryListKey, LibraryListSettings};

use super::collection_context::{
    present_playlist_context_menu, present_smart_playlist_context_menu,
};
use super::collections::{CollectionPlay, library_route_inset};
use super::detail_showcase::{
    CollectionDetailShowcase, DetailShowcaseView, collection_detail_showcase, detail_action_button,
    detail_delete_button, detail_playback_controls, detail_radio_button,
};
use super::library_fields::playlist_artwork;
use super::route::Route;
use super::route_layout::{
    DETAIL_SHOWCASE_METADATA_MIN_WIDTH, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
    detail_route_inner_width,
};

const PLAYLIST_DETAIL_COMPACT_WIDTH: i32 = 760;
const PLAYLIST_DETAIL_TINY_COVER_SIZE: i32 = 150;
const PLAYLIST_DETAIL_WIDE_COVER_SIZE: i32 = 208;

pub(crate) use library::PlaylistDetailPage as PlaylistDetailData;

pub(crate) use library::SmartPlaylistDetailPage as SmartPlaylistDetailData;

#[derive(Clone)]
enum PlaylistDetailOwner {
    Saved {
        key: PlaylistKey,
        summary: PlaylistRow,
    },
    Smart {
        key: SmartPlaylistKey,
        summary: SmartPlaylistRow,
    },
}

impl PlaylistDetailOwner {
    fn key(&self) -> LibraryListKey {
        match self {
            Self::Saved { .. } => LibraryListKey::PlaylistTracks,
            Self::Smart { .. } => LibraryListKey::SmartPlaylistTracks,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Saved { summary, .. } => &summary.name,
            Self::Smart { summary, .. } => &summary.name,
        }
    }

    fn seed(&self) -> u32 {
        match self {
            Self::Saved { summary, .. } => stable_seed(&summary.object_id),
            Self::Smart { summary, .. } => stable_seed(&summary.object_id),
        }
    }

    fn track_count(&self) -> i64 {
        match self {
            Self::Saved { summary, .. } => summary.track_count,
            Self::Smart { summary, .. } => summary.track_count,
        }
    }

    fn duration_millis(&self) -> i64 {
        match self {
            Self::Saved { summary, .. } => summary.duration_millis,
            Self::Smart { summary, .. } => summary.duration_millis,
        }
    }

    fn artwork(&self, prefer_server: bool) -> Vec<ArtworkBinding> {
        match self {
            Self::Saved { summary, .. } => playlist_artwork(summary, prefer_server),
            Self::Smart { summary, .. } => summary
                .artwork_bindings
                .iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
        }
    }

    fn saved_key(&self) -> Option<PlaylistKey> {
        match self {
            Self::Saved { key, .. } => Some(*key),
            Self::Smart { .. } => None,
        }
    }

    fn context_id(&self) -> String {
        match self {
            Self::Saved { key, .. } => format!("playlist:{key}"),
            Self::Smart { key, .. } => format!("smart-playlist:{key}"),
        }
    }

    fn install_context(
        &self,
        shell: &Rc<Shell>,
        target: &gtk::Widget,
        play: CollectionPlay,
        position: Option<(f64, f64)>,
    ) {
        match self {
            Self::Saved { summary, .. } => {
                present_playlist_context_menu(target, shell, summary.clone(), Some(play), position)
            }
            Self::Smart { summary, .. } => present_smart_playlist_context_menu(
                target,
                shell,
                summary.clone(),
                Some(play),
                position,
            ),
        }
    }
}

impl Shell {
    pub(crate) fn smart_playlist_detail_route(
        self: &Rc<Self>,
        key: SmartPlaylistKey,
        detail: Option<SmartPlaylistDetailData>,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Smart Playlist"), msgid("This isn't available")),
            );
        };
        crate::shell::navigation::update_sidebar_smart_playlist_pin_metadata(self, &detail.summary);
        let owner = PlaylistDetailOwner::Smart {
            key,
            summary: detail.summary,
        };
        let list_key = owner.key();
        let settings = self.settings.current.borrow().library_list(list_key);
        let rows_database = self.products.library.clone();
        let load = Arc::new(move |uris: Vec<String>, cancellation: ReadCancellation| {
            let database = rows_database.clone();
            Box::pin(async move {
                database
                    .smart_playlist_track_rows(&uris, &cancellation)
                    .await
                    .map_err(|error| error.to_string())
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        });
        let model = super::track_model::TrackCollectionModel::with_load(
            self.products.runtime.clone(),
            detail.tracks,
            detail.first_row_position,
            detail.first_rows,
            settings,
            load,
        );
        model.set_queue_source(
            library::QueueQuery::Smart {
                key,
                source,
                now: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64),
            },
            folder,
        );
        let play_model = model.clone();
        let play_queue = self.products.playback.queue.clone();
        let play_context = owner.context_id();
        let play: CollectionPlay = Rc::new(move |placement| {
            play_model.play_source(play_queue.clone(), placement, play_context.clone());
        });
        let (tracks_widget, tracks, toolbar) = self.scrolling_track_projection(
            model,
            list_key,
            "smart-playlist-detail",
            owner.context_id(),
        );
        let item_navigation = tracks.item_navigation();
        let apply_tracks = tracks.clone();
        let apply_toolbar = toolbar.clone();
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            apply_tracks.apply_library_list_settings(list_key, settings);
            apply_toolbar.apply(list_key, settings);
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = tracks.search();
        let layout_cycle = toolbar.layout_cycle();
        let initial_demand = {
            let tracks = tracks.clone();
            Rc::new(move || tracks.resume_initial_demand()) as Rc<dyn Fn()>
        };
        self.shared_playlist_detail_route(
            owner,
            source,
            folder,
            tracks_widget,
            item_navigation,
            apply,
            Rc::new(|| {}),
            search,
            layout_cycle,
            initial_demand,
            play,
        )
        .with_download_change(tracks.download_change())
    }

    pub(crate) fn playlist_detail_route(
        self: &Rc<Self>,
        key: PlaylistKey,
        detail: Option<PlaylistDetailData>,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Playlist"), msgid("This isn't available")),
            );
        };
        let owner = PlaylistDetailOwner::Saved {
            key,
            summary: detail.summary,
        };
        let folder = match &owner {
            PlaylistDetailOwner::Saved { summary, .. } if summary.source_key.is_none() => None,
            _ => folder,
        };
        let entries = Rc::new(self.playlist_entries_view(
            key,
            owner.name().to_string(),
            detail.order,
            detail.first_row_position,
            detail.first_rows,
        ));
        entries.set_queue_folder(folder);
        let item_navigation = entries.item_navigation();
        let tracks_widget = entries.widget();
        let database = Arc::clone(&self.products.library);
        let runtime = self.products.runtime.clone();
        let request_order: Rc<
            dyn Fn(
                std::rc::Weak<super::playlist_entries::PlaylistEntriesView>,
                u64,
                super::playlist_entry_model::PlaylistEntryProjectionRequest,
            ),
        > = Rc::new(move |entries, generation, request| {
            let database = Arc::clone(&database);
            let cancellation = ReadCancellation::new();
            let task = runtime.spawn(async move {
                database
                    .playlist_entry_order(
                        key,
                        folder,
                        request.settings.sort_key.playlist_entry_sort(),
                        request.settings.descending,
                        &request.query,
                        &cancellation,
                    )
                    .await
            });
            glib::spawn_future_local(async move {
                if let Ok(Ok(order)) = task.await
                    && let Some(entries) = entries.upgrade()
                {
                    entries.replace_order(generation, order);
                }
            });
        });
        let search_request = Rc::clone(&request_order);
        let search_entries = Rc::downgrade(&entries);
        entries.connect_search_request(move |generation, request| {
            search_request(search_entries.clone(), generation, request);
        });
        let apply_entries = Rc::clone(&entries);
        let refresh_entries = Rc::clone(&entries);
        let refresh = Rc::new(move || {
            let (generation, request) = refresh_entries.begin_order_request();
            request_order(Rc::downgrade(&refresh_entries), generation, request);
        }) as Rc<dyn Fn()>;
        let apply_refresh = Rc::clone(&refresh);
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            if apply_entries.apply_library_list_settings(LibraryListKey::PlaylistTracks, settings) {
                apply_refresh();
            }
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = entries.search();
        let layout_cycle = entries.layout_cycle();
        let initial_demand = {
            let entries = Rc::clone(&entries);
            Rc::new(move || entries.resume_initial_demand()) as Rc<dyn Fn()>
        };
        let play_queue = self.products.playback.queue.clone();
        let download_entries = Rc::clone(&entries);
        let play: CollectionPlay = Rc::new(move |placement| {
            entries.play(play_queue.clone(), placement);
        });
        self.shared_playlist_detail_route(
            owner,
            source,
            folder,
            tracks_widget,
            item_navigation,
            apply,
            refresh,
            search,
            layout_cycle,
            initial_demand,
            play,
        )
        .with_download_change(Rc::new(move |event| {
            let downloads::DownloadEvent::Changed {
                media_uri,
                downloaded,
            } = event
            else {
                return;
            };
            let (media_uri, downloaded) = (media_uri.as_str(), *downloaded);
            download_entries.update_downloaded(media_uri, downloaded);
        }))
    }

    fn shared_playlist_detail_route(
        self: &Rc<Self>,
        owner: PlaylistDetailOwner,
        source: Option<library::SourceKey>,
        folder: Option<library::FolderKey>,
        tracks_widget: gtk::Widget,
        item_navigation: crate::shell::route::MountedRouteItemNavigation,
        apply_list_settings: Rc<dyn Fn(&LibraryListSettings)>,
        refresh_tracks: Rc<dyn Fn()>,
        search: gtk::SearchEntry,
        layout_cycle: crate::shell::route::MountedRouteCommand,
        initial_demand: Rc<dyn Fn()>,
        play: CollectionPlay,
    ) -> MountedRoute {
        let key = owner.key();
        let owner_state = Rc::new(std::cell::RefCell::new(owner.clone()));
        let wrapper = gtk::Box::new(
            gtk::Orientation::Vertical,
            if owner.saved_key().is_some() { 20 } else { 18 },
        );
        wrapper.add_css_class("route-content");
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let artwork = owner.artwork(self.settings.current.borrow().prefer_server_playlist_covers);
        let cover = self.cover_group_projection_for_artwork(
            &artwork,
            playlist_cover_size(width),
            playlist_cover_size(i32::MAX),
        );
        cover.widget().add_css_class("playlist-detail-cover");
        let showcase_view = DetailShowcaseView::new(
            "playlist-detail-showcase",
            owner.seed(),
            "Playlist",
            true,
            owner.name(),
        );
        self.append_playlist_detail_kind_controls(&showcase_view, &owner);
        showcase_view.replace_summary(&[
            (
                "rufin-tracks-symbolic",
                track_count_text(owner.track_count().max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((owner.duration_millis().max(0) / 1_000) as u32),
            ),
        ]);
        let actions = showcase_view.actions();
        actions.set_halign(gtk::Align::Start);
        let controls = detail_playback_controls(
            &actions,
            msgid("Play playlist"),
            None,
            false,
            Rc::clone(&play),
        );
        match &owner {
            PlaylistDetailOwner::Saved { key, summary } => {
                let rename = detail_action_button(EDIT_ICON, "Rename");
                let rename_shell = Rc::clone(self);
                let playlist = *key;
                let rename_owner = Rc::clone(&owner_state);
                rename.connect_clicked(move |_| {
                    let name = rename_owner.borrow().name().to_string();
                    rename_shell.rename_playlist_dialog(playlist, name);
                });
                actions.append(&rename);

                let add_current = detail_action_button(ADD_ICON, "Add current");
                add_current.set_sensitive(
                    current_playback_track(self.selected_playback().as_deref()).is_some(),
                );
                let add_shell = Rc::clone(self);
                let playlist = *key;
                let destination = summary.name.clone();
                add_current.connect_clicked(move |_| {
                    let Some(media) =
                        current_playback_track(add_shell.selected_playback().as_deref())
                    else {
                        return;
                    };
                    let subject = DownloadSubject::Prepared {
                        context_id: "playlist-add-current".to_string(),
                        title: Some(media.title.clone()),
                    };
                    let feedback_shell = Rc::downgrade(&add_shell);
                    let destination = destination.clone();
                    let preview_uris = vec![media.media_uri.clone()];
                    add_shell.add_media_to_playlist(
                        playlist,
                        vec![media.media_uri],
                        false,
                        Rc::new(move |accepted| {
                            if accepted > 0
                                && let Some(shell) = feedback_shell.upgrade()
                            {
                                shell.show_operation_feedback(
                                    &crate::downloads::OperationFeedback {
                                        subject: subject.clone(),
                                        preview_uris: preview_uris.clone(),
                                        item_count: accepted,
                                        kind:
                                            crate::downloads::OperationFeedbackKind::PlaylistAdded {
                                                destination: destination.clone(),
                                            },
                                    },
                                );
                            }
                        }),
                    );
                });
                actions.append(&add_current);

                let delete = detail_delete_button("Delete");
                let delete_shell = Rc::clone(self);
                let playlist = *key;
                let delete_owner = Rc::clone(&owner_state);
                delete.connect_clicked(move |_| {
                    let name = delete_owner.borrow().name().to_string();
                    let dialog = adw::AlertDialog::builder()
                        .heading(tr("Delete Playlist"))
                        .body(format!("Delete \"{name}\"?"))
                        .build();
                    dialog.add_response("cancel", &localization::tr("Cancel"));
                    dialog.add_response("delete", &localization::tr("Delete"));
                    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                    let shell = Rc::clone(&delete_shell);
                    dialog.connect_response(None, move |_, response| {
                        if response != "delete" {
                            return;
                        }
                        shell.products.source.delete_playlist(playlist);
                        shell.navigate(super::route::Route::Playlists);
                    });
                    delete_shell.present_selected_dialog(&dialog);
                });
                actions.append(&delete);
            }
            PlaylistDetailOwner::Smart { summary, .. } => {
                let edit = detail_action_button(EDIT_ICON, "Edit");
                let edit_shell = Rc::clone(self);
                let edit_owner = Rc::clone(&owner_state);
                edit.connect_clicked(move |_| {
                    if let PlaylistDetailOwner::Smart { summary, .. } = &*edit_owner.borrow() {
                        edit_shell.edit_smart_playlist_dialog(summary.clone());
                    }
                });
                actions.append(&edit);

                let delete = detail_delete_button("Delete");
                let delete_shell = Rc::clone(self);
                let delete_summary = summary.clone();
                delete.connect_clicked(move |_| {
                    delete_shell.publish_smart_playlist_change(
                        crate::preferences::dialogs::SmartPlaylistChange::Delete(
                            delete_summary.smart_playlist_key,
                        ),
                        None,
                    );
                    delete_shell.navigate(super::route::Route::SmartPlaylists);
                });
                actions.append(&delete);
            }
        }

        let menu_shell = Rc::clone(self);
        let menu_owner = Rc::clone(&owner_state);
        let menu_play = Rc::clone(&play);
        let context_menu = Rc::new(move |target: &gtk::Widget, position| {
            menu_owner.borrow().install_context(
                &menu_shell,
                target,
                Rc::clone(&menu_play),
                position,
            );
        });
        let showcase = collection_detail_showcase(
            self,
            CollectionDetailShowcase {
                view: showcase_view.clone(),
                initial_width: width,
                compact_spacing: 20,
                wide_spacing: 28,
                cover: cover.clone(),
                cover_controls: controls,
                context_menu: Some(context_menu),
            },
        );
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&tracks_widget);

        let resume = {
            let shell = Rc::downgrade(self);
            let apply_list_settings = Rc::clone(&apply_list_settings);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                apply_list_settings(&settings);
            })
        };
        let refresh = {
            let shell = Rc::downgrade(self);
            let owner = Rc::clone(&owner_state);
            let showcase = showcase_view.clone();
            let cover = cover.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                refresh_tracks();
                let database = Arc::clone(&shell.products.library);
                let owner_key = owner.borrow().clone();
                let task = shell.products.runtime.spawn(async move {
                    let cancellation = ReadCancellation::new();
                    match owner_key {
                        PlaylistDetailOwner::Saved { key, .. } => database
                            .playlist_rows(&[key], &cancellation)
                            .await
                            .map_err(|error| error.to_string())?
                            .pop()
                            .map(|summary| PlaylistDetailOwner::Saved { key, summary }),
                        PlaylistDetailOwner::Smart { key, .. } => {
                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map_or(0, |duration| duration.as_secs() as i64);
                            database
                                .smart_playlist_rows(source, &[key], folder, now, &cancellation)
                                .await
                                .map_err(|error| error.to_string())?
                                .pop()
                                .map(|summary| PlaylistDetailOwner::Smart { key, summary })
                        }
                    }
                    .ok_or_else(|| "Playlist no longer exists".to_string())
                });
                let shell = Rc::downgrade(&shell);
                let owner = Rc::clone(&owner);
                let showcase = showcase.clone();
                let cover = cover.clone();
                glib::spawn_future_local(async move {
                    let Ok(Ok(next)) = task.await else { return };
                    let Some(shell) = shell.upgrade() else { return };
                    showcase.set_title(next.name());
                    showcase.replace_summary(&[
                        (
                            "rufin-tracks-symbolic",
                            track_count_text(next.track_count().max(0) as u64),
                        ),
                        (
                            "rufin-preferences-system-time-symbolic",
                            format_duration_units((next.duration_millis().max(0) / 1_000) as u32),
                        ),
                    ]);
                    cover.replace(
                        &shell,
                        &next.artwork(
                            shell
                                .settings
                                .current
                                .borrow()
                                .prefer_server_playlist_covers,
                        ),
                    );
                    shell.append_playlist_detail_kind_controls(&showcase, &next);
                    owner.replace(next);
                });
            }) as Rc<dyn Fn()>
        };

        let download_owner = Rc::clone(&owner_state);
        let downloads =
            self.collection_download_change(move |identity, downloaded| match &mut *download_owner
                .borrow_mut()
            {
                PlaylistDetailOwner::Saved { key, summary, .. }
                    if identity == format!("playlist:{key}") =>
                {
                    summary.downloaded_count = if downloaded { summary.track_count } else { 0 };
                }
                PlaylistDetailOwner::Smart { key, summary, .. }
                    if identity == format!("smart-playlist:{key}") =>
                {
                    summary.downloaded_count = if downloaded { summary.track_count } else { 0 };
                }
                _ => {}
            });
        MountedRoute::new(wrapper.upcast(), resume)
            .with_download_change(downloads)
            .with_search(search)
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(item_navigation)
            .with_initial_demand(initial_demand)
            .with_catalog_refresh(refresh)
    }

    fn append_playlist_detail_kind_controls(
        self: &Rc<Self>,
        showcase: &DetailShowcaseView,
        owner: &PlaylistDetailOwner,
    ) {
        showcase.clear_kind_controls();
        if let PlaylistDetailOwner::Saved { key, .. } = owner {
            let radio = detail_radio_button();
            let controller = self.products.playback.radio.clone();
            let key = *key;
            radio.connect_clicked(move |_| {
                controller.play_radio(playback::RadioPlayRequest::now(
                    library::RadioSeed::Playlist(key),
                ));
            });
            showcase.append_kind_control(&radio);
        }
        if let PlaylistDetailOwner::Saved { summary, .. } = owner {
            for genre in &summary.genres {
                let button = super::detail_showcase::detail_genre_pill_button(&genre.name);
                let shell = Rc::clone(self);
                let key = genre.genre_key;
                button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(key)));
                showcase.append_kind_control(&button);
            }
        }
    }
}

pub(crate) async fn load_smart_playlist_detail(
    database: &Database,
    source: Option<library::SourceKey>,
    folder: Option<library::FolderKey>,
    key: SmartPlaylistKey,
    window: library::RouteSeedWindow,
    cancellation: &ReadCancellation,
) -> Result<Option<SmartPlaylistDetailData>, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    database
        .smart_playlist_detail(source, key, folder, now, window, cancellation)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) fn playlist_cover_size(width: i32) -> i32 {
    if width < DETAIL_SHOWCASE_METADATA_MIN_WIDTH {
        width.clamp(96, PLAYLIST_DETAIL_TINY_COVER_SIZE)
    } else if playlist_detail_compact_for_width(width) {
        PLAYLIST_DETAIL_TINY_COVER_SIZE
            + ((width - DETAIL_SHOWCASE_METADATA_MIN_WIDTH)
                * (PLAYLIST_DETAIL_WIDE_COVER_SIZE - PLAYLIST_DETAIL_TINY_COVER_SIZE)
                / (PLAYLIST_DETAIL_COMPACT_WIDTH - DETAIL_SHOWCASE_METADATA_MIN_WIDTH))
    } else {
        PLAYLIST_DETAIL_WIDE_COVER_SIZE
    }
}

pub(crate) fn playlist_detail_compact_for_width(width: i32) -> bool {
    width < PLAYLIST_DETAIL_COMPACT_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_cover_respects_checkpoint_breakpoints() {
        assert_eq!(playlist_cover_size(359), PLAYLIST_DETAIL_TINY_COVER_SIZE);
        assert_eq!(playlist_cover_size(420), PLAYLIST_DETAIL_TINY_COVER_SIZE);
        assert_eq!(playlist_cover_size(760), PLAYLIST_DETAIL_WIDE_COVER_SIZE);
    }

    #[test]
    fn mounted_detail_covers_track_width_without_resize_jumps() {
        let media_limit = super::super::route_layout::detail_showcase_cover_size(i32::MAX);
        let collection_limit = playlist_cover_size(i32::MAX);
        let mut previous_media = super::super::route_layout::detail_showcase_cover_size(96);
        let mut previous_collection = playlist_cover_size(96);
        for width in 97..=900 {
            let media = super::super::route_layout::detail_showcase_cover_size(width);
            let collection = playlist_cover_size(width);
            assert!(media >= previous_media && media - previous_media <= 1);
            assert!(collection >= previous_collection && collection - previous_collection <= 1);
            assert!(media <= media_limit);
            assert!(collection <= collection_limit);
            if super::super::route_layout::detail_showcase_cover_only(width) {
                assert!(media <= width);
                assert!(collection <= width);
            }
            previous_media = media;
            previous_collection = collection;
        }
    }

    #[test]
    fn provider_playlist_cover_is_not_nested_inside_representative_mosaic() {
        let owner = PlaylistDetailOwner::Saved {
            key: PlaylistKey::from_raw(1),
            summary: PlaylistRow {
                playlist_key: PlaylistKey::from_raw(1),
                source_key: Some(library::SourceKey::from_raw(1)),
                object_id: "playlist".to_string(),
                name: "Playlist".to_string(),
                artwork_binding: Some(vec![1]),
                track_count: 4,
                duration_millis: 1,
                downloaded_count: 0,
                representative_artwork: vec![vec![2], vec![3], vec![4], vec![5]],
                genres: Vec::new(),
            },
        };

        assert_eq!(owner.artwork(true).len(), 1);
        assert_eq!(owner.artwork(false).len(), 4);
    }
}
