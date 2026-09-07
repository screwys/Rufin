mod catalog;
mod home_layout;
mod playlist_entry_model;
mod release_kind;
mod track_model;
mod track_selection;
pub use catalog::CatalogUi;

pub mod route_layout;
pub mod ui_resource;
pub fn register_resources() -> Result<(), String> {
    ui_shared::register_resources()?;
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("ui-library.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}

use rufin_core::settings::*;
mod album_detail;
mod album_detail_view;
mod artist;
mod artist_releases;
mod cards;
mod collection_context;
mod collections;
mod columns;
mod detail_showcase;
pub mod folders;
mod grid_cells;
mod grouped_detail;
mod home;
mod named_collections;
mod named_detail;
mod playlist_detail;
mod playlist_entries;
mod route_shell;
mod routes;
mod search;
mod table_links;
mod table_sizing;
pub(crate) fn collection_download_change(
    apply: impl Fn(&str, bool) + 'static,
) -> ui_shared::mounted_route::MountedDownloadChange {
    Rc::new(move |event| {
        let DownloadEvent::SubjectChanged {
            subject: DownloadSubject::Prepared { context_id, .. },
            downloaded,
        } = event
        else {
            return;
        };
        apply(context_id, *downloaded);
    })
}

use downloads::{DownloadEvent, DownloadSubject};
use std::rc::Rc;

pub use album_detail::load_album_collection;
pub use album_detail_view::load_album_detail;
pub use artist::{
    artist_detail_route, load_artist_discography, load_artist_overview, load_artist_tracks,
};
pub use named_detail::{NamedDetailId, load_named_detail};
pub use playlist_detail::load_smart_playlist_detail;
