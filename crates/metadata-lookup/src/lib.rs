#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

mod acoustid;
mod cache;
pub use acoustid::{FingerprintMatches, lookup_fingerprint};
pub use cache::{CacheMode, LookupCache, cache_time};
mod cover;
mod http;
mod images;
mod musicbrainz;

pub use images::{
    ArtworkQuery, ArtworkResult, download_artwork, lookup_artist_image, search_artwork,
};

pub use cover::{lookup_album_cover, public_album_cover_url};
pub use musicbrainz::{
    MetadataCandidate, MetadataLookup, MetadataMatchKind, lookup_metadata, lookup_release_types,
    metadata_identity, search_metadata,
};
use serde::{Deserialize, Serialize};

/// Where album covers are looked up when files and servers have none, after the album's
/// MusicBrainz IDs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CoverProvider {
    /// Cover Art Archive, found through a MusicBrainz search.
    #[serde(rename = "musicbrainz")]
    MusicBrainz,
    #[serde(rename = "lastfm")]
    LastFm,
}

impl CoverProvider {
    pub const fn all() -> [Self; 2] {
        [Self::MusicBrainz, Self::LastFm]
    }
}

/// Where artist images are looked up when servers have none.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ArtistImageProvider {
    #[serde(rename = "fanart")]
    FanartTv,
    #[serde(rename = "commons")]
    WikimediaCommons,
}

impl ArtistImageProvider {
    pub const fn all() -> [Self; 2] {
        [Self::FanartTv, Self::WikimediaCommons]
    }
}

/// The providers a lookup asks, in order, and the keys they take.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LookupPolicy {
    pub covers: Vec<CoverProvider>,
    pub artist_images: Vec<ArtistImageProvider>,
    pub lastfm_api_key: String,
    /// The user's own fanart.tv key, sent beside Rufin's project key when set.
    pub fanart_api_key: String,
}
