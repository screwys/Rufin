//! Playlist routing and publication.
use crate::runtime::source::PlaylistExport;
use crate::runtime::{CatalogChange, CatalogPublication, SourceEvent};
use crate::settings::{ConfiguredSource, fresh_source_id};
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
use library::{
    Database, FolderKey, PlaylistEntryKey, PlaylistKey, ReadCancellation, ScanOutcome, SourceKey,
};
use sources::{Source, SourceConfiguration, SourceId, SourceSetupInput};
use std::{
    future::Future,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
pub fn import_playlist(
    owner: &SourceOwner,
    path: PathBuf,
    current: Option<SourceConfiguration>,
    linked: bool,
) -> Receiver<Result<library::PlaylistImportReport, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = async {
            let (source_id, location, access) =
                crate::playlist_files::local_location(&owner, &path);
            let target =
                crate::playlist_files::import_target(&owner, source_id.as_ref(), &location, linked)
                    .await?;
            let report =
                crate::playlist_files::import_local_on(&owner, &access, target, current.as_ref())
                    .await?;
            if linked {
                crate::playlist_files::remember(&owner, report.playlist, source_id, location, None)
                    .await?;
            }
            accept_playlist_result(&owner, None, Some(report.playlist), Ok((true, None))).await;
            Ok(report)
        }
        .await;
        match result {
            Ok(report) => {
                let playlist = report.playlist;
                let _ = sender.send(Ok(report)).await;
                if let Err(error) = enrich_imported_playlist(&owner, playlist).await {
                    owner.shared.warn_nonfatal(&error);
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error)).await;
            }
        }
    });
    receiver
}

pub fn import_source_playlist(
    owner: &SourceOwner,
    source_id: SourceId,
    path: String,
    linked: bool,
) -> Receiver<Result<library::PlaylistImportReport, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = async {
            let path = crate::playlist_files::source_location(&owner, &source_id, &path)?;
            let target =
                crate::playlist_files::import_target(&owner, Some(&source_id), &path, linked)
                    .await?;
            let report =
                crate::playlist_files::import_source_on(&owner, &source_id, &path, target).await?;
            if linked {
                crate::playlist_files::remember(
                    &owner,
                    report.playlist,
                    Some(source_id),
                    path,
                    None,
                )
                .await?;
            }
            accept_playlist_result(&owner, None, Some(report.playlist), Ok((true, None))).await;
            Ok(report)
        }
        .await;
        let _ = sender.send(result).await;
    });
    receiver
}

pub fn export_playlist(
    owner: &SourceOwner,
    source_id: Option<SourceId>,
    path: PathBuf,
    target: PlaylistExport,
    scope: Option<(SourceKey, Option<FolderKey>)>,
    mode: library::PlaylistPathMode,
    linked: bool,
) -> Receiver<Result<(), String>> {
    crate::playlist_files::run(owner, move |owner| async move {
        let key = match &target {
            PlaylistExport::Playlist(key) => Some(*key),
            _ => None,
        };
        let (source, location) = if let Some(source) = &source_id {
            let path = path.to_str().ok_or("Source paths must be UTF-8")?;
            (
                Some(source.clone()),
                crate::playlist_files::source_location(&owner, source, path)?,
            )
        } else {
            let (source, location, _) = crate::playlist_files::local_location(&owner, &path);
            (source, location)
        };
        if linked && let Some(key) = key {
            crate::playlist_files::check_link(&owner, key, source.as_ref(), &location).await?;
        }
        if let Some(source) = &source_id {
            export_source_playlist_on(&owner, source, &location, target, scope, mode, None).await?;
        } else {
            crate::playlist_files::export_local_on(&owner, &path, target, scope, mode, None)
                .await?;
        }
        if linked && let Some(key) = key {
            crate::playlist_files::remember(&owner, key, source, location, Some(mode)).await?;
        }
        Ok(())
    })
}

pub(crate) async fn export_source_playlist_on(
    owner: &SourceOwner,
    source_id: &SourceId,
    path: &str,
    target: PlaylistExport,
    scope: Option<(SourceKey, Option<FolderKey>)>,
    mode: library::PlaylistPathMode,
    expected_revision: Option<&str>,
) -> Result<(), String> {
    let source = owner.client(source_id)?;
    if let Some(access) = source.local_playlist_path(path) {
        return crate::playlist_files::export_local_on(
            owner,
            &access,
            target,
            scope,
            mode,
            expected_revision,
        )
        .await;
    }
    let file = export_contents(owner, std::path::Path::new(path), target, scope, mode).await?;
    source
        .save_playlist_file(&owner.shared.database, path, file, mode, expected_revision)
        .await
        .map_err(|error| match error {
            sources::SourceError::Server { status: 412, .. } => {
                crate::playlist_files::FILE_CONFLICT.to_string()
            }
            error => error.to_string(),
        })
}

pub(crate) async fn export_contents(
    owner: &SourceOwner,
    path: &std::path::Path,
    target: PlaylistExport,
    scope: Option<(SourceKey, Option<FolderKey>)>,
    mode: library::PlaylistPathMode,
) -> Result<tempfile::TempPath, String> {
    use std::io::Write;
    let file = tempfile::NamedTempFile::new().map_err(string_error)?;
    let mut output = std::io::BufWriter::new(file.reopen().map_err(string_error)?);
    match target {
        PlaylistExport::Playlist(key) => {
            owner
                .shared
                .database
                .export_playlist_file(key, path, mode, &mut output)
                .await
        }
        PlaylistExport::Smart(key) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            owner
                .shared
                .database
                .export_smart_playlist_file(
                    key,
                    scope.map(|s| s.0),
                    scope.and_then(|s| s.1),
                    now,
                    path,
                    mode,
                    &mut output,
                )
                .await
        }
    }
    .map_err(string_error)?;
    output.flush().map_err(string_error)?;
    drop(output);
    Ok(file.into_temp_path())
}

pub fn create_playlist(
    owner: &SourceOwner,
    source_id: Option<SourceId>,
    name: String,
    media_uris: Vec<String>,
    public: Option<bool>,
) -> Receiver<Result<Option<String>, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let target = async {
            match source_id {
                None => Ok(PlaylistOwner::Local(None)),
                Some(id) => {
                    let cached = owner
                        .shared
                        .database
                        .cached_source(id.as_str(), &ReadCancellation::new())
                        .await
                        .map_err(string_error)?
                        .ok_or_else(|| "Source access is unavailable".to_string())?;
                    configured_playlist_owner(&owner, &id, cached.source)
                }
            }
        }
        .await;
        let source_key = target.as_ref().ok().and_then(PlaylistOwner::source_key);
        let result = match target {
            Ok(PlaylistOwner::Local(key)) => owner
                .shared
                .database
                .create_playlist(key, &name, &media_uris)
                .await
                .map(|playlist| {
                    let (playlist, object_id) = playlist.unzip();
                    (playlist.is_some(), None, playlist, object_id)
                })
                .map_err(string_error),
            Ok(PlaylistOwner::Server(source, key)) => {
                async {
                    let (changed, outcome, object_id) = source
                        .create_playlist(&owner.shared.database, key, &name, &media_uris, public)
                        .await
                        .map_err(string_error)?;
                    let playlist = if let Some(id) = object_id.as_deref() {
                        match owner
                            .shared
                            .database
                            .playlist_key_by_object(key, id, &ReadCancellation::new())
                            .await
                        {
                            Ok(playlist) => playlist,
                            Err(error) => {
                                owner.shared.warn_nonfatal(&error.to_string());
                                None
                            }
                        }
                    } else {
                        None
                    };
                    Ok((changed, outcome, playlist, object_id))
                }
                .await
            }
            Err(error) => Err(error),
        };
        let reply = match result {
            Ok((changed, outcome, playlist, object_id)) => {
                accept_playlist_result(&owner, source_key, playlist, Ok((changed, outcome))).await;
                Ok(object_id)
            }
            Err(error) => {
                owner.shared.warn_nonfatal(&error);
                Err(error)
            }
        };
        let _ = sender.send(reply).await;
    });
    receiver
}

pub fn playlist_public(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<Option<bool>, String>> {
    owner.reply(move |owner, database| async move {
        match playlist_source(&owner, playlist).await? {
            PlaylistOwner::Local(_) => Ok(None),
            PlaylistOwner::Server(source, key) => source
                .playlist_public(&database, key, playlist)
                .await
                .map_err(string_error),
        }
    })
}

pub fn update_playlist(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: Option<String>,
    public: Option<bool>,
) -> Receiver<Result<bool, String>> {
    crate::playlist_files::run(owner, move |owner| async move {
        update_playlist_on(&owner, playlist, name.as_deref(), public).await
    })
}

pub(crate) async fn update_playlist_on(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: Option<&str>,
    public: Option<bool>,
) -> Result<bool, String> {
    let target = playlist_source(owner, playlist).await?;
    let source_key = target.source_key();
    let result = match target {
        PlaylistOwner::Local(source) => {
            if let Some(name) = name {
                owner
                    .shared
                    .database
                    .rename_playlist(source, playlist, name)
                    .await
                    .map(|changed| (changed, None))
                    .map_err(string_error)
            } else {
                Ok((false, None))
            }
        }
        PlaylistOwner::Server(source, key) => source
            .update_playlist(&owner.shared.database, key, playlist, name, public)
            .await
            .map_err(string_error),
    };
    let reply = result
        .as_ref()
        .map(|(changed, _)| *changed || public.is_some())
        .map_err(Clone::clone);
    accept_playlist_result(owner, source_key, Some(playlist), result).await;
    reply
}

pub fn delete_playlist(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<bool, String>> {
    let operation_owner = owner.clone();
    playlist_change(&owner, playlist, move |target, database| async move {
        crate::playlist_files::forget_file(&operation_owner, playlist).await?;
        let result = match target {
            PlaylistOwner::Local(source_key) => database
                .delete_playlist(source_key, playlist)
                .await
                .map(|changed| (changed, None))
                .map_err(string_error),
            PlaylistOwner::Server(source, source_key) => source
                .delete_playlist(&database, source_key, playlist)
                .await
                .map_err(string_error),
        };
        if matches!(result, Ok((true, _))) {
            prune_imported_playlist_files(&operation_owner).await;
        }
        result
    })
}

pub fn add_playlist_tracks(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    media_uris: Vec<String>,
    skip_duplicates: bool,
) -> Receiver<Result<usize, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let target = playlist_source(&owner, playlist).await;
        let source_key = target.as_ref().ok().and_then(PlaylistOwner::source_key);
        let result = match target {
            Ok(PlaylistOwner::Local(source_key)) => owner
                .shared
                .database
                .add_playlist_media(source_key, playlist, &media_uris, skip_duplicates)
                .await
                .map(|accepted| (accepted, None))
                .map_err(sources::SourceError::from),
            Ok(PlaylistOwner::Server(source, source_key)) => {
                source
                    .add_playlist_tracks(
                        &owner.shared.database,
                        source_key,
                        playlist,
                        &media_uris,
                        skip_duplicates,
                    )
                    .await
            }
            Err(error) => Err(sources::SourceError::Other(error)),
        };
        let reply = match result {
            Ok((accepted, outcome)) => {
                accept_playlist_result(
                    &owner,
                    source_key,
                    Some(playlist),
                    Ok((accepted > 0, outcome)),
                )
                .await;
                Ok(accepted)
            }
            Err(error) => {
                if let sources::SourceError::Library(
                    library::LibraryError::PlaylistSourceMismatch(source_id),
                ) = &error
                {
                    owner
                        .shared
                        .send(SourceEvent::PlaylistSourceMismatch(source_id.clone()))
                        .await;
                } else {
                    owner.shared.warn_nonfatal(&error.to_string());
                }
                Err(error.to_string())
            }
        };
        let _ = sender.send(reply).await;
    });
    receiver
}

pub fn remove_playlist_entries(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    entries: Vec<PlaylistEntryKey>,
) -> Receiver<Result<bool, String>> {
    let operation_owner = owner.clone();
    playlist_change(&owner, playlist, move |target, database| async move {
        let result = match target {
            PlaylistOwner::Local(source_key) => database
                .remove_playlist_entries(source_key, playlist, &entries)
                .await
                .map(|removed| (removed > 0, None))
                .map_err(string_error),
            PlaylistOwner::Server(source, source_key) => source
                .remove_playlist_entries(&database, source_key, playlist, &entries)
                .await
                .map_err(string_error),
        };
        if matches!(result, Ok((true, _))) {
            prune_imported_playlist_files(&operation_owner).await;
        }
        result
    })
}

pub fn move_playlist_entry(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    entry: PlaylistEntryKey,
    position: usize,
) -> Receiver<Result<bool, String>> {
    playlist_change(&owner, playlist, move |target, database| async move {
        match target {
            PlaylistOwner::Local(source_key) => database
                .move_playlist_entry(source_key, playlist, entry, position)
                .await
                .map(|changed| (changed, None))
                .map_err(string_error),
            PlaylistOwner::Server(source, source_key) => source
                .move_playlist_entry(&database, source_key, playlist, entry, position)
                .await
                .map_err(string_error),
        }
    })
}

enum PlaylistOwner {
    Local(Option<SourceKey>),
    Server(Arc<Source>, SourceKey),
}

impl PlaylistOwner {
    fn source_key(&self) -> Option<SourceKey> {
        match self {
            Self::Local(key) => *key,
            Self::Server(_, key) => Some(*key),
        }
    }
}

fn playlist_change<F, Work>(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    change: F,
) -> Receiver<Result<bool, String>>
where
    F: FnOnce(PlaylistOwner, Arc<Database>) -> Work + Send + 'static,
    Work: Future<Output = Result<(bool, Option<ScanOutcome>), String>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let target = playlist_source(&owner, playlist).await;
        let source_key = target.as_ref().ok().and_then(PlaylistOwner::source_key);
        let result = match target {
            Ok(target) => change(target, Arc::clone(&owner.shared.database)).await,
            Err(error) => Err(error),
        };
        let reply = result
            .as_ref()
            .map(|(changed, _)| *changed)
            .map_err(Clone::clone);
        accept_playlist_result(&owner, source_key, Some(playlist), result).await;
        let _ = sender.try_send(reply);
    });
    receiver
}

async fn playlist_source(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Result<PlaylistOwner, String> {
    match owner
        .shared
        .database
        .playlist_owner(playlist, &ReadCancellation::new())
        .await
        .map_err(string_error)?
    {
        Some((None, None)) => Ok(PlaylistOwner::Local(None)),
        Some((Some(key), Some(id))) => configured_playlist_owner(owner, &SourceId::new(id), key),
        Some(_) => Err("Playlist owner is unavailable".to_string()),
        None => Err("Playlist no longer exists".to_string()),
    }
}

fn configured_playlist_owner(
    owner: &SourceOwner,
    id: &SourceId,
    key: SourceKey,
) -> Result<PlaylistOwner, String> {
    if owner
        .configuration(id)
        .is_some_and(|configuration| configuration.is_file_library())
    {
        Ok(PlaylistOwner::Local(Some(key)))
    } else {
        owner
            .client(id)
            .map(|source| PlaylistOwner::Server(source, key))
    }
}

async fn enrich_imported_playlist(
    owner: &SourceOwner,
    playlist: library::PlaylistKey,
) -> Result<(), String> {
    let mut after = -1;
    let mut readable = false;
    loop {
        let page = owner
            .shared
            .database
            .playlist_file_uri_page(playlist, after)
            .await
            .map_err(string_error)?;
        if page.is_empty() {
            break;
        }
        after = page.last().unwrap().0;
        if page
            .iter()
            .any(|(_, uri)| library::file_media_path(uri).is_some_and(|path| path.is_file()))
        {
            readable = true;
            break;
        }
    }
    if readable {
        let stored = owner.shared.settings.load();
        let local = stored
            .sources
            .configured
            .iter()
            .find(|item| item.configuration.is_local());
        let source = if let Some(local) = local {
            owner.client(&local.configuration.source_id)?
        } else {
            let connected = Source::connect(
                fresh_source_id()?,
                SourceSetupInput::Local(sources::LocalFolderHostInput { roots: Vec::new() }),
            )
            .await
            .map_err(string_error)?;
            let (configuration, source, credential) = connected.into_parts();
            owner.persist_connected_source(
                &ConfiguredSource {
                    configuration: configuration.clone(),
                    credential_ref: None,
                    music_folder_id: None,
                    local_access: None,
                    enable_half_stars: false,
                },
                credential,
            )?;
            library::Scan::begin(
                &owner.shared.database,
                configuration.source_id.as_str(),
                &configuration.name,
                "local",
                None,
            )
            .await
            .map_err(string_error)?
            .finish()
            .await
            .map_err(string_error)?;
            owner
                .shared
                .send(SourceEvent::Configured(
                    owner
                        .shared
                        .configured_sources(owner.shared.selected().as_deref()),
                ))
                .await;
            Arc::new(source)
        };
        let outcome = source
            .import_playlist_files(&owner.shared.database, playlist)
            .await
            .map_err(string_error)?;
        owner
            .accept_scan(source.source_id(), outcome, CatalogChange::Broad)
            .await;
    }
    Ok(())
}

pub(crate) async fn prune_imported_playlist_files(owner: &SourceOwner) {
    if let Some(local) = owner
        .shared
        .settings
        .load()
        .sources
        .configured
        .iter()
        .find(|item| item.configuration.is_local())
    {
        if let Ok(client) = owner.client(&local.configuration.source_id) {
            match client.prune_imported_files(&owner.shared.database).await {
                Ok(Some(outcome)) => {
                    if let Some(selected) = owner
                        .shared
                        .selected()
                        .filter(|selected| selected.source_id() == client.source_id())
                    {
                        owner
                            .accept_scan(selected.source_id(), outcome, CatalogChange::Broad)
                            .await;
                    }
                }
                Err(error) => owner.shared.warn_nonfatal(&error.to_string()),
                Ok(None) => {}
            }
        }
    }
}

async fn accept_playlist_result(
    owner: &SourceOwner,
    source: Option<SourceKey>,
    playlist: Option<PlaylistKey>,
    result: Result<(bool, Option<ScanOutcome>), String>,
) {
    let outcome = match result {
        Ok((true, outcome)) => outcome,
        Ok((false, _)) => return,
        Err(error) => {
            owner.shared.warn_nonfatal(&error);
            return;
        }
    };
    if let Some(playlist) = playlist {
        crate::playlist_files::after_edit(owner, playlist).await;
    }
    if let Some(outcome) = outcome
        && let Some(selected) = owner
            .shared
            .selected()
            .filter(|selected| Some(selected.source_key) == source)
    {
        owner
            .accept_scan(
                selected.source_id(),
                outcome,
                CatalogChange::Playlists(playlist),
            )
            .await;
    } else {
        owner
            .shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: source,
                favorite: None,
                change: CatalogChange::Playlists(playlist),
            }))
            .await;
    }
}
pub fn playlist_artwork_bindings(
    playlist: &library::PlaylistRow,
    prefer_server: bool,
) -> &[Vec<u8>] {
    if prefer_server {
        playlist
            .artwork_binding
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or(&playlist.representative_artwork)
    } else if playlist.representative_artwork.is_empty() {
        playlist
            .artwork_binding
            .as_ref()
            .map(std::slice::from_ref)
            .unwrap_or_default()
    } else {
        &playlist.representative_artwork
    }
}

pub fn smart_playlist_display_name(playlist: &library::SmartPlaylistRow) -> String {
    use localization::{msgid, tr};
    match playlist.object_id.as_str() {
        "builtin:most_played" => tr(msgid("Most Played")),
        "builtin:never_played" => tr(msgid("Never Played")),
        "builtin:most_skipped" => tr(msgid("Most Skipped")),
        _ => playlist.name.clone(),
    }
}
