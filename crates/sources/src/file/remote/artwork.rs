use library::{Database, Scan};

use super::RemoteSource;
use crate::{LocalImageRef, SourceError, SourceResult};

impl RemoteSource {
    pub(crate) async fn stage_artwork(
        &self,
        database: &Database,
        scan: &mut Scan,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        scan.clear_local_artwork_candidates().await?;
        let distinct = database.distinct_track_covers();
        let mut after_album = String::new();
        loop {
            let albums = scan.local_artwork_album_page(&after_album).await?;
            if albums.is_empty() {
                break;
            }
            for album in albums {
                after_album.clone_from(&album);
                let mut after = None;
                let mut group = None;
                let mut directories: [Option<(String, Option<LocalImageRef>)>; 2] = [None, None];
                loop {
                    let candidates = scan
                        .local_artwork_track_page(&album, after.as_ref())
                        .await?;
                    if candidates.is_empty() {
                        break;
                    }
                    for candidate in candidates {
                        if cancelled() {
                            return Err(SourceError::Cancelled);
                        }
                        if group.is_none() {
                            let relative = self.relative(&candidate.path)?;
                            let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
                            let ancestor = (!parent.is_empty())
                                .then(|| parent.rsplit_once('/').map_or("", |(parent, _)| parent));
                            for (slot, directory) in
                                std::iter::once(parent).chain(ancestor).enumerate()
                            {
                                if directories[slot]
                                    .as_ref()
                                    .is_none_or(|(path, _)| path != directory)
                                {
                                    let prefix = format!(
                                        "{}/",
                                        self.location(directory)?.trim_end_matches('/')
                                    );
                                    let image = if scan
                                        .local_artwork_directory_is_single_album(&prefix)
                                        .await?
                                    {
                                        self.directory_image(scan, &prefix).await?
                                    } else {
                                        None
                                    };
                                    directories[slot] = Some((directory.into(), image));
                                }
                                group = directories[slot]
                                    .as_ref()
                                    .and_then(|(_, image)| image.clone());
                                if group.is_some() {
                                    break;
                                }
                            }
                        }
                        let embedded = candidate
                            .picture_index
                            .and_then(|index| u32::try_from(index).ok())
                            .map(|picture_index| LocalImageRef::Embedded {
                                source_id: self.source_id.clone(),
                                path: candidate.path.clone(),
                                revision: candidate.revision.clone().unwrap_or_default(),
                                picture_index,
                            });
                        if group.is_none() {
                            group.clone_from(&embedded);
                        }
                        let group_bytes = group.as_ref().map(serde_json::to_vec).transpose()?;
                        let track_bytes = distinct
                            .then_some(embedded.as_ref())
                            .flatten()
                            .map(serde_json::to_vec)
                            .transpose()?;
                        scan.write_local_artwork_candidate(
                            &album,
                            &candidate.object_id,
                            group_bytes.as_deref(),
                            track_bytes.as_deref(),
                        )
                        .await?;
                        after = Some(candidate);
                        if !distinct && group.is_some() {
                            break;
                        }
                    }
                    if !distinct && group.is_some() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    async fn directory_image(
        &self,
        scan: &Scan,
        prefix: &str,
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
                count += 1;
                let candidate = (crate::file::artwork::image_rank(name), path, revision);
                if best.as_ref().is_none_or(|current| &candidate < current) {
                    best = Some(candidate);
                }
            }
        }
        Ok(best
            .filter(|(rank, _, _)| *rank != usize::MAX || count == 1)
            .map(|(_, path, revision)| LocalImageRef::File {
                source_id: self.source_id.clone(),
                path,
                revision: revision.unwrap_or_default(),
            }))
    }
}
