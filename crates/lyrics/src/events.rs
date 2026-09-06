use std::path::PathBuf;
use std::sync::Arc;

use playback::CurrentMediaId;
use sources::SourceMetadataError;

use crate::{LyricsDocument, LyricsOrigin, LyricsQuery, LyricsSearchResult};

#[derive(Clone, Debug)]
pub enum CurrentLyricsContent {
    Instrumental,
    Document {
        document: Arc<LyricsDocument>,
        pronunciation: Option<Arc<LyricsDocument>>,
    },
}

#[derive(Clone, Debug)]
pub enum CurrentLyrics {
    Cleared,
    Loading {
        media_id: CurrentMediaId,
    },
    Ready {
        media_id: CurrentMediaId,
        content: Option<CurrentLyricsContent>,
        origin: Option<LyricsOrigin>,
    },
}

impl Default for CurrentLyrics {
    fn default() -> Self {
        Self::Cleared
    }
}

#[derive(Clone, Debug)]
pub enum LyricsEvent {
    JapaneseDictionaryChanged(crate::JapaneseDictionaryStatus),
    Current(CurrentLyrics),
    SearchFinished {
        media_id: CurrentMediaId,
        query: LyricsQuery,
        result: Result<Vec<LyricsSearchResult>, String>,
    },
    Saved {
        media_id: CurrentMediaId,
        path: PathBuf,
    },
    SourceSaveFailed {
        media_id: CurrentMediaId,
        error: SourceMetadataError,
    },
}
