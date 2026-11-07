//! Playlists: adding media to them, creating, editing and deleting them.
mod edit;
mod new;
pub(crate) mod picker;

pub(crate) use edit::{delete_playlist_dialog, delete_playlists_dialog};
