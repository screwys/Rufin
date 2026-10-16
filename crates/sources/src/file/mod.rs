//! Shared file interpretation with concrete local and network acquisition.

pub(crate) mod artwork;
pub(crate) mod cue;
pub(crate) mod discovery;
pub(crate) mod documents;
pub(crate) mod local;
pub(crate) mod lofty;
pub(crate) mod media;
pub(crate) mod metadata;
pub(crate) mod playlist;
pub(crate) mod remote;

fn check_cancelled(cancelled: &(dyn Fn() -> bool + Send + Sync)) -> crate::SourceResult<()> {
    if cancelled() {
        Err(crate::SourceError::Cancelled)
    } else {
        Ok(())
    }
}
