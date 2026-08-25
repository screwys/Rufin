mod album_detail;
mod album_detail_view;
mod artist;
mod artist_releases;
pub(super) mod cards;
pub(crate) mod collection_context;
pub(crate) mod collections;
mod columns;
pub(crate) mod detail_links;
mod detail_showcase;
mod factory_cells;
pub(crate) mod folders;
mod grid_cells;
mod grouped_detail;
pub(crate) mod home;
mod home_layout;
pub(crate) mod library_fields;
pub(crate) mod models;
pub(crate) mod named_collections;
mod named_detail;
pub(crate) mod playlist_detail;
pub(crate) mod playlist_entries;
mod playlist_entry_model;
pub(crate) mod playlist_picker;
pub(super) mod release_kind;
pub(crate) mod route;
pub(crate) mod route_layout;
mod route_shell;
mod routes;
mod search;
mod sparse_model;
mod table_links;
mod table_sizing;
mod track_model;

pub(crate) use album_detail_view::load_album_detail;
pub(crate) use artist::{load_artist_discography, load_artist_overview, load_artist_tracks};
pub(crate) use named_detail::{NamedDetailId, load_named_detail};
pub(crate) use playlist_detail::{load_playlist_detail, load_smart_playlist_detail};
pub(crate) use playlist_entry_model::prepare_playlist_entry_positions;
pub(crate) use routes::{
    load_albums, load_artists, load_favorite_tracks, load_history_tracks, load_playlists,
    load_smart_playlists, load_tracks,
};

#[cfg(test)]
mod route_tests;
