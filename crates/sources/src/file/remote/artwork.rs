use library::Scan;

use super::FileSource;
use crate::{LocalImageRef, SourceResult};

impl FileSource {
    pub(crate) async fn directory_image(
        &self,
        scan: &Scan,
        prefix: &str,
    ) -> SourceResult<Option<LocalImageRef>> {
        directory_image_by_role(&self.source_id, scan, prefix, None).await
    }

    pub(crate) async fn directory_artist_image_for(
        &self,
        scan: &Scan,
        prefix: &str,
        artist: &str,
        allow_generic: bool,
    ) -> SourceResult<Option<LocalImageRef>> {
        directory_image_by_role(&self.source_id, scan, prefix, Some((artist, allow_generic))).await
    }
}

pub(crate) async fn directory_image_by_role(
    source_id: &crate::SourceId,
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
            let uri = url::Url::parse(&path).map_err(|_| crate::SourceError::NotFound)?;
            let relative = percent_encoding::percent_decode_str(uri.path())
                .decode_utf8()
                .map_err(|_| crate::SourceError::NotFound)?;
            let relative = if uri.scheme() == "rufin-document" {
                crate::file::documents::display_path(&relative)
            } else {
                relative.into_owned()
            };
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
            source_id: source_id.clone(),
            path,
            revision: revision.unwrap_or_default(),
        }))
}
