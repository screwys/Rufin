//! Catalog paging and progress coordination for artwork preparation.
use artwork::{Artwork, ArtworkError};
use sources::SourceId;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    #[error(transparent)]
    Artwork(#[from] ArtworkError),
    #[error("artwork library operation failed: {0}")]
    Library(#[from] library::LibraryError),
}

pub async fn prepare_database_source(
    artwork: &Artwork,
    database: &library::Database,
    source_key: library::SourceKey,
    source_id: &SourceId,
    accepted_digest: [u8; 32],
    progress: &(dyn Fn(u64, usize) + Send + Sync),
    cancelled: Arc<AtomicBool>,
) -> Result<Option<u64>, PreparationError> {
    let accepted_revision = digest_revision(&accepted_digest);
    if artwork.source_preparation_complete(source_id, accepted_revision)? {
        return Ok(None);
    }
    let mut completed = 0_usize;
    let mut visible_progress = false;
    let revision = accepted_revision;
    let manifest = artwork.begin_source_manifest(source_id.clone(), revision)?;
    let mut after_binding = None;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(ArtworkError::Cancelled.into());
        }
        let page = database
            .artwork_preparation_page(
                source_key,
                after_binding.as_deref(),
                128,
                &library::ReadCancellation::new(),
            )
            .await?;
        if page.is_empty() {
            break;
        }
        after_binding = page.last().cloned();
        manifest.record_page(&page)?;
        let artwork = artwork.clone();
        let page_cancelled = Arc::clone(&cancelled);
        let summary = tokio::task::spawn_blocking(move || {
            artwork.prefetch_source_artwork(page.into(), &|_, _| {}, &|| {
                page_cancelled.load(Ordering::Acquire)
            })
        })
        .await
        .map_err(|error| ArtworkError::Decode(error.to_string()))??;
        completed = completed.saturating_add(summary.total);
        if summary.total > summary.cached.saturating_add(summary.missing) {
            visible_progress = true;
        }
        if visible_progress {
            progress(revision, completed);
        }
    }
    manifest.finish()?;
    Ok(Some(revision))
}

fn digest_revision(digest: &[u8; 32]) -> u64 {
    u64::from_le_bytes(digest[..8].try_into().expect("digest prefix"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::runtime::Handle;
    #[tokio::test]
    async fn newly_fetched_background_artwork_reports_progress() {
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("cover.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([30, 80, 160, 255]))
            .save(&image_path)
            .unwrap();
        let source = SourceId::new("source");
        let binding = serde_json::to_vec(&sources::LocalImageRef::File {
            source_id: source.clone(),
            path: image_path.to_string_lossy().into_owned(),
            revision: "1".into(),
        })
        .unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let mut scan = library::Scan::begin(&database, source.as_str(), "Source", "source", None)
            .await
            .unwrap();
        scan.write_artist(
            "artist",
            "Artist",
            "artist",
            Some("artist"),
            None,
            Some(&binding),
            None,
            None,
        )
        .await
        .unwrap();
        let library::ScanOutcome::Changed(publication) = scan.finish().await.unwrap() else {
            panic!("artist artwork must be published");
        };
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        for expected in [1, 0] {
            let progress = AtomicUsize::new(0);
            prepare_database_source(
                &artwork,
                &database,
                publication.source,
                &source,
                publication.artwork_digest,
                &|_, completed| {
                    progress.store(completed, Ordering::Relaxed);
                },
                Arc::new(AtomicBool::new(false)),
            )
            .await
            .unwrap();
            assert_eq!(progress.load(Ordering::Relaxed), expected);
        }
    }

    #[tokio::test]
    async fn completed_database_source_emits_no_preparation_progress() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let scan = library::Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        let publication = match scan.finish().await.unwrap() {
            library::ScanOutcome::Changed(publication)
            | library::ScanOutcome::ArtworkChanged(publication)
            | library::ScanOutcome::Identical(publication) => publication,
            outcome => panic!("unexpected Scan outcome: {outcome:?}"),
        };
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let source = SourceId::new("source");
        artwork
            .begin_source_manifest(source.clone(), digest_revision(&publication.artwork_digest))
            .unwrap()
            .finish()
            .unwrap();
        let progress = AtomicUsize::new(0);
        let result = prepare_database_source(
            &artwork,
            &database,
            publication.source,
            &source,
            publication.artwork_digest,
            &|_, _| {
                progress.fetch_add(1, Ordering::Relaxed);
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(progress.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn manifest_reconciliation_without_fetch_work_stays_silent() {
        let directory = tempfile::tempdir().unwrap();
        let database = library::Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap();
        let scan = library::Scan::begin(&database, "source", "Source", "source", None)
            .await
            .unwrap();
        let publication = match scan.finish().await.unwrap() {
            library::ScanOutcome::Changed(publication)
            | library::ScanOutcome::ArtworkChanged(publication)
            | library::ScanOutcome::Identical(publication) => publication,
            outcome => panic!("unexpected Scan outcome: {outcome:?}"),
        };
        let artwork = Artwork::new(directory.path().join("covers"), Handle::current()).unwrap();
        let progress = AtomicUsize::new(0);
        let result = prepare_database_source(
            &artwork,
            &database,
            publication.source,
            &SourceId::new("source"),
            publication.artwork_digest,
            &|_, _| {
                progress.fetch_add(1, Ordering::Relaxed);
            },
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        assert!(result.is_some());
        assert_eq!(progress.load(Ordering::Relaxed), 0);
    }
}
