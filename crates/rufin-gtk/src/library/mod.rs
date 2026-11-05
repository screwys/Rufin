mod catalog;
mod home_layout;
mod playlist_entry_model;
mod track_model;
mod track_selection;
pub(crate) use catalog::CatalogUi;

pub mod route_layout;

use rufin_core::settings::*;
mod album_detail;
mod album_detail_view;
mod artist;
mod artist_releases;
pub(crate) mod cards;
mod collection_context;
mod collections;
pub(crate) mod columns;
mod detail_showcase;
pub mod folders;
pub(crate) mod grid_cells;
mod grouped_detail;
mod home;
mod named_collections;
mod named_detail;
mod playlist_detail;
mod playlist_entries;
mod route_shell;
mod routes;
mod search;
pub use search::session::{SEARCH_PREVIEW_LIMIT, SearchSession};
mod table_links;
pub(crate) fn collection_download_change(
    apply: impl Fn(&CollectionTarget, bool) + 'static,
) -> crate::widgets::mounted_route::MountedDownloadChange {
    Rc::new(move |event| {
        if let Some((target, downloaded)) = CollectionTarget::downloaded(event) {
            apply(&target, downloaded);
        }
    })
}

use rufin_core::collection_context::CollectionTarget;
use std::rc::Rc;
