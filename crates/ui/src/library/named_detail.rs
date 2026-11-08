use std::{cell::RefCell, rc::Rc};

use artwork::ArtworkBinding;
use library::{GenreKey, MoodKey, TagRow};
use localization::{msgid, track_count_text};

use crate::library::CatalogUi;
use crate::library::LibraryListKey;
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use rufin_core::settings::presentation::format_duration_units;
use rufin_core::view::Opened;

use super::grouped_detail::GroupedDetailData;
use crate::menus::media_menus::CollectionPlay;
use crate::menus::media_menus::present_row_menu;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamedDetailId {
    Genre(GenreKey),
    Mood(MoodKey),
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

    fn missing_body(self) -> &'static str {
        match self {
            Self::Genre(_) => msgid("This isn't available"),
            Self::Mood(_) => "Files need Mood/BPM tags written on them. Not supported for Jellyfin",
        }
    }
}

fn named_artwork(row: &TagRow) -> Vec<ArtworkBinding> {
    row.artwork_binding
        .as_ref()
        .filter(|_| row.kind == library::TagKind::Genre)
        .map(std::slice::from_ref)
        .unwrap_or(&row.representative_artwork)
        .iter()
        .map(|binding| ArtworkBinding::opaque(binding))
        .collect()
}

impl CatalogUi {
    pub fn named_detail_view(self: &Rc<Self>, id: NamedDetailId, mut read: Opened) -> MountedRoute {
        let (false, Some(library::Header::Tag(summary))) =
            (read.view.is_missing(), read.header.take())
        else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                id.kind(),
                id.missing_body(),
            ));
        };
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        let title = summary.name.clone();
        let artwork = named_artwork(&summary);
        let summary_items = named_summary_items(&summary);
        let current = Rc::new(RefCell::new(summary));
        let context_menu = named_context_menu(self, Rc::clone(&current));
        let row = library::Row::Tag(current.borrow().clone());
        let grouped = self.grouped_detail_view(GroupedDetailData {
            key: id.key(),
            row,
            title,
            artwork,
            summary_items,
            context_menu,
            rows: rows.clone(),
            table_context: id.table_context(),
            play_label: id.play_label(),
        });
        let tracks = grouped.tracks().clone();
        // A reread, or a header read after a download, shows the header it read.
        let header = Rc::clone(&current);
        let shell = Rc::downgrade(self);
        let shown_view = grouped.clone();
        let shown: crate::widgets::mounted_route::MountedHeader = Rc::new(move |read| {
            if let (Some(shell), Some(library::Header::Tag(summary))) = (shell.upgrade(), read) {
                shown_view.show(
                    &shell,
                    library::Row::Tag(summary.clone()),
                    &summary.name,
                    named_artwork(&summary),
                    &named_summary_items(&summary),
                );
                header.replace(summary);
            }
        });
        let reads = tracks.page_reads(|_| {}, {
            let shown = Rc::clone(&shown);
            move |read| shown(read)
        });
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = tracks.clone();
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(id.key());
                projection.apply_library_list_settings(id.key(), &settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        MountedRoute::new(grouped.widget(), resume)
            .with_rows(&rows)
            .with_header(
                move || Some(library::Header::Tag(current.borrow().clone())),
                shown,
            )
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_search(grouped.search())
            .with_layout_cycle(grouped.layout_cycle())
            .with_item_navigation(grouped.item_navigation())
            .with_initial_demand(Rc::new(move || rows.resume_initial_demand()))
    }
}

fn named_context_menu(
    shell: &Rc<CatalogUi>,
    current: Rc<RefCell<TagRow>>,
) -> Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)> {
    let shell = Rc::clone(shell);
    Rc::new(move |target: &gtk::Widget, position, play| {
        let row = library::Row::Tag(current.borrow().clone());
        present_row_menu(target, shell.shell(), row, Some(play), position);
    })
}

fn named_summary_items(summary: &TagRow) -> Vec<(&'static str, String)> {
    let mut items = vec![(
        "rufin-tracks-symbolic",
        track_count_text(summary.summary.track_count.max(0) as u64),
    )];
    if summary.summary.duration_millis > 0 {
        items.push((
            "rufin-preferences-system-time-symbolic",
            format_duration_units(
                (summary.summary.duration_millis / 1_000)
                    .try_into()
                    .unwrap_or(u32::MAX),
            ),
        ));
    }
    items
}
