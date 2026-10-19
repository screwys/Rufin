use std::sync::{Arc, Mutex};
use std::time::Duration;

use smb_fscc::NotifyAction;

use super::FileSource;
use crate::file::check_cancelled;
use crate::file::remote::input::FileInput;
use crate::{SourceError, SourceResult};

pub(crate) enum FileChange {
    FullRefresh,
    Paths {
        paths: Vec<String>,
        rename: Option<(String, String)>,
    },
}

impl FileChange {
    pub(crate) fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (
                Self::Paths { mut paths, rename },
                Self::Paths {
                    paths: incoming,
                    rename: next,
                },
            ) => {
                if rename.is_some() && next.is_some() {
                    return Self::FullRefresh;
                }
                for path in incoming {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                    if paths.len() > crate::source::LIVE_CHANGE_LIMIT {
                        return Self::FullRefresh;
                    }
                }
                Self::Paths {
                    paths,
                    rename: rename.or(next),
                }
            }
            _ => Self::FullRefresh,
        }
    }
}

impl FileSource {
    pub(crate) fn has_notifications(&self) -> bool {
        self.kind == "smb"
    }

    pub(crate) async fn watch(&self, mut changed: impl FnMut(FileChange) -> bool) {
        let Ok((settings, _)) = self.network() else {
            return;
        };
        loop {
            let mut connected = None;
            for address in std::iter::once(&settings.url).chain(&settings.alternate_urls) {
                match self.connect_input(address).await {
                    Ok(FileInput::Smb(client)) => {
                        connected = Some(client);
                        break;
                    }
                    Ok(_) => return,
                    Err(error) => tracing::debug!(%error, "could not connect SMB notifications"),
                }
            }
            if let Some(client) = connected {
                if !changed(FileChange::FullRefresh) {
                    return;
                }
                let mut old_name = None;
                let result = client
                    .watch(|entry| {
                        let path = entry.file_name.to_string().replace('\\', "/");
                        if super::metadata::is_write_temporary(&path) {
                            return true;
                        }
                        match entry.action {
                            NotifyAction::RenamedOldName => {
                                old_name = Some(path);
                                true
                            }
                            NotifyAction::RenamedNewName => {
                                let rename = old_name.take().map(|old| (old, path.clone()));
                                let mut paths = vec![path];
                                if let Some((old, _)) = &rename {
                                    paths.push(old.clone());
                                }
                                changed(FileChange::Paths { paths, rename })
                            }
                            _ => {
                                let mut paths = vec![path];
                                if let Some(old) = old_name.take() {
                                    paths.push(old);
                                }
                                changed(FileChange::Paths {
                                    paths,
                                    rename: None,
                                })
                            }
                        }
                    })
                    .await;
                match result {
                    Ok(false) => {
                        // Explicit protocol limitation: retain automatic checks on this server.
                        loop {
                            tokio::time::sleep(Duration::from_secs(300)).await;
                            if !changed(FileChange::FullRefresh) {
                                return;
                            }
                        }
                    }
                    Ok(true) => return,
                    Err(error) => tracing::warn!(%error, "SMB notification connection lost"),
                }
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    pub(crate) async fn acquire_paths(
        &self,
        catalog: &mut library::Catalog<'_>,
        paths: &[String],
        rename: Option<&(String, String)>,
        cancelled: &(dyn Fn() -> bool + Send + Sync),
    ) -> SourceResult<()> {
        let database = catalog.database;
        let source_id = catalog.source_id;
        let stamp = catalog.observed;
        let mut affected = Vec::new();
        for path in paths {
            let relative = self.relative(path).ok();
            let path = relative.as_ref().unwrap_or(path);
            if self.includes(path) {
                if !affected.contains(path) {
                    affected.push(path.clone());
                }
            } else {
                for folder in &self.folders() {
                    if folder
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
                        && !self.excludes(folder)
                        && !affected.contains(folder)
                    {
                        affected.push(folder.clone());
                    }
                }
            }
        }
        let paths = affected;
        let input = self.input().await?;
        let seeds = paths
            .iter()
            .map(|path| self.location(path))
            .collect::<SourceResult<Vec<_>>>()?;
        let rename = rename
            .map(|(old, new)| Ok::<_, SourceError>((self.location(old)?, self.location(new)?)))
            .transpose()?;
        let mut prefixes = seeds
            .iter()
            .map(|path| format!("{}/", path.trim_end_matches('/')))
            .collect::<Vec<_>>();
        let mut image_directories = Vec::new();
        for path in &paths {
            if crate::file::artwork::supported_image(std::path::Path::new(path)) {
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                let prefix = format!("{}/", self.location(parent)?.trim_end_matches('/'));
                if !image_directories.contains(&prefix) {
                    image_directories.push(prefix);
                }
            }
        }
        prefixes.extend(image_directories);
        let mut after = None;
        loop {
            let page = database
                .source_component_page(source_id, &seeds, &prefixes, after.as_deref())
                .await?;
            if page.is_empty() {
                break;
            }
            after = page.last().cloned();
            let mut observations = Vec::new();
            let mut missing = Vec::new();
            let result: SourceResult<()> = async {
                for path in &page {
                    match self.stat(&input, &self.relative(path)?).await {
                        Ok(file) => observations.push((file, vec![])),
                        Err(SourceError::NotFound) => missing.push(path.clone()),
                        Err(error) => return Err(error),
                    }
                }
                Ok(())
            }
            .await;
            check_cancelled(cancelled)?;
            catalog
                .write_file_observations(&observations, Vec::new(), true)
                .await?;
            result?;
            check_cancelled(cancelled)?;
            catalog
                .remove_source_files(&missing, stamp.expect("file observation"))
                .await?;
        }
        let workers: [_; 4] =
            std::array::from_fn(|_| Arc::new(Mutex::new(crate::file::media::Worker::default())));
        self.acquire_cues(
            catalog,
            None,
            Arc::clone(&workers[0]),
            &|_| {},
            cancelled,
            rename.as_ref(),
        )
        .await?;
        let mut after = None;
        loop {
            let Some(directory) = database
                .source_next_directory(source_id, stamp, after.as_deref(), &self.location("")?)
                .await?
            else {
                break;
            };
            self.acquire_directory(
                catalog,
                &input,
                &directory,
                Arc::clone(&workers[0]),
                &|_| {},
                cancelled,
                rename.as_ref(),
            )
            .await?;
            after = Some(directory);
        }
        self.acquire_media_files(catalog, &workers, &|_| {}, cancelled, rename.as_ref(), 0)
            .await?;
        crate::file::artwork::ArtworkFiles::Remote(self)
            .acquire(catalog, cancelled)
            .await?;
        Ok(())
    }
}
