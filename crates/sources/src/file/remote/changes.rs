use std::time::Duration;

use library::{Database, LocalFileKind, Scan};
use smb_fscc::NotifyAction;

use super::RemoteSource;
use crate::file::remote::input::FileInput;
use crate::{SourceError, SourceResult};

pub(crate) enum FileChange {
    Inventory,
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
                    return Self::Inventory;
                }
                for path in incoming {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                    if paths.len() > crate::source::LIVE_CHANGE_LIMIT {
                        return Self::Inventory;
                    }
                }
                Self::Paths {
                    paths,
                    rename: rename.or(next),
                }
            }
            _ => Self::Inventory,
        }
    }
}

impl RemoteSource {
    pub(crate) fn has_notifications(&self) -> bool {
        self.kind == "smb"
    }

    pub(crate) async fn watch(&self, mut changed: impl FnMut(FileChange) -> bool) {
        loop {
            let mut connected = None;
            for address in std::iter::once(&self.settings.url).chain(&self.settings.alternate_urls)
            {
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
                if !changed(FileChange::Inventory) {
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
                            if !changed(FileChange::Inventory) {
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

    pub(crate) async fn publish_paths(
        &self,
        database: &Database,
        source: library::SourceKey,
        paths: &[String],
        rename: Option<&(String, String)>,
    ) -> SourceResult<library::ScanOutcome> {
        let mut affected = Vec::new();
        for path in paths {
            if self.includes(path) {
                if !affected.contains(path) {
                    affected.push(path.clone());
                }
            } else {
                for folder in &self.settings.folders {
                    if folder
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
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
        let mut scan = Scan::begin_items(database, self.source_id.as_str()).await?;
        for page in seeds.chunks(128) {
            scan.write_local_component_paths(page).await?;
        }
        // The same inventory dependency expansion used by Local includes CUE backing files.
        // Prefixes include old directory contents even when a moved directory no longer exists.
        let prefixes = seeds
            .iter()
            .map(|path| format!("{}/", path.trim_end_matches('/')))
            .collect::<Vec<_>>();
        for page in prefixes.chunks(128) {
            scan.expand_local_artwork_prefixes(source, page, &[])
                .await?;
        }
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
        for page in image_directories.chunks(128) {
            scan.expand_local_artwork_prefixes(source, &[], page)
                .await?;
        }
        scan.expand_local_component(source).await?;
        scan.remove_local_component_tracks(source).await?;
        let mut after = None;
        loop {
            let page = scan
                .local_component_path_page(after.as_deref(), 128)
                .await?;
            if page.is_empty() {
                break;
            }
            after = page.last().cloned();
            for path in &page {
                match self.stat(&input, &self.relative(path)?).await {
                    Ok(file) => scan.write_local_files(&[(file, vec![])]).await?,
                    Err(SourceError::NotFound) => {}
                    Err(error) => return Err(error),
                }
            }
            scan.remove_local_file_paths(&page).await?;
        }
        let mut after = None;
        loop {
            let page = scan
                .local_inventory_path_page(LocalFileKind::Directory, after.as_deref(), false, 1)
                .await?;
            let Some(directory) = page.into_iter().next() else {
                break;
            };
            self.list_directory(&input, &mut scan, &self.relative(&directory)?, &|| false)
                .await?;
            after = Some(directory);
        }
        self.stage_files(database, &mut scan, &|_| {}, &|| false, rename.as_ref())
            .await?;
        self.stage_artwork(database, &mut scan, &|| false).await?;
        Ok(scan.finish().await?)
    }
}
