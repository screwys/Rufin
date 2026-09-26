use library::Scan;

use super::RemoteSource;
use crate::{LocalImageRef, SourceResult};

impl RemoteSource {
    pub(crate) async fn directory_image(
        &self,
        scan: &Scan,
        prefix: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        self.directory_image_by_role(scan, prefix, None).await
    }

    pub(crate) async fn directory_artist_image_for(
        &self,
        scan: &Scan,
        prefix: &str,
        artist: &str,
        allow_generic: bool,
    ) -> SourceResult<Option<LocalImageRef>> {
        self.directory_image_by_role(scan, prefix, Some((artist, allow_generic)))
            .await
    }

    async fn directory_image_by_role(
        &self,
        scan: &Scan,
        prefix: &str,
        artist: Option<(&str, bool)>,
    ) -> SourceResult<Option<LocalImageRef>> {
        let mut after = String::new();
        let mut best = None;
        let mut count = 0;
        loop {
            let page = scan.file_artwork_image_page(prefix, &after).await?;
            if page.is_empty() {
                break;
            }
            for (path, revision) in page {
                after.clone_from(&path);
                let relative = self.relative(&path)?;
                let name = relative.rsplit('/').next().unwrap_or_default();
                if !crate::file::artwork::supported_image(std::path::Path::new(name)) {
                    continue;
                }
                if artist.is_none() && crate::file::artwork::is_artist_image(name) {
                    continue;
                }
                count += 1;
                let rank = if let Some((artist, allow_generic)) = artist {
                    crate::file::artwork::artist_image_rank_for(name, artist, allow_generic)
                } else {
                    crate::file::artwork::image_rank(name)
                };
                let candidate = (rank, path, revision);
                if best.as_ref().is_none_or(|current| &candidate < current) {
                    best = Some(candidate);
                }
            }
        }
        Ok(best
            .filter(|(rank, _, _)| *rank != usize::MAX || artist.is_none() && count == 1)
            .map(|(_, path, revision)| LocalImageRef::File {
                source_id: self.source_id.clone(),
                path,
                revision: revision.unwrap_or_default(),
            }))
    }
}
