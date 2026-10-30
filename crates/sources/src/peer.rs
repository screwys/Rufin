//! Peer bytes are another access route to an existing file identity.
use std::future::Future;
use std::pin::Pin;

use library::AudioEncoding;

use crate::{SourceError, SourceResult};

pub const PEER_REVISION_HEADER: &str = "x-rufin-revision";

pub fn peer_revision(value: &[u8]) -> SourceResult<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| SourceError::Other(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| SourceError::Other(error.to_string()))
}

pub(crate) fn encode_peer_revision(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

pub struct PeerAudio {
    pub reader: Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
    pub length: Option<u64>,
    pub offset: u64,
    pub content_type: Option<String>,
    pub revision: String,
    pub validator: Option<String>,
}

pub trait PeerMedia: Send + Sync {
    fn open<'a>(
        &'a self,
        uri: &'a str,
        encoding: Option<AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> Pin<Box<dyn Future<Output = SourceResult<PeerAudio>> + Send + 'a>>;
}
