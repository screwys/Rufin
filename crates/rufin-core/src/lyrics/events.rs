use std::path::PathBuf;
use std::sync::Arc;

use playback::CurrentMediaId;
use sources::SourceMetadataError;

use ::lyrics::{LyricsDocument, LyricsOrigin, LyricsQuery, LyricsSearchResult};

#[derive(Clone, Debug)]
pub enum CurrentLyricsContent {
    Instrumental,
    Document {
        document: Arc<LyricsDocument>,
        pronunciation: Option<Arc<LyricsDocument>>,
        prepared: Arc<::lyrics::PreparedLyrics>,
    },
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
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
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub enum LyricsEvent {
    JapaneseDictionaryChanged(::lyrics::JapaneseDictionaryStatus),
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

#[cfg_attr(feature = "bindings", uniffi::export)]
impl CurrentLyrics {
    pub fn media_id(&self) -> Option<CurrentMediaId> {
        match self {
            Self::Cleared => None,
            Self::Loading { media_id } | Self::Ready { media_id, .. } => Some(media_id.clone()),
        }
    }

    pub fn document_changed(&self, previous: Option<Arc<CurrentLyrics>>) -> bool {
        self.document() != previous.as_deref().map(Self::document).unwrap_or_default()
    }

    pub fn text(&self, offset: i64) -> Option<String> {
        self.document()
            .1
            .map(|document| ::lyrics::lyrics_to_lrc_text(document, offset))
    }
}

impl CurrentLyrics {
    fn document(&self) -> (Option<&str>, Option<&Arc<LyricsDocument>>) {
        match self {
            Self::Cleared => (None, None),
            Self::Loading { media_id } => (Some(&media_id.media_uri), None),
            Self::Ready {
                media_id, content, ..
            } => (
                Some(&media_id.media_uri),
                match content {
                    Some(CurrentLyricsContent::Document { document, .. }) => Some(document),
                    _ => None,
                },
            ),
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl LyricsEvent {
    pub fn media_id(&self) -> Option<CurrentMediaId> {
        match self {
            Self::SearchFinished { media_id, .. }
            | Self::Saved { media_id, .. }
            | Self::SourceSaveFailed { media_id, .. } => Some(media_id.clone()),
            Self::Current(current) => current.media_id(),
            Self::JapaneseDictionaryChanged(_) => None,
        }
    }
    pub fn query(&self) -> Option<LyricsQuery> {
        match self {
            Self::SearchFinished { query, .. } => Some(query.clone()),
            _ => None,
        }
    }
    pub fn results(&self) -> Vec<LyricsSearchResult> {
        match self {
            Self::SearchFinished {
                result: Ok(results),
                ..
            } => results.clone(),
            _ => Vec::new(),
        }
    }
    pub fn error(&self) -> Option<String> {
        match self {
            Self::SearchFinished {
                result: Err(error), ..
            } => Some(error.clone()),
            Self::SourceSaveFailed { error, .. } => Some(error.to_string()),
            _ => None,
        }
    }
    pub fn saved(&self) -> bool {
        matches!(self, Self::Saved { .. })
    }
    pub fn dictionary_status(&self) -> Option<::lyrics::JapaneseDictionaryStatus> {
        match self {
            Self::JapaneseDictionaryChanged(status) => Some(status.clone()),
            _ => None,
        }
    }
}
