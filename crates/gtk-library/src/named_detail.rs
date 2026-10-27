use rufin_core::browse::{DetailContent, DetailPage, DetailSummary};
use rufin_core::collection_context::CollectionTarget;
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use artwork::ArtworkBinding;
use library::{GenreKey, GenreView, MoodKey, MoodView, RadioSeed};
use localization::{msgid, track_count_text};
use rufin_core::playback::RadioPlayRequest;

use crate::CatalogUi;
use crate::LibraryListKey;
use gtk_widgets::artwork::presentation::stable_seed;
use gtk_widgets::format_duration_units;
use gtk_widgets::mounted_route::MountedRoute;

use super::detail_showcase::detail_radio_button;
use super::grouped_detail::GroupedDetailData;
use gtk_media_menus::media_menus::CollectionPlay;
use gtk_media_menus::media_menus::{present_genre_context_menu, present_mood_context_menu};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamedDetailId {
    Genre(GenreKey),
    Mood(MoodKey),
}

#[derive(Clone)]
pub enum NamedDetailSummary {
    Genre(GenreView),
    Mood(MoodView),
}

impl NamedDetailId {
    pub const fn key(self) -> LibraryListKey {
        match self {
            Self::Genre(_) => LibraryListKey::GenreTracks,
            Self::Mood(_) => LibraryListKey::MoodTracks,
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

    fn target(self) -> CollectionTarget {
        match self {
            Self::Genre(id) => CollectionTarget::Genre(id),
            Self::Mood(id) => CollectionTarget::Mood(id),
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
            Self::Genre(row) => &row.metadata.name,
            Self::Mood(row) => &row.metadata.name,
        }
    }

    fn track_count(&self) -> i64 {
        match self {
            Self::Genre(row) => row.summary.track_count,
            Self::Mood(row) => row.summary.track_count,
        }
    }

    fn duration_millis(&self) -> i64 {
        match self {
            Self::Genre(row) => row.summary.duration_millis,
            Self::Mood(row) => row.summary.duration_millis,
        }
    }

    fn artwork(&self) -> Vec<ArtworkBinding> {
        match self {
            Self::Genre(row) => row
                .artwork_binding
                .as_ref()
                .map(std::slice::from_ref)
                .unwrap_or(&row.representative_artwork)
                .iter()
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

impl CatalogUi {
    pub fn named_detail_view(
        self: &Rc<Self>,
        id: NamedDetailId,
        opened: rufin_core::page::Page,
        detail: Option<DetailPage>,
    ) -> MountedRoute {
        let Some(DetailPage {
            summary,
            content: DetailContent::Tracks(page),
        }) = detail
        else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                id.kind(),
                id.missing_body(),
            ));
        };
        let summary = match summary {
            DetailSummary::Genre(row) => NamedDetailSummary::Genre(*row),
            DetailSummary::Mood(row) => NamedDetailSummary::Mood(*row),
            _ => unreachable!("named detail"),
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
            kind: if matches!(id, NamedDetailId::Genre(_)) {
                "Genre"
            } else {
                "Mood"
            },
            genre_kind: matches!(id, NamedDetailId::Genre(_)),
            kind_controls: named_kind_controls(self, id),
            title,
            artwork,
            seed,
            summary_items,
            context_menu,
            tracks: page,
            table_context: id.table_context(),
            playback_context: id.target().context_id(),
            play_label: id.play_label(),
        });
        let tracks = Rc::new(grouped.tracks().clone());
        let reread = tracks.page_reread(self, "mounted named detail", opened, |_| {}, |_| {});
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = Rc::clone(&tracks);
            let reread = Rc::clone(&reread);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(id.key());
                projection.apply_library_list_settings(id.key(), &settings);
                reread.resume(&shell.settings.current.borrow());
            })
        };
        let download_summary = Rc::clone(&current);
        let downloads = crate::collection_download_change(move |target, downloaded| {
            if *target != id.target() {
                return;
            }
            match &mut *download_summary.borrow_mut() {
                NamedDetailSummary::Genre(row) => {
                    row.summary.downloaded_count = if downloaded {
                        row.summary.track_count
                    } else {
                        0
                    };
                }
                NamedDetailSummary::Mood(row) => {
                    row.summary.downloaded_count = if downloaded {
                        row.summary.track_count
                    } else {
                        0
                    };
                }
            }
        });
        MountedRoute::new(grouped.widget(), resume)
            .with_catalog_refresh(Rc::new(move || reread.reread()))
            .with_download_change(downloads)
            .with_download_change(tracks.download_change())
            .with_search(grouped.search())
            .with_layout_cycle(grouped.layout_cycle())
            .with_item_navigation(grouped.item_navigation())
            .with_initial_demand({
                let tracks = Rc::clone(&tracks);
                Rc::new(move || tracks.resume_initial_demand())
            })
    }
}

fn named_context_menu(
    shell: &Rc<CatalogUi>,
    current: Rc<RefCell<NamedDetailSummary>>,
) -> Option<Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>> {
    let shell = Rc::clone(shell);
    Some(Rc::new(
        move |target: &gtk::Widget, position, play| match &*current.borrow() {
            NamedDetailSummary::Genre(summary) => present_genre_context_menu(
                target,
                &shell.media_menus,
                summary.clone(),
                Some(play),
                position,
            ),
            NamedDetailSummary::Mood(summary) => present_mood_context_menu(
                target,
                &shell.media_menus,
                summary.clone(),
                Some(play),
                position,
            ),
        },
    )
        as Rc<
            dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay),
        >)
}

fn named_kind_controls(shell: &Rc<CatalogUi>, id: NamedDetailId) -> Vec<gtk::Widget> {
    let mut controls = Vec::new();
    if let NamedDetailId::Genre(key) = id {
        let radio = detail_radio_button();
        let controller = shell.playback.clone();
        radio.connect_clicked(move |_| {
            controller.play_radio(RadioPlayRequest::now(RadioSeed::Genre(key)));
        });
        controls.push(radio.upcast());
    }
    controls
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
