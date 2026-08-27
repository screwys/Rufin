use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use adw::prelude::*;
use artwork::ArtworkBinding;
use library::{Database, GenreKey, GenreRow, MoodKey, MoodRow, RadioSeed, ReadCancellation};
use localization::{msgid, track_count_text};
use playback::RadioPlayRequest;

use crate::format_duration_units;
use crate::localization::localized_label;
use crate::shell::Shell;
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::MountedRoute;
use crate::{LibraryListKey, LibraryListSettings};

use super::collection_context::{present_genre_context_menu, present_mood_context_menu};
use super::collections::CollectionPlay;
use super::detail_showcase::detail_radio_button;
use super::grouped_detail::GroupedDetailData;
use super::route::Route;
use super::track_model::{PreparedTrackProjection, TrackProjectionRequest};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NamedDetailId {
    Genre(GenreKey),
    Mood(MoodKey),
}

#[derive(Clone)]
pub(crate) enum NamedDetailSummary {
    Genre(GenreRow),
    Mood(MoodRow),
}

pub(crate) struct NamedOrderLane {
    generation: Cell<u64>,
    cancellation: RefCell<Option<ReadCancellation>>,
}

impl NamedOrderLane {
    pub(crate) fn new() -> Self {
        Self {
            generation: Cell::new(0),
            cancellation: RefCell::new(None),
        }
    }

    pub(crate) fn begin(&self) -> (u64, ReadCancellation) {
        if let Some(cancellation) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let cancellation = ReadCancellation::new();
        self.cancellation.replace(Some(cancellation.clone()));
        (generation, cancellation)
    }

    pub(crate) fn finish(&self, generation: u64) -> bool {
        if self.generation.get() != generation {
            return false;
        }
        self.cancellation.borrow_mut().take();
        true
    }
}

impl Drop for NamedOrderLane {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.get_mut().take() {
            cancellation.cancel();
        }
    }
}

impl NamedDetailId {
    pub(crate) const fn key(self) -> LibraryListKey {
        match self {
            Self::Genre(_) => LibraryListKey::GenreTracks,
            Self::Mood(_) => LibraryListKey::MoodTracks,
        }
    }

    fn route(self) -> Route {
        match self {
            Self::Genre(id) => Route::GenreDetail(id),
            Self::Mood(id) => Route::MoodDetail(id),
        }
    }

    fn kind(self) -> &'static str {
        match self {
            Self::Genre(_) => "Genre",
            Self::Mood(_) => "Mood",
        }
    }

    fn table_context(self) -> &'static str {
        match self {
            Self::Genre(_) => "genre-detail",
            Self::Mood(_) => "mood-detail",
        }
    }

    fn play_label(self) -> &'static str {
        match self {
            Self::Genre(_) => msgid("Play genre"),
            Self::Mood(_) => msgid("Play mood"),
        }
    }

    fn context_id(self) -> String {
        match self {
            Self::Genre(id) => format!("genre:{id}"),
            Self::Mood(id) => format!("mood:{id}"),
        }
    }

    fn missing_body(self) -> &'static str {
        match self {
            Self::Genre(_) => msgid("This isn't available"),
            Self::Mood(_) => "Files need Mood/BPM tags written on them. Not supported for Jellyfin",
        }
    }
}

impl NamedDetailSummary {
    fn name(&self) -> &str {
        match self {
            Self::Genre(row) => &row.name,
            Self::Mood(row) => &row.name,
        }
    }

    fn track_count(&self) -> i64 {
        match self {
            Self::Genre(row) => row.track_count,
            Self::Mood(row) => row.track_count,
        }
    }

    fn duration_millis(&self) -> i64 {
        match self {
            Self::Genre(row) => row.duration_millis,
            Self::Mood(row) => row.duration_millis,
        }
    }

    fn artwork(&self) -> Vec<ArtworkBinding> {
        match self {
            Self::Genre(row) => row
                .artwork_binding
                .as_ref()
                .or_else(|| row.representative_artwork.first())
                .into_iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
            Self::Mood(row) => row
                .representative_artwork
                .iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
        }
    }

    fn seed(&self) -> u32 {
        match self {
            Self::Genre(row) => stable_seed(&row.object_id),
            Self::Mood(row) => stable_seed(&row.mood_key.to_string()),
        }
    }
}

impl Shell {
    pub(crate) fn named_detail_view(
        self: &Rc<Self>,
        id: NamedDetailId,
        summary: Option<NamedDetailSummary>,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(summary) = summary else {
            return MountedRoute::static_widget(
                self.placeholder_view(id.kind(), id.missing_body()),
            );
        };
        let current = Rc::new(RefCell::new(summary));
        let borrowed = current.borrow();
        let title = borrowed.name().to_string();
        let artwork = borrowed.artwork();
        let seed = borrowed.seed();
        let summary_items = named_summary_items(&borrowed);
        drop(borrowed);
        let context_menu = named_context_menu(self, Rc::clone(&current));
        let grouped = self.grouped_detail_view(GroupedDetailData {
            key: id.key(),
            kind_row: Some(named_kind_row(self, id).upcast()),
            title,
            artwork,
            seed,
            summary_items,
            context_menu,
            selected: selected.clone(),
            tracks: order,
            first_rows,
            play_order: None,
            table_context: id.table_context(),
            playback_context: id.context_id(),
            play_label: id.play_label(),
        });
        let tracks = Rc::new(grouped.tracks().clone());
        let lane = Rc::new(NamedOrderLane::new());
        {
            let shell = Rc::downgrade(self);
            let projection = Rc::downgrade(&tracks);
            let selected = selected.clone();
            let lane = Rc::clone(&lane);
            grouped.tracks().connect_search_request(move |request| {
                request_named_order(
                    shell.clone(),
                    projection.clone(),
                    selected.clone(),
                    id,
                    request,
                    Rc::clone(&lane),
                );
            });
        }
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = Rc::clone(&tracks);
            let selected = selected.clone();
            let lane = Rc::clone(&lane);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(id.key());
                projection.apply_library_list_settings(id.key(), &settings);
                request_named_order(
                    Rc::downgrade(&shell),
                    Rc::downgrade(&projection),
                    selected.clone(),
                    id,
                    projection.projection_request(),
                    Rc::clone(&lane),
                );
            })
        };
        MountedRoute::new(grouped.widget(), resume).with_item_navigation(grouped.item_navigation())
    }
}

fn request_named_order(
    shell: std::rc::Weak<Shell>,
    projection: std::rc::Weak<super::routes::TrackListProjection>,
    selected: crate::runtime::SelectedLibrary,
    id: NamedDetailId,
    request: TrackProjectionRequest,
    lane: Rc<NamedOrderLane>,
) {
    let (generation, cancellation) = lane.begin();
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let query = request.query.clone();
    let sort = request.settings.sort_key.track_sort();
    let descending = request.settings.descending;
    let task = selected.runtime.spawn(async move {
        match id {
            NamedDetailId::Genre(key) => {
                database
                    .genre_track_route_page(
                        source,
                        key,
                        folder,
                        &query,
                        sort,
                        descending,
                        &cancellation,
                    )
                    .await
            }
            NamedDetailId::Mood(key) => {
                database
                    .mood_track_route_page(
                        source,
                        key,
                        folder,
                        &query,
                        sort,
                        descending,
                        &cancellation,
                    )
                    .await
            }
        }
    });
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let page = task.await.ok().and_then(Result::ok);
        let Some(projection) = projection.upgrade() else {
            return;
        };
        if !lane.finish(generation) || shell.navigation.routes.borrow().current() != &id.route() {
            return;
        }
        if let Some(page) = page {
            projection.replace_prepared(PreparedTrackProjection {
                order: page.order,
                first_rows: page.first_rows,
                request,
            });
        }
    });
}

pub(crate) async fn load_named_detail(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    id: NamedDetailId,
    settings: &LibraryListSettings,
    cancellation: &ReadCancellation,
) -> Result<(Option<NamedDetailSummary>, library::TrackRoutePage), String> {
    let sort = settings.sort_key.track_sort();
    let descending = settings.descending;
    match id {
        NamedDetailId::Genre(key) => {
            let detail = database
                .genre_detail(source, key, folder, cancellation)
                .await
                .map_err(|error| error.to_string())?;
            let detail = if let Some(detail) = detail {
                let mut row = detail.genre;
                row.representative_artwork = database
                    .album_rows(source, &detail.representative_albums, folder, cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter_map(|album| album.artwork_binding)
                    .collect();
                Some(NamedDetailSummary::Genre(row))
            } else {
                None
            };
            let page = database
                .genre_track_route_page(source, key, folder, "", sort, descending, cancellation)
                .await
                .map_err(|error| error.to_string())?;
            Ok((detail, page))
        }
        NamedDetailId::Mood(key) => {
            let detail = database
                .mood_detail(source, key, folder, cancellation)
                .await
                .map_err(|error| error.to_string())?;
            let detail = if let Some(detail) = detail {
                let mut row = detail.mood;
                row.representative_artwork = database
                    .album_rows(source, &detail.representative_albums, folder, cancellation)
                    .await
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .filter_map(|album| album.artwork_binding)
                    .collect();
                Some(NamedDetailSummary::Mood(row))
            } else {
                None
            };
            let page = database
                .mood_track_route_page(source, key, folder, "", sort, descending, cancellation)
                .await
                .map_err(|error| error.to_string())?;
            Ok((detail, page))
        }
    }
}

fn named_context_menu(
    shell: &Rc<Shell>,
    current: Rc<RefCell<NamedDetailSummary>>,
) -> Option<Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>> {
    let shell = Rc::clone(shell);
    Some(Rc::new(
        move |target: &gtk::Widget, position, play| match &*current.borrow() {
            NamedDetailSummary::Genre(summary) => {
                present_genre_context_menu(target, &shell, summary.clone(), Some(play), position)
            }
            NamedDetailSummary::Mood(summary) => {
                present_mood_context_menu(target, &shell, summary.clone(), Some(play), position)
            }
        },
    )
        as Rc<
            dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay),
        >)
}

fn named_kind_row(shell: &Rc<Shell>, id: NamedDetailId) -> gtk::Box {
    let genre = matches!(id, NamedDetailId::Genre(_));
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
    if let NamedDetailId::Genre(key) = id {
        let radio = detail_radio_button();
        let controller = shell.products.playback.radio.clone();
        radio.connect_clicked(move |_| {
            controller.play_radio(RadioPlayRequest::now(RadioSeed::Genre(key)));
        });
        row.append(&radio);
    }
    row
}

fn named_summary_items(summary: &NamedDetailSummary) -> Vec<(&'static str, String)> {
    let mut items = vec![(
        "rufin-tracks-symbolic",
        track_count_text(summary.track_count().max(0) as u64),
    )];
    if summary.duration_millis() > 0 {
        items.push((
            "rufin-preferences-system-time-symbolic",
            format_duration_units(
                (summary.duration_millis() / 1_000)
                    .try_into()
                    .unwrap_or(u32::MAX),
            ),
        ));
    }
    items
}
