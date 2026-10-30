use std::time::Duration;

use smb_fscc::NotifyAction;

use super::FileSource;
use crate::file::remote::input::FileInput;

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
}
