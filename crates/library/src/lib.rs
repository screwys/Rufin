//! Rufin's durable music library.
//!
//! Concrete sources provide canonical facts. [`Libraries`] accepts those facts
//! into its private SQLite Store and hydrates one source-scoped [`Library`].
//! Rufin owns which source is selected; routes and Playback never query SQLite
//! or receive a general Store handle.

use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

macro_rules! opaque_id {
    ($name:ident, $prefix:literal) => {
        #[derive(
            Clone,
            Debug,
            serde::Deserialize,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            serde::Serialize,
        )]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                assert!(
                    !value.is_empty(),
                    concat!(stringify!($name), " cannot be empty")
                );
                Self(value)
            }

            pub fn fake(number: impl std::fmt::Display) -> Self {
                Self::new(format!("{}{}", $prefix, number))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

mod activity;
mod album_release;
mod browse;
mod download_coverage;
mod favorites;
mod home;
mod items;
mod loaded;
mod local;
mod local_playback;
mod loudness;
mod lyrics_cache;
mod metadata;
mod playback_state;
mod playlists;
mod radio;
pub(crate) mod refresh;
mod scrobbles;
mod search;
pub mod smart_playlists;
mod store;
mod stream;

pub use activity::*;
pub use album_release::*;
pub use browse::*;
pub use favorites::*;
pub use home::*;
pub use items::*;
pub use loaded::*;
pub use local::*;
pub use local_playback::*;
pub use loudness::*;
pub use lyrics_cache::*;
pub use metadata::*;
pub use playback_state::*;
pub use playlists::*;
pub use radio::*;
pub use refresh::*;
pub use scrobbles::*;
pub use search::*;
pub use smart_playlists::{
    SmartPlaylist, SmartPlaylistBuiltin, SmartPlaylistDefinition, SmartPlaylistDetail,
    SmartPlaylistRecord, SmartPlaylistRule, SmartPlaylistRuleField, SmartPlaylistRuleOperator,
    SmartPlaylistRuleValue, SmartPlaylistRuleValueKind, SmartPlaylistSortField,
    SmartPlaylistSummary,
};
pub use store::StoreRepairReport;
pub use stream::*;

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("unsupported Store (application ID {application_id}, schema {user_version})")]
    UnsupportedStore {
        application_id: i64,
        user_version: i64,
    },
    #[error("invalid final Store: {0}")]
    InvalidStore(String),
    #[error("Library persistence failed: {0}")]
    Persistence(String),
    #[error("a source candidate cannot continue after a batch write failed")]
    CandidateWriteFailed,
    #[error(transparent)]
    Query(#[from] LibraryQueryError),
}

pub type LibraryResult<T> = Result<T, LibraryError>;

impl From<store::StoreError> for LibraryError {
    fn from(error: store::StoreError) -> Self {
        match error {
            store::StoreError::UnsupportedSchema {
                application_id,
                user_version,
            } => Self::UnsupportedStore {
                application_id,
                user_version,
            },
            store::StoreError::InvalidFinalSchema(message) => Self::InvalidStore(message),
            error => Self::Persistence(error.to_string()),
        }
    }
}

/// Cloneable access to the Store operations shared by every source library.
///
/// Operations are blocking because they cross the one Store lane. Rufin calls
/// them from its blocking boundary, never from GTK or a Tokio worker.
#[derive(Clone)]
pub struct Libraries {
    store: store::StoreLane,
    home_sessions: Arc<home::HomeSessions>,
}

impl Libraries {
    pub fn open(path: impl AsRef<Path>) -> LibraryResult<Self> {
        Ok(Self {
            store: store::StoreLane::open(path.as_ref().to_path_buf())?,
            home_sessions: Arc::new(home::HomeSessions::new()),
        })
    }

    pub fn memory() -> LibraryResult<Self> {
        Ok(Self {
            store: store::StoreLane::memory()?,
            home_sessions: Arc::new(home::HomeSessions::new()),
        })
    }

    /// Opens a healthy Store or preserves and replaces an unusable Store.
    ///
    /// Recognized current Stores contribute readable Rufin-owned rows. Every
    /// other unusable Store starts clean so the source owner can rebuild its
    /// ordinary facts without making persisted content a launch gate.
    pub fn open_with_repair(
        path: impl AsRef<Path>,
    ) -> LibraryResult<(Self, Option<StoreRepairReport>)> {
        let (store, repair) = store::StoreLane::open_with_repair(path.as_ref().to_path_buf())?;
        Ok((
            Self {
                store,
                home_sessions: Arc::new(home::HomeSessions::new()),
            },
            repair,
        ))
    }

    pub fn load_source(&self, source_id: &SourceId) -> LibraryResult<Option<Arc<Library>>> {
        let loaded = self
            .store
            .load_current(source_id.clone())?
            .map(|input| Library::build(input, self.store.clone(), Arc::clone(&self.home_sessions)))
            .transpose()
            .map_err(LibraryError::from)?;
        if let Some(loaded) = &loaded {
            loaded.prepare_home()?;
        }
        Ok(loaded)
    }

    /// Removes every Store row owned by one forgotten source.
    ///
    /// Pending external scrobbles are account-scoped delivery work rather than
    /// source data and survive.
    pub fn remove_source_data(&self, source_id: &SourceId) -> LibraryResult<()> {
        self.store.remove_source_data(source_id.clone())?;
        self.home_sessions.remove_source(source_id)?;
        Ok(())
    }

    pub fn begin_source_candidate(
        &self,
        header: CandidateHeader,
    ) -> LibraryResult<SourceCandidate> {
        let source_id = header.source_id.clone();
        let library_id = loop {
            match self.store.begin_candidate(header.clone()) {
                Ok(library_id) => break library_id,
                Err(store::StoreError::CandidateCleanupPending(pending))
                    if pending == source_id =>
                {
                    // Each typed request lets the Store retire one bounded
                    // cleanup batch before this retry reaches the lane.
                    std::thread::yield_now();
                }
                Err(error) => return Err(error.into()),
            }
        };
        Ok(SourceCandidate::new(self.clone(), header, library_id))
    }

    pub(crate) fn write_candidate(
        &self,
        library_id: i64,
        batch: CandidateBatch,
    ) -> LibraryResult<()> {
        Ok(self.store.write_candidate(library_id, batch)?)
    }

    pub(crate) fn schedule_candidate_cleanup(&self, library_id: i64) {
        self.store.schedule_cleanup(library_id);
    }
}

pub(crate) const fn msgid(message: &'static str) -> &'static str {
    message
}
