//! File association and synchronization for authored playlists.
use std::{
    future::Future,
    io::BufReader,
    path::{Path, PathBuf},
};

use async_channel::Receiver;
use gio::prelude::*;
use library::{PlaylistFileLink, PlaylistKey, PlaylistPathMode, ReadCancellation, SourceId};

use crate::{
    runtime::{CatalogChange, CatalogPublication, SourceEvent},
    source::{SourceOwner, string_error},
};

const LOCAL_CONFLICT: &str =
    "Both the playlist and its file have changed. Reload the file or save the Rufin version.";
pub(crate) const FILE_CONFLICT: &str =
    "The playlist file has changed. Reload the file or replace it with the Rufin version.";

pub fn is_conflict(error: &str) -> bool {
    matches!(error, LOCAL_CONFLICT | FILE_CONFLICT)
}

#[derive(Clone, Debug, serde::Serialize)]
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
    owner.spawn_serialized(move |owner| async move {
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
    owner.reply(move |owner, database| async move {
        let link = database
            .get_playlist_file_link(playlist)
            .await
            .map_err(string_error)?;
        let display_path = link.as_ref().map(|link| {
            local_path(&owner, link)
                .map(|path| {
                    library::playlist_host_path(&path)
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_else(|| link.path.clone())
        });
        let source_auto_save = database
            .playlist_file_auto_save(link.as_ref().and_then(|link| link.source_id.as_ref()))
            .await
            .map_err(string_error)?;
        let can_link = database
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
    owner.reply(move |_, database| async move {
        database
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
            .database
            .set_playlist_file_auto_save(Some(&source_id), enabled)
            .await
            .map_err(string_error)?;
        if enabled {
            refresh_links(&owner, Some(&source_id)).await;
        }
        Ok(())
    })
}

pub fn update(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: String,
    auto_refresh: bool,
    auto_save: Option<bool>,
    path_mode: PlaylistPathMode,
) -> Receiver<Result<(), String>> {
    run(owner, move |owner| async move {
        let mut settings_changed = false;
        if let Some(mut link) = owner
            .shared
            .database
            .get_playlist_file_link(playlist)
            .await
            .map_err(string_error)?
        {
            link.auto_refresh = auto_refresh;
            link.auto_save = auto_save;
            link.path_mode = path_mode;
            settings_changed = owner
                .shared
                .database
                .save_playlist_file_link(&link)
                .await
                .map_err(string_error)?;
        }
        if !crate::playlists::rename_playlist_on(&owner, playlist, &name).await? {
            after_edit(&owner, playlist).await;
            if settings_changed {
                publish(&owner, Some(playlist)).await;
            }
        }
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
        .database
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
        .database
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
                .database
                .save_playlist_file_link(&previous)
                .await
                .map_err(string_error)?;
        } else {
            owner
                .shared
                .database
                .unlink_playlist_file(playlist)
                .await
                .map_err(string_error)?;
        }
        return Err(error);
    }
    publish(owner, Some(playlist)).await;
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
        .database
        .playlist_file_linkable(playlist)
        .await
        .map_err(string_error)?
    {
        return Err("Export a copy of this provider playlist before linking a file".into());
    }
    if library::PlaylistFormat::from_path(Path::new(path)).is_none() {
        return Err("Unsupported playlist format".into());
    }
    if owner
        .shared
        .database
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
        .database
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
            .database
            .unlink_playlist_file(playlist)
            .await
            .map_err(string_error)?;
        publish(&owner, Some(playlist)).await;
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
        publish(&owner, Some(playlist)).await;
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
        publish(&owner, Some(playlist)).await;
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
        } else {
            std::fs::remove_file(&link.path).map_err(string_error)?;
        }
        forget_file(&owner, playlist).await?;
        let source = owner
            .shared
            .database
            .playlist_owner(playlist, &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .and_then(|(source, _)| source);
        owner
            .shared
            .database
            .delete_playlist(source, playlist)
            .await
            .map_err(string_error)?;
        crate::playlists::prune_imported_playlist_files(&owner).await;
        publish(&owner, Some(playlist)).await;
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
        if library::PlaylistFormat::from_path(Path::new(&name))
            != library::PlaylistFormat::from_path(Path::new(&link.path))
        {
            return Err("Use Export to change the playlist format".into());
        }
        let previous = link.path.clone();
        let next = if local_path(&owner, &link).is_some() {
            Path::new(&link.path)
                .with_file_name(&name)
                .to_string_lossy()
                .into_owned()
        } else {
            link.path
                .rsplit_once('/')
                .map_or_else(|| name.clone(), |(parent, _)| format!("{parent}/{name}"))
        };
        if previous == next {
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
            .database
            .ignore_playlist_file(link.source_id.as_ref(), &previous)
            .await
            .map_err(string_error)?;
        link.path = next;
        link.revision = Some(revision(&owner, &link).await?);
        owner
            .shared
            .database
            .save_playlist_file_link(&link)
            .await
            .map_err(string_error)?;
        publish(&owner, Some(playlist)).await;
        Ok(())
    })
}

async fn required_link(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Result<PlaylistFileLink, String> {
    owner
        .shared
        .database
        .get_playlist_file_link(playlist)
        .await
        .map_err(string_error)?
        .ok_or_else(|| "Playlist is not linked to a file".into())
}

pub(crate) async fn forget_file(owner: &SourceOwner, playlist: PlaylistKey) -> Result<(), String> {
    if let Some(link) = owner
        .shared
        .database
        .get_playlist_file_link(playlist)
        .await
        .map_err(string_error)?
    {
        owner
            .shared
            .database
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
        if let Some((location, access)) = configured.configuration.local_playlist_location(path) {
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
        .and_then(|path| owner.configuration(source)?.local_playlist_location(&path))
        .map_or_else(|| path.to_owned(), |(location, _)| location))
}

fn local_path(owner: &SourceOwner, link: &PlaylistFileLink) -> Option<PathBuf> {
    match &link.source_id {
        None => Some(PathBuf::from(&link.path)),
        Some(source) => owner.client(source).ok()?.local_playlist_path(&link.path),
    }
}

async fn revision(owner: &SourceOwner, link: &PlaylistFileLink) -> Result<String, String> {
    if let Some(path) = local_path(owner, link) {
        return tokio::task::spawn_blocking(move || sources::playlist_file_revision(&path))
            .await
            .map_err(string_error)?
            .map_err(string_error);
    }
    match &link.source_id {
        None => sources::playlist_file_revision(Path::new(&link.path)).map_err(string_error),
        Some(source) => owner
            .client(source)?
            .playlist_file_revision(&link.path)
            .await
            .map_err(string_error),
    }
}

pub(crate) async fn import_source_on(
    owner: &SourceOwner,
    source: &SourceId,
    path: &str,
    target: Option<PlaylistKey>,
) -> Result<library::PlaylistImportReport, String> {
    let source = owner.client(source)?;
    if let Some(path) = source.local_playlist_path(path) {
        import_local_on(owner, &path, target, None).await
    } else {
        source
            .import_playlist_file(&owner.shared.database, path, target)
            .await
            .map_err(string_error)
    }
}

pub(crate) async fn import_local_on(
    owner: &SourceOwner,
    path: &Path,
    target: Option<PlaylistKey>,
    current: Option<&sources::SourceConfiguration>,
) -> Result<library::PlaylistImportReport, String> {
    let (_, _, access) = local_location(owner, path);
    let (document, access, rejected) = tokio::task::spawn_blocking(move || {
        let mut document = library::PlaylistFile::read(
            BufReader::new(std::fs::File::open(&access).map_err(string_error)?),
            &access,
        )
        .map_err(string_error)?;
        if target.is_none() {
            document.identity = None;
        }
        let before = document.entries.len();
        document.entries.retain(|entry| {
            library::playlist_locator(&entry.locator, access.parent().unwrap_or(Path::new(".")))
                .and_then(|uri| library::file_media_path(&uri))
                .is_none_or(|path| !sources::playlist_file_is_non_audio(&path))
        });
        let rejected = before - document.entries.len();
        Ok::<_, String>((document, access, rejected))
    })
    .await
    .map_err(string_error)??;
    let mut report = owner
        .shared
        .database
        .import_playlist_document(document, &access, target, |locator| {
            current.and_then(|source| source.recognize_media_locator(locator))
        })
        .await
        .map_err(string_error)?;
    report.skipped += rejected as u64;
    Ok(report)
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
        .database
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
        .database
        .save_playlist_file_link(&link)
        .await
        .map_err(string_error)?;
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
    let before = revision(owner, link).await?;
    let report = if let Some(source) = &link.source_id {
        import_source_on(owner, source, &link.path, Some(link.playlist)).await?
    } else {
        import_local_on(owner, Path::new(&link.path), Some(link.playlist), None).await?
    };
    owner
        .shared
        .database
        .mark_playlist_file_synced(link.playlist, &before)
        .await
        .map_err(string_error)?;
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
    if let Some(path) = local_path(owner, link) {
        export_local_on(
            owner,
            &path,
            crate::runtime::source::PlaylistExport::Playlist(link.playlist),
            None,
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
            None,
            link.path_mode,
            if overwrite {
                None
            } else {
                link.revision.as_deref()
            },
        )
        .await?;
    }
    let next = revision(owner, link).await?;
    owner
        .shared
        .database
        .mark_playlist_file_synced(link.playlist, &next)
        .await
        .map_err(string_error)
}

pub(crate) async fn export_local_on(
    owner: &SourceOwner,
    path: &Path,
    target: crate::runtime::source::PlaylistExport,
    scope: Option<(library::SourceKey, Option<library::FolderKey>)>,
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
    let temporary = crate::playlists::export_contents(owner, &access, target, scope, mode).await?;
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

pub(crate) async fn after_edit(owner: &SourceOwner, playlist: PlaylistKey) {
    let Ok(Some(link)) = owner.shared.database.get_playlist_file_link(playlist).await else {
        return;
    };
    let enabled = match link.auto_save {
        Some(value) => value,
        None => owner
            .shared
            .database
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
            .database
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
            .database
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
                    if reload_on(owner, &link, false).await? {
                        publish(owner, Some(link.playlist)).await;
                    }
                } else if !link.dirty && link.revision.as_ref() == Some(&current) {
                    if link.error.is_some() {
                        owner
                            .shared
                            .database
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

pub(crate) async fn discover(
    owner: &SourceOwner,
    source_id: &SourceId,
    source_key: library::SourceKey,
) {
    let Some(configuration) = owner
        .configuration(source_id)
        .filter(|source| source.is_file_library())
    else {
        return;
    };
    let mut after = None;
    loop {
        let Ok(page) = owner
            .shared
            .database
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
            let path = if configuration.is_local() {
                let Some((path, _)) = configuration.local_playlist_location(Path::new(&file.path))
                else {
                    continue;
                };
                path
            } else {
                file.relative_path
            };
            if owner
                .shared
                .database
                .is_playlist_file_ignored(Some(source_id), &path)
                .await
                .unwrap_or(false)
            {
                continue;
            }
            if owner
                .shared
                .database
                .find_playlist_file_link(Some(source_id), &path)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                continue;
            }
            let result = async {
                let report = import_source_on(owner, source_id, &path, None).await?;
                remember(
                    owner,
                    report.playlist,
                    Some(source_id.clone()),
                    path.clone(),
                    None,
                )
                .await?;
                publish(owner, Some(report.playlist)).await;
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

async fn publish(owner: &SourceOwner, playlist: Option<PlaylistKey>) {
    owner
        .shared
        .send(SourceEvent::CatalogPublished(CatalogPublication {
            source_key: None,
            favorite: None,
            change: CatalogChange::Playlists(playlist),
        }))
        .await;
}
