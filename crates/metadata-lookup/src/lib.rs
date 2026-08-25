mod cover;
mod http;
mod musicbrainz;

pub use cover::{AlbumCover, AlbumCoverPolicy, lookup_album_cover, public_album_cover_url};
pub use musicbrainz::{
    AlbumReleaseMetadata, identify_album_metadata, identify_artist_metadata,
    identify_track_metadata, lookup_album_release,
};
