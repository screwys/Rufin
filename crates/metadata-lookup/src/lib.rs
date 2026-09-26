mod cover;
mod http;
mod images;
mod musicbrainz;

pub use images::{
    ArtworkQuery, ArtworkResult, download_artwork, lookup_artist_image, search_artwork,
};

pub use cover::{AlbumCover, AlbumCoverPolicy, lookup_album_cover, public_album_cover_url};
pub use musicbrainz::{
    AlbumReleaseMetadata, identify_album_metadata, identify_artist_metadata,
    identify_track_metadata, lookup_album_release,
};
