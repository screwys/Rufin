//! File association and synchronization for authored playlists.
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::Duration,
};

use async_channel::Receiver;
use gio::prelude::*;
use library::{PlaylistFileLink, PlaylistKey, PlaylistPathMode, Scope, SourceId};

use crate::features::{Feature, Pass};
use crate::settings::StoredSettings;
use crate::source::{Shared, SourceOwner};
use crate::string_error;

const LOCAL_CONFLICT: &str =
    "Both the playlist and its file have changed. Reload the file or save the Rufin version.";
pub(crate) const FILE_CONFLICT: &str =
    "The playlist file has changed. Reload the file or replace it with the Rufin version.";

/// How often linked files are checked for changes made outside Rufin.
const CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Linked playlist files while there are any. Linked playlists an edit touched are saved when
/// their links are marked changed and save automatically; the edit itself marks its link
/// changed. Files are checked for changes made outside Rufin when it starts and every five
/// minutes.
pub(crate) struct Links(pub(crate) Weak<Shared>);

impl Feature for Links {
    fn wanted(&self, _: &StoredSettings, catalog: &library::Catalog) -> bool {
        catalog.has_linked_playlists()
    }

    fn hears(&self, scope: &Scope) -> bool {
        matches!(
            scope,
            Scope::Reload | Scope::Playlist { .. } | Scope::PlaylistLink { .. }
        )
    }

    fn period(&self) -> Option<Duration> {
        Some(CHECK_INTERVAL)
    }

    fn apply(self: Arc<Self>, changes: Option<Vec<Arc<library::Change>>>) -> Pass {
        Box::pin(async move {
            let Some(shared) = self.0.upgrade() else {
                return;
            };
            let Some(changes) = changes else {
                refresh_links(&SourceOwner { shared }, None).await;
                return;
            };
            let catalog = &shared.catalog;
            let mut edited = changes
                .iter()
                .flat_map(|change| &change.scopes)
                .filter_map(|scope| match scope {
                    // A link's settings change saves edits that waited for automatic saving.
                    Scope::Playlist { key, .. } | Scope::PlaylistLink { key }
                        if catalog.is_linked(*key) =>
                    {
                        Some(*key)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            edited.sort_unstable();
            edited.dedup();
            let owner = SourceOwner { shared };
            for playlist in edited {
                after_edit(&owner, playlist).await;
            }
        })
    }
}

pub fn is_conflict(error: &str) -> bool {
    matches!(error, LOCAL_CONFLICT | FILE_CONFLICT)
}

#[derive(Clone, Debug, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistFileSettings {
    pub can_link: bool,
    pub link: Option<PlaylistFileLink>,
    pub display_path: Option<String>,
    pub source_auto_save: bool,
}

pub(crate) fn run<T, Work>(
    owner: &SourceOwner,
    work: impl FnOnce(SourceOwner) -> Work + Send + 'static,
) -> Receiver<Result<T, String>>
where
    T: Send + 'static,
    Work: Future<Output = Result<T, String>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    let task = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task;
        let result = work(owner.clone()).await;
        if let Err(error) = &result {
            owner.shared.warn_nonfatal(error);
        }
        let _ = sender.send(result).await;
    });
    receiver
}

pub fn settings(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<PlaylistFileSettings, String>> {
    owner.reply(move |owner, catalog| async move {
        let link = catalog
            .get_playlist_file_link(playlist)
            .await
            .map_err(string_error)?;
        let display_path = if let Some(link) = &link {
            Some(
                if link.source_id.is_none() && link.path.starts_with("content:") {
                    document_entry(&link.path)
                        .await
                        .map(|entry| entry.name)
                        .unwrap_or_else(|_| link.path.clone())
                } else {
                    local_path(&owner, link)
                        .map(|path| {
                            library::playlist_host_path(&path)
                                .to_string_lossy()
                                .into_owned()
                        })
                        .unwrap_or_else(|| link.path.clone())
                },
            )
        } else {
            None
        };
        let source_auto_save = catalog
            .playlist_file_auto_save(link.as_ref().and_then(|link| link.source_id.as_ref()))
            .await
            .map_err(string_error)?;
        let can_link = catalog
            .playlist_file_linkable(playlist)
            .await
            .map_err(string_error)?;
        Ok(PlaylistFileSettings {
            can_link,
            link,
            display_path,
            source_auto_save,
        })
    })
}

pub fn source_auto_save(
    owner: &SourceOwner,
    source_id: SourceId,
) -> Receiver<Result<bool, String>> {
    owner.reply(move |_, catalog| async move {
        catalog
            .playlist_file_auto_save(Some(&source_id))
            .await
            .map_err(string_error)
    })
}

pub fn set_source_auto_save(
    owner: &SourceOwner,
    source_id: SourceId,
    enabled: bool,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        owner
            .shared
            .catalog
            .set_playlist_file_auto_save(Some(&source_id), enabled)
            .await
            .map_err(string_error)?;
        if enabled {
            refresh_links(&owner, Some(&source_id)).await;
        }
        Ok(())
    })
}

/// Saves a linked playlist's file settings. The name and artwork are edited with
/// `playlists::update_playlist`.
pub fn update(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    auto_refresh: bool,
    auto_save: Option<bool>,
    path_mode: PlaylistPathMode,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let Some(mut link) = owner
            .shared
            .catalog
            .get_playlist_file_link(playlist)
            .await
            .map_err(string_error)?
        else {
            return Ok(());
        };
        link.auto_refresh = auto_refresh;
        link.auto_save = auto_save;
        link.path_mode = path_mode;
        owner
            .shared
            .catalog
            .save_playlist_file_link(&link)
            .await
            .map_err(string_error)?;
        Ok(())
    })
}

pub fn link_local(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    path: PathBuf,
    replace_file: bool,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let (source, location, _) = local_location(&owner, &path);
        link_on(&owner, playlist, source, location, replace_file).await
    })
}

pub fn link_source(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    source_id: SourceId,
    path: String,
    replace_file: bool,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let path = source_location(&owner, &source_id, &path)?;
        link_on(&owner, playlist, Some(source_id), path, replace_file).await
    })
}

pub fn link_document(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    uri: String,
    replace_file: bool,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        link_on(&owner, playlist, None, uri, replace_file).await
    })
}

async fn link_on(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    source_id: Option<SourceId>,
    path: String,
    replace_file: bool,
) -> Result<(), String> {
    check_link(owner, playlist, source_id.as_ref(), &path).await?;
    let previous = owner
        .shared
        .catalog
        .get_playlist_file_link(playlist)
        .await
        .map_err(string_error)?;
    let link = PlaylistFileLink {
        playlist,
        source_id,
        path,
        auto_refresh: true,
        auto_save: None,
        path_mode: PlaylistPathMode::Automatic,
        revision: None,
        dirty: replace_file,
        error: None,
    };
    owner
        .shared
        .catalog
        .save_playlist_file_link(&link)
        .await
        .map_err(string_error)?;
    let result = if replace_file {
        save_on(owner, &link, true).await
    } else {
        reload_on(owner, &link, true).await.map(|_| ())
    };
    if let Err(error) = result {
        if let Some(previous) = previous {
            owner
                .shared
                .catalog
                .save_playlist_file_link(&previous)
                .await
                .map_err(string_error)?;
        } else {
            owner
                .shared
                .catalog
                .unlink_playlist_file(playlist)
                .await
                .map_err(string_error)?;
        }
        return Err(error);
    }
    associate(owner, &link).await?;
    Ok(())
}

pub(crate) async fn check_link(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    source: Option<&SourceId>,
    path: &str,
) -> Result<(), String> {
    if !owner
        .shared
        .catalog
        .playlist_file_linkable(playlist)
        .await
        .map_err(string_error)?
    {
        return Err("Export a copy of this provider playlist before linking a file".into());
    }
    let name = if source.is_none() && path.starts_with("content:") {
        document_entry(path).await?.name
    } else {
        path.to_owned()
    };
    if library::PlaylistFormat::from_path(Path::new(&name)).is_none() {
        return Err("Unsupported playlist format".into());
    }
    if owner
        .shared
        .catalog
        .find_playlist_file_link(source, path)
        .await
        .map_err(string_error)?
        .is_some_and(|link| link.playlist != playlist)
    {
        return Err("This file is already linked to another playlist".into());
    }
    Ok(())
}

pub(crate) async fn import_target(
    owner: &SourceOwner,
    source: Option<&SourceId>,
    path: &str,
    linked: bool,
) -> Result<Option<PlaylistKey>, String> {
    if !linked {
        return Ok(None);
    }
    let link = owner
        .shared
        .catalog
        .find_playlist_file_link(source, path)
        .await
        .map_err(string_error)?;
    if link.as_ref().is_some_and(|link| link.dirty) {
        return Err(LOCAL_CONFLICT.into());
    }
    Ok(link.map(|link| link.playlist))
}

pub fn unlink(owner: &SourceOwner, playlist: PlaylistKey) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        forget_file(&owner, playlist).await?;
        owner
            .shared
            .catalog
            .unlink_playlist_file(playlist)
            .await
            .map_err(string_error)?;
        Ok(())
    })
}

pub fn save(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    overwrite: bool,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let link = required_link(&owner, playlist).await?;
        let result = save_on(&owner, &link, overwrite).await;
        record_result(&owner, &link, &result).await;
        result
    })
}

pub fn reload(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    discard_changes: bool,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let link = required_link(&owner, playlist).await?;
        let result = reload_on(&owner, &link, discard_changes).await.map(|_| ());
        record_result(&owner, &link, &result).await;
        result
    })
}

pub fn delete_file(owner: &SourceOwner, playlist: PlaylistKey) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let link = required_link(&owner, playlist).await?;
        if let Some(source) = &link.source_id {
            owner
                .client(source)?
                .delete_playlist_file(&link.path)
                .await
                .map_err(string_error)?;
        } else if link.path.starts_with("content:") {
            let uri = link.path.clone();
            tokio::task::spawn_blocking(move || sources::delete_document(&uri))
                .await
                .map_err(string_error)?
                .map_err(string_error)?;
        } else {
            std::fs::remove_file(&link.path).map_err(string_error)?;
        }
        forget_file(&owner, playlist).await?;
        owner
            .shared
            .catalog
            .delete_playlists(&[playlist])
            .await
            .map_err(string_error)?;
        Ok(())
    })
}

pub fn rename_file(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: String,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let mut link = required_link(&owner, playlist).await?;
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
            return Err("Enter a filename without folders".into());
        }
        let document = if link.source_id.is_none() && link.path.starts_with("content:") {
            Some(document_entry(&link.path).await?)
        } else {
            None
        };
        let is_document = document.is_some();
        if library::PlaylistFormat::from_path(Path::new(&name))
            != library::PlaylistFormat::from_path(Path::new(
                document.as_ref().map_or(&link.path, |entry| &entry.name),
            ))
        {
            return Err("Use Export to change the playlist format".into());
        }
        let previous = link.path.clone();
        let next = if let Some(document) = document {
            if document.name == name {
                return Ok(());
            }
            let uri = link.path.clone();
            let document_name = name.clone();
            tokio::task::spawn_blocking(move || sources::rename_document(&uri, &document_name))
                .await
                .map_err(string_error)?
                .map_err(string_error)?
                .uri
        } else if local_path(&owner, &link).is_some() {
            Path::new(&link.path)
                .with_file_name(&name)
                .to_string_lossy()
                .into_owned()
        } else {
            link.path
                .rsplit_once('/')
                .map_or_else(|| name.clone(), |(parent, _)| format!("{parent}/{name}"))
        };
        if previous == next && !is_document {
            return Ok(());
        }
        check_link(&owner, playlist, link.source_id.as_ref(), &next).await?;
        if let Some(path) = local_path(&owner, &link) {
            gio::File::for_path(&path)
                .move_(
                    &gio::File::for_path(path.with_file_name(&name)),
                    gio::FileCopyFlags::NONE,
                    gio::Cancellable::NONE,
                    None,
                )
                .map_err(string_error)?;
        } else if let Some(source) = &link.source_id {
            owner
                .client(source)?
                .rename_playlist_file(&previous, &next)
                .await
                .map_err(string_error)?;
        }
        owner
            .shared
            .catalog
            .ignore_playlist_file(link.source_id.as_ref(), &previous)
            .await
            .map_err(string_error)?;
        link.path = next;
        link.revision = Some(revision(&owner, &link).await?);
        owner
            .shared
            .catalog
            .save_playlist_file_link(&link)
            .await
            .map_err(string_error)?;
        Ok(())
    })
}

async fn required_link(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Result<PlaylistFileLink, String> {
    owner
        .shared
        .catalog
        .get_playlist_file_link(playlist)
        .await
        .map_err(string_error)?
        .ok_or_else(|| "Playlist is not linked to a file".into())
}

pub(crate) async fn forget_file(owner: &SourceOwner, playlist: PlaylistKey) -> Result<(), String> {
    if let Some(link) = owner
        .shared
        .catalog
        .get_playlist_file_link(playlist)
        .await
        .map_err(string_error)?
    {
        owner
            .shared
            .catalog
            .ignore_playlist_file(link.source_id.as_ref(), &link.path)
            .await
            .map_err(string_error)?;
    }
    Ok(())
}

pub(crate) fn local_location(
    owner: &SourceOwner,
    path: &Path,
) -> (Option<SourceId>, String, PathBuf) {
    for configured in &owner.shared.settings.load().sources.configured {
        if let Some((location, access)) = configured.configuration.local_file_location(path) {
            return (
                Some(configured.configuration.source_id.clone()),
                location,
                access,
            );
        }
    }
    (None, path.to_string_lossy().into_owned(), path.to_owned())
}

pub(crate) fn source_location(
    owner: &SourceOwner,
    source: &SourceId,
    path: &str,
) -> Result<String, String> {
    let client = owner.client(source)?;
    Ok(client
        .local_playlist_path(path)
        .and_then(|path| owner.configuration(source)?.local_file_location(&path))
        .map_or_else(|| path.to_owned(), |(location, _)| location))
}

fn local_path(owner: &SourceOwner, link: &PlaylistFileLink) -> Option<PathBuf> {
    match &link.source_id {
        None if link.path.starts_with("content:") => None,
        None => Some(PathBuf::from(&link.path)),
        Some(source) => owner.client(source).ok()?.local_playlist_path(&link.path),
    }
}

async fn document_entry(uri: &str) -> Result<sources::DocumentEntry, String> {
    let uri = uri.to_owned();
    tokio::task::spawn_blocking(move || sources::stat_document(&uri))
        .await
        .map_err(string_error)?
        .map_err(string_error)
}

async fn revision(owner: &SourceOwner, link: &PlaylistFileLink) -> Result<String, String> {
    if let Some(path) = local_path(owner, link) {
        return tokio::task::spawn_blocking(move || sources::playlist_file_revision(&path))
            .await
            .map_err(string_error)?
            .map_err(string_error);
    }
    match &link.source_id {
        None => {
            let uri = link.path.clone();
            tokio::task::spawn_blocking(move || {
                let entry = sources::stat_document(&uri).map_err(string_error)?;
                if let Some(revision) = entry.revision {
                    return Ok(revision);
                }
                let mut input = sources::open_document_input(&uri).map_err(string_error)?;
                let mut hash = blake3::Hasher::new();
                std::io::copy(&mut input, &mut hash).map_err(string_error)?;
                Ok(hash.finalize().to_hex().to_string())
            })
            .await
            .map_err(string_error)?
        }
        Some(source) => owner
            .client(source)?
            .playlist_file_revision(&link.path)
            .await
            .map_err(string_error),
    }
}

pub(crate) async fn remember(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    source_id: Option<SourceId>,
    path: String,
    mode: Option<PlaylistPathMode>,
) -> Result<(), String> {
    let mut link = owner
        .shared
        .catalog
        .get_playlist_file_link(playlist)
        .await
        .map_err(string_error)?
        .unwrap_or(PlaylistFileLink {
            playlist,
            source_id: source_id.clone(),
            path: path.clone(),
            auto_refresh: true,
            auto_save: None,
            path_mode: PlaylistPathMode::Automatic,
            revision: None,
            dirty: false,
            error: None,
        });
    link.source_id = source_id;
    link.path = path;
    if let Some(mode) = mode {
        link.path_mode = mode;
    }
    link.revision = Some(revision(owner, &link).await?);
    link.dirty = false;
    link.error = None;
    owner
        .shared
        .catalog
        .save_playlist_file_link(&link)
        .await
        .map_err(string_error)?;
    associate(owner, &link).await?;
    Ok(())
}

async fn associate(owner: &SourceOwner, link: &PlaylistFileLink) -> Result<(), String> {
    if let Some(source) = &link.source_id
        && let Some(path) = owner
            .configuration(source)
            .and_then(|config| config.playlist_file_location(&link.path))
    {
        owner
            .shared
            .catalog
            .associate_playlist_files(source, &[(path, link.playlist)])
            .await
            .map_err(string_error)?;
    }
    Ok(())
}

async fn reload_on(
    owner: &SourceOwner,
    link: &PlaylistFileLink,
    discard: bool,
) -> Result<bool, String> {
    if link.dirty && !discard {
        return Err(LOCAL_CONFLICT.into());
    }
    if link.dirty {
        // The file replaces Rufin's changes, so an automatic save must not write them meanwhile.
        let mut clean = link.clone();
        clean.dirty = false;
        owner
            .shared
            .catalog
            .save_playlist_file_link(&clean)
            .await
            .map_err(string_error)?;
    }
    let before = revision(owner, link).await?;
    let report = if let Some(source) = &link.source_id {
        owner
            .client(source)?
            .import_playlist_file(&owner.shared.catalog, &link.path, Some(link.playlist))
            .await
            .map_err(string_error)?
    } else if link.path.starts_with("content:") {
        crate::playlists::import_document_on(owner, link.path.clone(), Some(link.playlist), None)
            .await?
    } else {
        let (_, _, access) = local_location(owner, Path::new(&link.path));
        sources::import_local_playlist_file(
            &owner.shared.catalog,
            &access,
            Some(link.playlist),
            |_| None,
        )
        .await
        .map_err(string_error)?
    };
    owner
        .shared
        .catalog
        .mark_playlist_file_synced(link.playlist, &before, 0)
        .await
        .map_err(string_error)?;
    crate::playlists::enrich_imported_playlist(owner, report.playlist).await?;
    Ok(report.changed)
}

async fn save_on(
    owner: &SourceOwner,
    link: &PlaylistFileLink,
    overwrite: bool,
) -> Result<(), String> {
    if !overwrite && link.revision.as_deref() != Some(revision(owner, link).await?.as_str()) {
        return Err(FILE_CONFLICT.into());
    }
    let catalog = &owner.shared.catalog;
    let edits = catalog.playlist_file_edits(link.playlist).await;
    let edits = edits.map_err(string_error)?;
    if let Some(path) = local_path(owner, link) {
        export_local_on(
            owner,
            &path,
            crate::runtime::source::PlaylistExport::Playlist(link.playlist),
            link.path_mode,
            if overwrite {
                None
            } else {
                link.revision.as_deref()
            },
        )
        .await?;
    } else if let Some(source) = &link.source_id {
        crate::playlists::export_source_playlist_on(
            owner,
            source,
            &link.path,
            crate::runtime::source::PlaylistExport::Playlist(link.playlist),
            link.path_mode,
            if overwrite {
                None
            } else {
                link.revision.as_deref()
            },
        )
        .await?;
    } else {
        use std::io::Write;
        let entry = document_entry(&link.path).await?;
        let format = library::PlaylistFormat::from_path(Path::new(&entry.name))
            .ok_or("Unsupported playlist format")?;
        let file = tempfile::NamedTempFile::new().map_err(string_error)?;
        let mut output = std::io::BufWriter::new(file.reopen().map_err(string_error)?);
        owner
            .shared
            .catalog
            .export_playlist_file_as(
                link.playlist,
                Path::new(&link.path),
                format,
                link.path_mode,
                &mut output,
            )
            .await
            .map_err(string_error)?;
        output.flush().map_err(string_error)?;
        drop(output);
        let uri = link.path.clone();
        let expected = if overwrite || entry.revision.is_none() {
            None
        } else {
            link.revision.clone()
        };
        tokio::task::spawn_blocking(move || {
            sources::save_document(&uri, file.path(), expected.as_deref())
        })
        .await
        .map_err(string_error)?
        .map_err(|error| match error {
            sources::SourceError::Server { status: 412, .. } => FILE_CONFLICT.into(),
            error => error.to_string(),
        })?;
    }
    let next = revision(owner, link).await?;
    catalog
        .mark_playlist_file_synced(link.playlist, &next, edits)
        .await
        .map_err(string_error)?;
    Ok(())
}

pub(crate) async fn export_local_on(
    owner: &SourceOwner,
    path: &Path,
    target: crate::runtime::source::PlaylistExport,
    mode: PlaylistPathMode,
    expected_revision: Option<&str>,
) -> Result<(), String> {
    let (_, _, access) = local_location(owner, path);
    let file = gio::File::for_path(&access);
    let etag = file
        .query_info(
            "etag::value",
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|info| info.attribute_string("etag::value"));
    if let Some(expected) = expected_revision
        && sources::playlist_file_revision(&access).map_err(string_error)? != expected
    {
        return Err(FILE_CONFLICT.into());
    }
    let temporary = crate::playlists::export_contents(owner, &access, target, mode).await?;
    let input = gio::File::for_path(&temporary)
        .read(gio::Cancellable::NONE)
        .map_err(string_error)?;
    let output = file
        .replace(
            etag.as_deref(),
            false,
            gio::FileCreateFlags::REPLACE_DESTINATION,
            gio::Cancellable::NONE,
        )
        .map_err(|error| {
            if error.matches(gio::IOErrorEnum::WrongEtag) {
                FILE_CONFLICT.into()
            } else {
                error.to_string()
            }
        })?;
    output
        .splice(
            &input,
            gio::OutputStreamSpliceFlags::CLOSE_SOURCE | gio::OutputStreamSpliceFlags::CLOSE_TARGET,
            gio::Cancellable::NONE,
        )
        .map_err(string_error)?;
    Ok(())
}

async fn after_edit(owner: &SourceOwner, playlist: PlaylistKey) {
    let Ok(Some(link)) = owner.shared.catalog.get_playlist_file_link(playlist).await else {
        return;
    };
    let enabled = match link.auto_save {
        Some(value) => value,
        None => owner
            .shared
            .catalog
            .playlist_file_auto_save(link.source_id.as_ref())
            .await
            .unwrap_or(false),
    };
    if enabled && link.dirty {
        let result = save_on(owner, &link, false).await;
        record_result(owner, &link, &result).await;
    }
}

async fn record_result(owner: &SourceOwner, link: &PlaylistFileLink, result: &Result<(), String>) {
    if let Err(error) = result {
        let _ = owner
            .shared
            .catalog
            .set_playlist_file_error(link.playlist, Some(error))
            .await;
        if link.error.as_ref() != Some(error) {
            owner.shared.warn_nonfatal(error);
        }
    }
}

pub(crate) async fn refresh_links(owner: &SourceOwner, source: Option<&SourceId>) {
    let mut after = PlaylistKey::from_raw(0);
    loop {
        let Ok(page) = owner
            .shared
            .catalog
            .playlist_file_link_page(after, 128)
            .await
        else {
            break;
        };
        if page.is_empty() {
            break;
        }
        after = page.last().unwrap().playlist;
        for link in page {
            if source.is_some() && link.source_id.as_ref() != source {
                continue;
            }
            let result = async {
                let current = revision(owner, &link).await?;
                if link.auto_refresh && link.revision.as_ref() != Some(&current) {
                    reload_on(owner, &link, false).await?;
                } else if !link.dirty && link.revision.as_ref() == Some(&current) {
                    if link.error.is_some() {
                        owner
                            .shared
                            .catalog
                            .set_playlist_file_error(link.playlist, None)
                            .await
                            .map_err(string_error)?;
                    }
                } else {
                    after_edit(owner, link.playlist).await;
                }
                Ok(())
            }
            .await;
            record_result(owner, &link, &result).await;
        }
    }
}

/// Imports the playlist files a file source holds that no playlist is made from yet, and
/// checks the linked ones. Runs after a refresh that stored, moved or removed a playlist file.
pub(crate) async fn discover(owner: &SourceOwner, source_id: &SourceId) {
    let Some(configuration) = owner
        .configuration(source_id)
        .filter(|source| source.is(library::SourceKind::file_library))
    else {
        return;
    };
    let Some(source_key) = owner.shared.catalog.source_key(source_id) else {
        return;
    };
    let mut after = None;
    loop {
        let Ok(page) = owner
            .shared
            .catalog
            .observed_playlist_file_page(source_key, after)
            .await
        else {
            break;
        };
        if page.is_empty() {
            break;
        }
        after = page.last().map(|file| file.local_file_key);
        for file in page {
            let Some(path) = configuration.playlist_file_path(&file.path) else {
                continue;
            };
            if owner
                .shared
                .catalog
                .is_playlist_file_ignored(Some(source_id), &path)
                .await
                .unwrap_or(false)
            {
                continue;
            }
            if let Ok(Some(link)) = owner
                .shared
                .catalog
                .find_playlist_file_link(Some(source_id), &path)
                .await
            {
                if file.playlist_identity.is_none() {
                    if let Err(error) = owner
                        .shared
                        .catalog
                        .associate_playlist_files(source_id, &[(file.path, link.playlist)])
                        .await
                    {
                        tracing::warn!(%path, %error, "could not associate playlist file");
                    }
                }
                continue;
            }
            if file.playlist_identity.is_some() && file.playlist.is_none() {
                continue;
            }
            let result = async {
                let playlist = match file.playlist {
                    Some(playlist) => playlist,
                    None => {
                        owner
                            .client(source_id)?
                            .import_playlist_file(&owner.shared.catalog, &path, None)
                            .await
                            .map_err(string_error)?
                            .playlist
                    }
                };
                remember(owner, playlist, Some(source_id.clone()), path.clone(), None).await?;
                Ok::<_, String>(())
            }
            .await;
            if let Err(error) = result {
                tracing::warn!(%path, %error, "could not import playlist file");
            }
        }
    }
    refresh_links(owner, Some(source_id)).await;
}
