use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{
    GenreDetail, GenreId, Library, MoodDetail, MoodId, MusicFolderId, RadioSeed, Track,
};
use adw::prelude::*;
use localization::{msgid, track_count_text};
use playback::RadioPlayRequest;

use crate::format_duration_units;
use crate::localization::localized_label;
use crate::shell::Shell;
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::{LatestMountedRouteRead, MountedRoute, SelectedRouteIdentity};
use crate::{LibraryListKey, LibraryListSettings};

use super::collection_context::present_genre_context_menu;
use super::collections::CollectionPlay;
use super::detail_showcase::detail_radio_button;
use super::grouped_detail::GroupedDetailData;
use super::named_collections::NamedCollectionItem;
use super::route::Route;
use super::track_model::{
    PreparedTrackProjection, TrackProjectionRequest, prepare_track_projection,
};

#[derive(Clone)]
pub(crate) enum NamedDetailId {
    Genre(GenreId),
    Mood(MoodId),
}

pub(crate) struct NamedDetail {
    summary: NamedCollectionItem,
    tracks: ::library::TrackList,
}

#[derive(Clone)]
struct NamedDetailReadRequest {
    identity: SelectedRouteIdentity,
    tracks: TrackProjectionRequest,
}

struct PreparedNamedDetail {
    summary: NamedCollectionItem,
    tracks: PreparedTrackProjection,
}

impl NamedDetailId {
    pub(crate) const fn key(&self) -> LibraryListKey {
        match self {
            Self::Genre(_) => LibraryListKey::GenreTracks,
            Self::Mood(_) => LibraryListKey::MoodTracks,
        }
    }

    fn route(&self) -> Route {
        match self {
            Self::Genre(id) => Route::GenreDetail(id.clone()),
            Self::Mood(id) => Route::MoodDetail(id.clone()),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Genre(_) => "Genre",
            Self::Mood(_) => "Mood",
        }
    }

    fn table_context(&self) -> &'static str {
        match self {
            Self::Genre(_) => "genre-detail",
            Self::Mood(_) => "mood-detail",
        }
    }

    fn play_label(&self) -> &'static str {
        match self {
            Self::Genre(_) => msgid("Play genre"),
            Self::Mood(_) => msgid("Play mood"),
        }
    }

    fn context_id(&self) -> String {
        match self {
            Self::Genre(id) => format!("genre:{}", id.as_str()),
            Self::Mood(id) => format!("mood:{}", id.as_str()),
        }
    }

    fn changed(&self, update: &crate::runtime::SelectedLibraryUpdate) -> bool {
        match self {
            Self::Genre(id) => update.change.genres.contains(id),
            Self::Mood(id) => update.change.moods.contains(id),
        }
    }

    fn contains(&self, track: &Track) -> bool {
        match self {
            Self::Genre(id) => track.relations.genres.iter().any(|value| &value.id == id),
            Self::Mood(id) => track.relations.moods.iter().any(|value| &value.id == id),
        }
    }

    fn missing_body(&self) -> &'static str {
        match self {
            Self::Genre(_) => msgid("This isn't available"),
            Self::Mood(_) => "Files need Mood/BPM tags written on them. Not supported for Jellyfin",
        }
    }
}

impl Shell {
    pub(crate) fn named_detail_view(
        self: &Rc<Self>,
        id: NamedDetailId,
        detail: Option<NamedDetail>,
        loaded: Arc<Library>,
        music_folder_id: Option<MusicFolderId>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(id.kind(), id.missing_body()),
            );
        };
        let current = Rc::new(RefCell::new(detail.summary));
        let summary = current.borrow();
        let artwork = summary.artwork();
        let title = summary.name().to_string();
        let seed = stable_seed(named_id(&summary));
        let summary_items = named_summary_items(&summary);
        let track_count = Rc::new(Cell::new(summary.track_count()));
        drop(summary);
        let context_menu = named_context_menu(self, Rc::clone(&current));
        let grouped = self.grouped_detail_view(GroupedDetailData {
            key: id.key(),
            kind_row: Some(named_kind_row(self, Rc::clone(&current)).upcast()),
            title,
            artwork,
            seed,
            summary_items,
            context_menu,
            tracks: detail.tracks,
            table_context: id.table_context(),
            playback_context: id.context_id(),
            play_label: id.play_label(),
        });
        let localized_track_count = Rc::clone(&track_count);
        grouped.bind_summary_text_with(0, move || {
            track_count_text(u64::from(localized_track_count.get()))
        });
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(&grouped.widget(), Some("content"));
        stack.add_named(
            &self.placeholder_view(id.kind(), msgid("This isn't available")),
            Some("missing"),
        );
        stack.set_visible_child_name("content");
        let identity =
            self.mounted_route_read_identity(id.route(), &loaded, music_folder_id.clone());
        let apply = {
            let shell = Rc::clone(self);
            let grouped = grouped.clone();
            let stack = stack.clone();
            let current = Rc::clone(&current);
            let track_count = Rc::clone(&track_count);
            let kind = id.kind();
            Rc::new(
                move |request: NamedDetailReadRequest,
                      result: Result<Option<PreparedNamedDetail>, String>| {
                    if !shell.mounted_route_read_is_current(&request.identity) {
                        return;
                    }
                    let next = match result {
                        Ok(next) => next,
                        Err(error) => {
                            tracing::warn!(%error, kind, "failed to refresh the mounted metadata route");
                            return;
                        }
                    };
                    let Some(next) = next else {
                        stack.set_visible_child_name("missing");
                        return;
                    };
                    let artwork = next.summary.artwork();
                    if !grouped.replace_prepared(
                        &shell,
                        next.summary.name(),
                        &artwork,
                        &named_summary_items(&next.summary),
                        next.tracks,
                    ) {
                        return;
                    }
                    track_count.set(next.summary.track_count());
                    current.replace(next.summary);
                    stack.set_visible_child_name("content");
                },
            )
        };
        let load = {
            let loaded = Arc::clone(&loaded);
            let id = id.clone();
            let music_folder_id = music_folder_id.clone();
            Arc::new(move |request: &NamedDetailReadRequest| {
                load_named_detail(
                    &loaded,
                    &id,
                    music_folder_id.as_ref(),
                    &request.tracks.settings,
                )
                .and_then(|detail| {
                    detail
                        .map(|NamedDetail { summary, tracks }| {
                            prepare_track_projection(tracks, request.tracks.clone())
                                .map(|tracks| PreparedNamedDetail { summary, tracks })
                                .map_err(|error| error.to_string())
                        })
                        .transpose()
                })
            })
        };
        let read = LatestMountedRouteRead::new_with_request(apply, load, "mounted metadata route");
        {
            let read = Rc::downgrade(&read);
            let identity = identity.clone();
            grouped.tracks().connect_search_request(move |tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with_if_running(NamedDetailReadRequest {
                    identity: identity.clone(),
                    tracks,
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let grouped = grouped.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            let key = id.key();
            Rc::new(move || {
                let settings = shell.settings.current.borrow().library_list(key);
                grouped.apply_library_list_settings(key, &settings);
                read.request_with_if_running(NamedDetailReadRequest {
                    identity: identity.clone(),
                    tracks: grouped.tracks().projection_request(),
                });
            })
        };
        let update = {
            let grouped = grouped.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            let id = id.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                let replacements = update.change.tracks.as_slice();
                if !id.changed(update) {
                    if replacements.is_empty() {
                        return;
                    }
                    if grouped
                        .tracks()
                        .apply_track_replacement(replacements, |track| {
                            id.contains(track)
                                && music_folder_id.as_ref().is_none_or(|folder_id| {
                                    track.relations.music_folders.contains(folder_id)
                                })
                        })
                    {
                        return;
                    }
                }
                read.request_with(NamedDetailReadRequest {
                    identity: identity.clone(),
                    tracks: grouped.tracks().projection_request(),
                });
            })
        };
        MountedRoute::new(stack.upcast(), resume)
            .with_item_navigation(grouped.item_navigation())
            .with_library_update(update)
    }
}

pub(crate) fn load_named_detail(
    loaded: &Arc<Library>,
    id: &NamedDetailId,
    music_folder_id: Option<&MusicFolderId>,
    settings: &LibraryListSettings,
) -> Result<Option<NamedDetail>, String> {
    let detail = match id {
        NamedDetailId::Genre(id) => loaded.genre_detail(id, music_folder_id).map(|detail| {
            detail.map(|GenreDetail { summary, tracks }| NamedDetail {
                summary: NamedCollectionItem::Genre(summary),
                tracks,
            })
        }),
        NamedDetailId::Mood(id) => loaded.mood_detail(id, music_folder_id).map(|detail| {
            detail.map(|MoodDetail { summary, tracks }| NamedDetail {
                summary: NamedCollectionItem::Mood(summary),
                tracks,
            })
        }),
    }
    .map_err(|error| error.to_string())?;
    detail
        .map(|mut detail| {
            detail.tracks = detail
                .tracks
                .sorted(settings.sort_key.track_sort(), settings.descending)
                .map_err(|error| error.to_string())?;
            Ok(detail)
        })
        .transpose()
}

fn named_context_menu(
    shell: &Rc<Shell>,
    current: Rc<RefCell<NamedCollectionItem>>,
) -> Option<Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>> {
    let genre = matches!(&*current.borrow(), NamedCollectionItem::Genre(_));
    genre.then(|| {
        let shell = Rc::clone(shell);
        Rc::new(move |target: &gtk::Widget, position, play| {
            let current = current.borrow();
            let NamedCollectionItem::Genre(summary) = &*current else {
                return;
            };
            present_genre_context_menu(target, &shell, summary.clone(), Some(play), position);
        }) as Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>
    })
}

fn named_kind_row(shell: &Rc<Shell>, current: Rc<RefCell<NamedCollectionItem>>) -> gtk::Box {
    let genre = matches!(&*current.borrow(), NamedCollectionItem::Genre(_));
    let kind = localized_label(if genre { "Genre" } else { "Mood" });
    kind.add_css_class("eyebrow");
    kind.set_xalign(0.0);
    kind.set_halign(gtk::Align::Start);
    kind.set_valign(gtk::Align::Center);
    kind.set_margin_end(6);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    row.add_css_class("album-detail-kind-row");
    if genre {
        row.add_css_class("album-detail-genre-row");
    }
    row.set_valign(gtk::Align::Center);
    row.set_halign(gtk::Align::Start);
    row.append(&kind);
    if genre {
        let radio = detail_radio_button();
        let controller = shell.products.playback.radio.clone();
        radio.connect_clicked(move |_| {
            let current = current.borrow();
            let NamedCollectionItem::Genre(summary) = &*current else {
                return;
            };
            controller.play_radio(RadioPlayRequest::now(RadioSeed::Genre {
                id: summary.genre.id.clone(),
                name: summary.genre.name.clone(),
            }));
        });
        row.append(&radio);
    }
    row
}

fn named_id(summary: &NamedCollectionItem) -> &str {
    match summary {
        NamedCollectionItem::Genre(summary) => summary.genre.id.as_str(),
        NamedCollectionItem::Mood(summary) => summary.mood.id.as_str(),
    }
}

fn named_summary_items(summary: &NamedCollectionItem) -> Vec<(&'static str, String)> {
    let mut items = vec![(
        "rufin-tracks-symbolic",
        track_count_text(summary.track_count().into()),
    )];
    if summary.duration_seconds() > 0 {
        items.push((
            "rufin-preferences-system-time-symbolic",
            format_duration_units(summary.duration_seconds()),
        ));
    }
    items
}
