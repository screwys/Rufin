use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use library::{
    Database, PlaylistKey, PlaylistRow, ReadCancellation, SmartPlaylistKey, SmartPlaylistRow,
};
use localization::{msgid, tr, track_count_text};

use crate::format_duration_units;
use crate::localization::localized_label;
use crate::player::state::current_playback_track;
use crate::shell::Shell;
use crate::shell::actions::{ADD_ICON, EDIT_ICON};
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::MountedRoute;
use crate::{LibraryListKey, LibraryListSettings};

use super::collection_context::{
    present_playlist_context_menu, present_smart_playlist_context_menu,
};
use super::collections::{CollectionPlay, PlaybackTarget, library_route_inset};
use super::detail_showcase::{
    DetailSummaryProjection, PlaylistDetailShowcase, detail_action_button, detail_action_row,
    detail_delete_button, detail_playback_controls, detail_radio_button, detail_title_label,
    playlist_detail_showcase,
};
use super::library_fields::playlist_artwork;
use super::route::Route;
use super::route_layout::{PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_route_inner_width};

const PLAYLIST_DETAIL_COMPACT_WIDTH: i32 = 760;
const PLAYLIST_DETAIL_COVER_ONLY_WIDTH: i32 = 420;
const PLAYLIST_DETAIL_TINY_COVER_SIZE: i32 = 150;
const PLAYLIST_DETAIL_WIDE_COVER_SIZE: i32 = 208;

#[derive(Clone)]
pub(crate) struct PlaylistDetailData {
    pub(crate) summary: PlaylistRow,
    pub(crate) order: library::PlaylistEntryOrder,
    pub(crate) first_rows: Vec<library::PlaylistEntryRow>,
}

#[derive(Clone)]
pub(crate) struct SmartPlaylistDetailData {
    pub(crate) summary: SmartPlaylistRow,
    pub(crate) tracks: Vec<library::TrackKey>,
    pub(crate) first_rows: Vec<library::TrackRow>,
}

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

enum PlaylistDetailMembership {
    Saved {
        order: library::PlaylistEntryOrder,
        first_rows: Vec<library::PlaylistEntryRow>,
    },
    Smart {
        tracks: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
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

    fn target(&self) -> PlaybackTarget {
        match self {
            Self::Saved { key, .. } => PlaybackTarget::Playlist(*key),
            Self::Smart { key, .. } => PlaybackTarget::SmartPlaylist(*key),
        }
    }

    fn context_id(&self) -> String {
        match self {
            Self::Saved { key, .. } => format!("playlist:{key}"),
            Self::Smart { key, .. } => format!("smart-playlist:{key}"),
        }
    }

    fn saved_key(&self) -> Option<PlaylistKey> {
        match self {
            Self::Saved { key, .. } => Some(*key),
            Self::Smart { .. } => None,
        }
    }

    fn smart_key(&self) -> Option<SmartPlaylistKey> {
        match self {
            Self::Smart { key, .. } => Some(*key),
            Self::Saved { .. } => None,
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
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Smart Playlist"), msgid("This isn't available")),
            );
        };
        self.shared_playlist_detail_route(
            PlaylistDetailOwner::Smart {
                key,
                summary: detail.summary,
            },
            PlaylistDetailMembership::Smart {
                tracks: detail.tracks,
                first_rows: detail.first_rows,
            },
            selected,
        )
    }

    pub(crate) fn playlist_detail_route(
        self: &Rc<Self>,
        key: PlaylistKey,
        detail: Option<PlaylistDetailData>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Playlist"), msgid("This isn't available")),
            );
        };
        self.shared_playlist_detail_route(
            PlaylistDetailOwner::Saved {
                key,
                summary: detail.summary,
            },
            PlaylistDetailMembership::Saved {
                order: detail.order,
                first_rows: detail.first_rows,
            },
            selected,
        )
    }

    fn shared_playlist_detail_route(
        self: &Rc<Self>,
        owner: PlaylistDetailOwner,
        membership: PlaylistDetailMembership,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let key = owner.key();
        let owner_state = Rc::new(std::cell::RefCell::new(owner.clone()));
        let context = match owner {
            PlaylistDetailOwner::Saved { .. } => "playlist-detail",
            PlaylistDetailOwner::Smart { .. } => "smart-playlist-detail",
        };
        let (tracks_widget, item_navigation, apply_membership_settings) = match membership {
            PlaylistDetailMembership::Saved { order, first_rows } => {
                let playlist = owner
                    .saved_key()
                    .expect("saved Playlist membership has one Playlist key");
                let entries =
                    Rc::new(self.playlist_entries_view(&selected, playlist, order, first_rows));
                let navigation = entries.item_navigation();
                let widget = entries.widget();
                let database = Arc::clone(&selected.database);
                let runtime = selected.runtime.clone();
                let source = selected.source_key;
                let folder = selected.music_folder_key;
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
                                source,
                                playlist,
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
                let apply_request = Rc::clone(&request_order);
                let apply = Rc::new(move |settings: &LibraryListSettings| {
                    apply_entries
                        .apply_library_list_settings(LibraryListKey::PlaylistTracks, settings);
                    let (generation, request) = apply_entries.begin_order_request();
                    apply_request(Rc::downgrade(&apply_entries), generation, request);
                }) as Rc<dyn Fn(&LibraryListSettings)>;
                (widget, navigation, apply)
            }
            PlaylistDetailMembership::Smart { tracks, first_rows } => {
                let smart = owner
                    .smart_key()
                    .expect("Smart Playlist membership has one Smart Playlist key");
                let (widget, tracks, toolbar) = self.scrolling_track_projection(
                    &selected,
                    tracks,
                    first_rows,
                    key,
                    context,
                    owner.context_id(),
                );
                let navigation = tracks.item_navigation();
                let apply_tracks = tracks.clone();
                let apply_toolbar = toolbar.clone();
                let database = Arc::clone(&selected.database);
                let runtime = selected.runtime.clone();
                let source = selected.source_key;
                let folder = selected.music_folder_key;
                let lane = Rc::new(super::named_detail::NamedOrderLane::new());
                let apply = Rc::new(move |settings: &LibraryListSettings| {
                    apply_tracks.apply_library_list_settings(key, settings);
                    apply_toolbar.apply(key, settings);
                    let request = apply_tracks.projection_request();
                    let (generation, cancellation) = lane.begin();
                    let database = Arc::clone(&database);
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_secs() as i64);
                    let task = runtime.spawn(async move {
                        database
                            .smart_playlist_track_order(source, smart, folder, now, &cancellation)
                            .await
                    });
                    let tracks = apply_tracks.clone();
                    let lane = Rc::clone(&lane);
                    glib::spawn_future_local(async move {
                        if let Ok(Ok(order)) = task.await
                            && lane.finish(generation)
                        {
                            tracks.replace_prepared(super::track_model::PreparedTrackProjection {
                                order,
                                first_rows: Vec::new(),
                                request,
                            });
                        }
                    });
                }) as Rc<dyn Fn(&LibraryListSettings)>;
                (widget, navigation, apply)
            }
        };
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
        let title = detail_title_label(owner.name());
        let kind_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        kind_slot.append(&self.playlist_detail_kind_row(&owner));
        let summary = DetailSummaryProjection::new(&[
            (
                "rufin-tracks-symbolic",
                track_count_text(owner.track_count().max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((owner.duration_millis().max(0) / 1_000) as u32),
            ),
        ]);
        let actions = detail_action_row();
        actions.set_halign(gtk::Align::Start);
        let target = owner.target();
        let play_shell = Rc::clone(self);
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            target.play(&play_shell, placement, shuffled);
        });
        let controls = detail_playback_controls(
            &actions,
            msgid("Play playlist"),
            None,
            false,
            Rc::clone(&play),
        );
        match &owner {
            PlaylistDetailOwner::Saved { key, summary: _ } => {
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
                    current_playback_track(self.selected_playback().as_deref())
                        .and_then(|track| track.track_key)
                        .is_some(),
                );
                let add_shell = Rc::clone(self);
                let playlist = *key;
                add_current.connect_clicked(move |_| {
                    let Some(track) =
                        current_playback_track(add_shell.selected_playback().as_deref())
                            .and_then(|track| track.track_key)
                    else {
                        return;
                    };
                    if let Some(operations) = add_shell.selected_source_operations() {
                        let skip_duplicates = add_shell
                            .selected_library()
                            .as_deref()
                            .is_none_or(|selected| !selected.playlist_tracks_can_repeat);
                        operations.add_playlist_tracks(playlist, vec![track], skip_duplicates);
                    }
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
                        if let Some(operations) = shell.selected_source_operations() {
                            operations.delete_playlist(playlist);
                            shell.navigate(super::route::Route::Playlists);
                        }
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
        let showcase = playlist_detail_showcase(
            self,
            PlaylistDetailShowcase {
                seed: owner.seed(),
                initial_width: width,
                cover: cover.clone(),
                cover_controls: controls,
                context_menu: Some(context_menu),
                kind_row: kind_slot.clone().upcast(),
                title: title.clone().upcast(),
                summary: summary.widget(),
                actions: actions.upcast(),
            },
        );
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&tracks_widget);

        let resume = {
            let shell = Rc::downgrade(self);
            let apply_membership_settings = Rc::clone(&apply_membership_settings);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                apply_membership_settings(&settings);
            })
        };
        let refresh = {
            let shell = Rc::downgrade(self);
            let selected = selected.clone();
            let owner = Rc::clone(&owner_state);
            let title = title.clone();
            let summary = summary.clone();
            let cover = cover.clone();
            let kind_slot = kind_slot.clone();
            let apply_membership_settings = Rc::clone(&apply_membership_settings);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let list_settings = shell.settings.current.borrow().library_list(key);
                apply_membership_settings(&list_settings);
                let database = Arc::clone(&selected.database);
                let source = selected.source_key;
                let folder = selected.music_folder_key;
                let owner_key = owner.borrow().clone();
                let task = selected.runtime.spawn(async move {
                    let cancellation = ReadCancellation::new();
                    match owner_key {
                        PlaylistDetailOwner::Saved { key, .. } => database
                            .playlist_rows(source, &[key], folder, &cancellation)
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
                    .ok_or_else(|| "Playlist is no longer current".to_string())
                });
                let shell = Rc::downgrade(&shell);
                let owner = Rc::clone(&owner);
                let title = title.clone();
                let summary = summary.clone();
                let cover = cover.clone();
                let kind_slot = kind_slot.clone();
                glib::spawn_future_local(async move {
                    let Ok(Ok(next)) = task.await else { return };
                    let Some(shell) = shell.upgrade() else { return };
                    title.set_text(next.name());
                    summary.replace(&[
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
                    while let Some(child) = kind_slot.first_child() {
                        kind_slot.remove(&child);
                    }
                    kind_slot.append(&shell.playlist_detail_kind_row(&next));
                    owner.replace(next);
                });
            }) as Rc<dyn Fn()>
        };
        MountedRoute::new(wrapper.upcast(), resume)
            .with_item_navigation(item_navigation)
            .with_catalog_refresh(refresh)
    }

    fn playlist_detail_kind_row(self: &Rc<Self>, owner: &PlaylistDetailOwner) -> gtk::Box {
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
        if let PlaylistDetailOwner::Saved { key, .. } = owner {
            let radio = detail_radio_button();
            let controller = self.products.playback.radio.clone();
            let key = *key;
            radio.connect_clicked(move |_| {
                controller.play_radio(playback::RadioPlayRequest::now(
                    library::RadioSeed::Playlist(key),
                ));
            });
            row.append(&radio);
        }
        if let PlaylistDetailOwner::Saved { summary, .. } = owner {
            for genre in &summary.genres {
                let button = super::detail_showcase::detail_genre_pill_button(&genre.name);
                let shell = Rc::clone(self);
                let key = genre.genre_key;
                button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(key)));
                row.append(&button);
            }
        }
        row
    }
}

pub(crate) async fn load_playlist_detail(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    key: PlaylistKey,
    settings: &LibraryListSettings,
    cancellation: &ReadCancellation,
) -> Result<Option<PlaylistDetailData>, String> {
    let summary = database
        .playlist_rows(source, &[key], folder, cancellation)
        .await
        .map_err(|error| error.to_string())?
        .pop();
    let Some(summary) = summary else {
        return Ok(None);
    };
    let order = database
        .playlist_entry_order(
            source,
            key,
            folder,
            settings.sort_key.playlist_entry_sort(),
            settings.descending,
            "",
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let first_rows = database
        .playlist_entry_rows(
            source,
            &order.entries[..order.entries.len().min(64)],
            folder,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(PlaylistDetailData {
        summary,
        order,
        first_rows,
    }))
}

pub(crate) async fn load_smart_playlist_detail(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    key: SmartPlaylistKey,
    cancellation: &ReadCancellation,
) -> Result<Option<SmartPlaylistDetailData>, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let summary = database
        .smart_playlist_rows(source, &[key], folder, now, cancellation)
        .await
        .map_err(|error| error.to_string())?
        .pop();
    let Some(summary) = summary else {
        return Ok(None);
    };
    let tracks = database
        .smart_playlist_track_order(source, key, folder, now, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let first_rows = database
        .track_rows(source, &tracks[..tracks.len().min(64)], cancellation)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(SmartPlaylistDetailData {
        summary,
        tracks,
        first_rows,
    }))
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
                source_key: library::SourceKey::from_raw(1),
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
