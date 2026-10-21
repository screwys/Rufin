//! Playlist routing and publication.
use crate::runtime::source::PlaylistExport;
use crate::runtime::{CatalogChange, CatalogPublication, SourceEvent};
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
use futures_util::{StreamExt, TryStreamExt};
use library::{Database, FolderKey, PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey};
use sources::{Implementation, Source, SourceConfiguration, SourceError, SourceId};
use std::{
    future::Future,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
pub fn import_local_paths(
    owner: &SourceOwner,
    paths: Vec<PathBuf>,
    name: String,
) -> Receiver<Result<PlaylistKey, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    let task_owner = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task_owner;
        let result = async {
            let source = owner.ensure_local_source().await?;
            let token = owner.shared.begin_refresh(source.source_id(), false, true);
            if token.is_some() {
                owner.shared.present_refresh(source.source_id());
            }
            let progress = |value| {
                if let Some(token) = &token {
                    owner
                        .shared
                        .refresh_progress(source.source_id(), token, value);
                }
            };
            let result = source
                .import_local_paths(&owner.shared.database, paths, &name, &progress)
                .await;
            let playlist = result.as_ref().copied().map_err(string_error);
            if let Some(token) = token {
                owner
                    .finish_source_refresh(source.source_id(), &token, result.map(|_| ()))
                    .await;
            }
            let playlist = playlist?;
            crate::playlist_files::after_edit(&owner, playlist).await;
            local_playlist_changed(&owner, None, Some(playlist)).await;
            Ok(playlist)
        }
        .await;
        let _ = sender.send(result).await;
    });
    receiver
}

pub fn import_playlist(
    owner: &SourceOwner,
    path: PathBuf,
    current: Option<SourceConfiguration>,
    linked: bool,
) -> Receiver<Result<library::PlaylistImportReport, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    let task_owner = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task_owner;
        let lane = owner.shared.lane.lock().await;
        let result = async {
            let (source_id, location, access) =
                crate::playlist_files::local_location(&owner, &path);
            let target =
                crate::playlist_files::import_target(&owner, source_id.as_ref(), &location, linked)
                    .await?;
            let report = sources::import_local_playlist_file(
                &owner.shared.database,
                &access,
                target,
                |locator| {
                    current
                        .as_ref()
                        .and_then(|source| source.recognize_media_locator(locator))
                },
            )
            .await
            .map_err(string_error)?;
            if linked {
                crate::playlist_files::remember(&owner, report.playlist, source_id, location, None)
                    .await?;
            }
            Ok(report)
        }
        .await;
        drop(lane);
        match result {
            Ok(report) => {
                let playlist = report.playlist;
                if let Err(error) = enrich_imported_playlist(&owner, playlist).await {
                    owner.shared.warn_nonfatal(&error);
                }
                crate::playlist_files::after_edit(&owner, playlist).await;
                local_playlist_changed(&owner, None, Some(playlist)).await;
                let _ = sender.send(Ok(report)).await;
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
            let report = owner
                .client(&source_id)?
                .import_playlist_file(&owner.shared.database, &path, target)
                .await
                .map_err(string_error)?;
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
            enrich_imported_playlist(&owner, report.playlist).await?;
            crate::playlist_files::after_edit(&owner, report.playlist).await;
            local_playlist_changed(&owner, None, Some(report.playlist)).await;
            Ok(report)
        }
        .await;
        let _ = sender.send(result).await;
    });
    receiver
}

pub fn import_document_playlist(
    owner: &SourceOwner,
    uri: String,
    current: Option<SourceConfiguration>,
) -> Receiver<Result<library::PlaylistImportReport, String>> {
    crate::playlist_files::run(owner, move |owner| async move {
        let report = import_document_on(&owner, uri, None, current).await?;
        enrich_imported_playlist(&owner, report.playlist).await?;
        crate::playlist_files::after_edit(&owner, report.playlist).await;
        local_playlist_changed(&owner, None, Some(report.playlist)).await;
        Ok(report)
    })
}

pub(crate) async fn import_document_on(
    owner: &SourceOwner,
    uri: String,
    target: Option<PlaylistKey>,
    current: Option<SourceConfiguration>,
) -> Result<library::PlaylistImportReport, String> {
    let roots = match owner
        .shared
        .settings
        .local_configuration()
        .as_ref()
        .and_then(|configuration| configuration.editable().ok())
    {
        Some(sources::EditableSource::Local { document_roots, .. }) => document_roots,
        _ => Vec::new(),
    };
    let (mut document, file, locators) = tokio::task::spawn_blocking(move || {
        let file = sources::stat_document(&uri).map_err(string_error)?.name;
        let input = sources::open_document_input(&uri).map_err(string_error)?;
        let format = library::PlaylistFormat::from_path(std::path::Path::new(&file))
            .ok_or("Unsupported playlist format")?;
        let mut document = library::PlaylistFile::read_as(
            std::io::BufReader::new(input),
            std::path::Path::new(&uri),
            format,
        )
        .map_err(string_error)?;
        if target.is_none() {
            document.identity = None;
        }
        let mut locators = Vec::new();
        for entry in &mut document.entries {
            if url::Url::parse(&entry.locator).is_err()
                && !std::path::Path::new(&entry.locator).is_absolute()
                && !entry.locator.starts_with("\\\\")
            {
                entry.locator = sources::resolve_document_relative(&uri, &entry.locator, &roots)
                    .map_err(string_error)?
                    .uri;
            }
            if entry.locator.starts_with("content:") {
                let identity = sources::document_identity(&entry.locator).map_err(string_error)?;
                let media_uri = library::document_media_uri(&identity);
                locators.push(library::LocalLocatorWrite {
                    source_id: None,
                    media_uri: media_uri.clone(),
                    origin: "import".into(),
                    path: entry.locator.clone(),
                    root: entry.locator.clone(),
                    relative_path: sources::stat_document(&entry.locator)
                        .map_err(string_error)?
                        .name,
                    access_uri: entry.locator.clone(),
                });
                entry.locator = media_uri;
            }
        }
        Ok::<_, String>((document, file, locators))
    })
    .await
    .map_err(string_error)??;
    let mut rekeys = std::collections::HashMap::new();
    for page in locators.chunks(128) {
        let ids = page
            .iter()
            .map(|locator| {
                library::document_media_id(&locator.media_uri).expect("parsed document identity")
            })
            .collect::<Vec<_>>();
        let canonical = owner
            .shared
            .database
            .media_uris_for_document_ids(&ids)
            .await
            .map_err(string_error)?;
        for (mut locator, canonical) in page.iter().cloned().zip(canonical) {
            if let Some(canonical) = canonical {
                rekeys.insert(locator.media_uri.clone(), canonical.clone());
                locator.media_uri = canonical;
            }
            owner
                .shared
                .database
                .upsert_local_locator(&locator)
                .await
                .map_err(string_error)?;
        }
    }
    for entry in &mut document.entries {
        if let Some(canonical) = rekeys.get(&entry.locator) {
            entry.locator = canonical.clone();
        }
    }
    let before = document.entries.len();
    document.entries.retain(|entry| {
        library::playlist_locator(&entry.locator, std::path::Path::new("."))
            .and_then(|uri| library::file_media_path(&uri))
            .is_none_or(|path| !sources::playlist_file_is_non_audio(&path))
    });
    document.skipped += (before - document.entries.len()) as u64;
    let report = owner
        .shared
        .database
        .import_playlist_document(document, std::path::Path::new(&file), target, |locator| {
            current
                .as_ref()
                .and_then(|source| source.recognize_media_locator(locator))
        })
        .await
        .map_err(string_error)?;
    Ok(report)
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

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistCreation {
    pub object_id: String,
    pub settings_error: Option<String>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn create_playlist(
    owner: &SourceOwner,
    source_id: Option<SourceId>,
    name: String,
    songs: Arc<library::MemberSelection>,
    public: Option<bool>,
    current: Option<bool>,
) -> sources::SourceResult<Option<PlaylistCreation>> {
    crate::playlist_files::run(owner, move |owner| async move {
        let database = &owner.shared.database;
        let pin_source = source_id.clone();
        let target = match source_id {
            None => PlaylistOwner::Local(None),
            Some(id) => {
                let cached = database
                    .cached_source(id.as_str(), &ReadCancellation::new())
                    .await
                    .map_err(string_error)?
                    .ok_or("Source access is unavailable")?;
                configured_playlist_owner(&owner, &id, cached.source)?
            }
        };
        let source_key = target.source_key();
        let local = matches!(target, PlaylistOwner::Local(_));
        let (playlist, object_id) = match target {
            PlaylistOwner::Local(key) => {
                let Some((playlist, object)) = database
                    .create_playlist(key, &name, &songs)
                    .await
                    .map_err(string_error)?
                else {
                    return Ok(None);
                };
                (Some(playlist), object)
            }
            PlaylistOwner::Server(source, key) => {
                let tracks = database
                    .source_playlist_track_ids(key, None, &songs, false)
                    .await
                    .map_err(string_error)?;
                let first = &tracks[..tracks.len().min(256)];
                let object = match &source.implementation {
                    Implementation::Plex(provider) => provider.create_playlist(&name, first).await,
                    Implementation::JellyfinEmby(provider) => {
                        provider.create_playlist(&name, first, public).await
                    }
                    Implementation::OpenSubsonic(provider) => {
                        provider.create_playlist(&name, first, public).await
                    }
                    _ => unreachable!(),
                }
                .map_err(string_error)?;
                append_native_tracks(&source, &object, &tracks[first.len()..])
                    .await
                    .map_err(string_error)?;
                refresh_playlist(&source, database, Some(object.clone()), None)
                    .await
                    .map_err(string_error)?;
                let playlist = match database
                    .playlist_key_by_object(key, &object, &ReadCancellation::new())
                    .await
                {
                    Ok(playlist) => playlist,
                    Err(error) => {
                        owner.shared.warn_nonfatal(&error.to_string());
                        None
                    }
                };
                (playlist, object)
            }
        };
        if let Some(playlist) = playlist {
            crate::playlist_files::after_edit(&owner, playlist).await;
        }
        if local {
            local_playlist_changed(&owner, source_key, playlist).await;
        }
        let playlist_id = object_id.clone();
        let settings = owner.shared.settings.clone();
        let settings_error = tokio::task::spawn_blocking(move || {
            settings.update(|stored| {
                stored.ui.sidebar.set_pinned(
                    crate::settings::SidebarPin::Playlist {
                        source_id: pin_source,
                        playlist_id,
                    },
                    true,
                );
                if let Some(current) = current {
                    stored.ui.application.new_playlist_current = current;
                }
                Ok(())
            })
        })
        .await
        .map_err(string_error)
        .and_then(|result| result)
        .err();
        Ok(Some(PlaylistCreation {
            object_id,
            settings_error,
        }))
    })
    .recv()
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
    .map_err(SourceError::Other)
}

pub fn playlist_public(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<Option<bool>, String>> {
    owner.reply(move |owner, _database| async move {
        let (target, id) = playlist_source(&owner, playlist, None).await?;
        match target {
            PlaylistOwner::Local(_) => Ok(None),
            PlaylistOwner::Server(source, _) => match &source.implementation {
                Implementation::JellyfinEmby(provider) => provider.playlist_public(&id).await,
                Implementation::OpenSubsonic(provider) => provider.playlist_public(&id).await,
                _ => Ok(None),
            }
            .map_err(string_error),
        }
    })
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PlaylistArtworkEditing {
    pub binding: Option<Vec<u8>>,
    pub automatic_binding: Option<Vec<u8>>,
    pub writable: bool,
}

pub fn playlist_artwork_editing(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<PlaylistArtworkEditing, String>> {
    owner.reply(move |owner, database| async move {
        let row = database
            .playlists(&[playlist], &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .pop()
            .ok_or("Playlist no longer exists")?;
        let writable = row.metadata_writable
            && (!row.source_owned
                || row
                    .source_id
                    .as_ref()
                    .map(|id| owner.client(&SourceId::new(id)))
                    .transpose()?
                    .is_some_and(|source| source.supports_playlist_artwork()));
        let binding = row
            .artwork_binding
            .as_deref()
            .map(library::authored_playlist_artwork)
            .transpose()
            .map_err(string_error)?
            .flatten();
        let automatic_binding = row
            .artwork_binding
            .as_deref()
            .map(library::automatic_playlist_artwork)
            .transpose()
            .map_err(string_error)?
            .flatten();
        Ok(PlaylistArtworkEditing {
            binding,
            automatic_binding,
            writable,
        })
    })
}

pub fn regenerate_playlist_artwork(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, database| async move {
        let bindings = database
            .random_playlist_artwork(playlist)
            .await
            .map_err(string_error)?;
        generate_artwork(&owner, bindings).await
    })
}

pub fn regenerate_smart_playlist_artwork(
    owner: &SourceOwner,
    key: library::SmartPlaylistKey,
    source: Option<library::SourceKey>,
    folder: Option<library::FolderKey>,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, database| async move {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let bindings = database
            .random_smart_playlist_artwork(key, source, folder, now)
            .await
            .map_err(string_error)?;
        generate_artwork(&owner, bindings).await
    })
}

async fn generate_artwork(
    owner: &SourceOwner,
    bindings: Vec<Vec<u8>>,
) -> Result<Option<Arc<sources::ImageBytes>>, String> {
    let stored = owner.shared.settings.load();
    let policy = artwork::ExternalPolicy::new(
        stored.ui.application.external_metadata_enabled,
        stored.ui.allows_external_metadata_lookup(),
        stored.ui.lastfm_api_key,
    );
    let mut images = futures_util::stream::iter(bindings)
        .map(|binding| {
            let request =
                artwork::ArtworkRequest::new(artwork::ArtworkBinding::opaque(&binding), 512, 512)
                    .with_external(policy.clone());
            owner.shared.artwork.image_bytes(request)
        })
        .buffered(4)
        .try_filter_map(|bytes| async { Ok(bytes) })
        .try_collect::<Vec<_>>()
        .await?;
    if images.is_empty() {
        return Ok(None);
    }
    if images.len() == 1 {
        let bytes = images.pop().unwrap();
        return Ok(Some(Arc::new(sources::ImageBytes {
            content_type: artwork::image_mime(&bytes).map(str::to_owned),
            bytes,
        })));
    }
    let bytes = tokio::task::spawn_blocking(move || artwork::collage_png(&images, 1024))
        .await
        .map_err(string_error)?
        .map_err(string_error)?;
    Ok(Some(Arc::new(sources::ImageBytes {
        bytes,
        content_type: Some("image/png".into()),
    })))
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn update_playlist(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: Option<String>,
    public: Option<bool>,
    artwork: Option<sources::ArtworkChange>,
) -> sources::SourceResult<bool> {
    crate::playlist_files::run(owner, move |owner| async move {
        update_playlist_on(&owner, playlist, name.as_deref(), public, artwork.as_ref()).await
    })
    .recv()
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
    .map_err(SourceError::Other)
}

pub(crate) async fn update_playlist_on(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: Option<&str>,
    public: Option<bool>,
    artwork: Option<&sources::ArtworkChange>,
) -> Result<bool, String> {
    let (target, id) = playlist_source(
        owner,
        playlist,
        (name.is_some() || artwork.is_some()).then_some(false),
    )
    .await?;
    let source_key = target.source_key();
    let native_source = match &target {
        PlaylistOwner::Local(_) => None,
        PlaylistOwner::Server(source, _) => Some(source.source_id().clone()),
    };
    let previous = if artwork.is_some() {
        owner
            .shared
            .database
            .playlist_artwork_binding(playlist)
            .await
            .map_err(string_error)?
    } else {
        None
    };
    let result = match target {
        PlaylistOwner::Local(source) => {
            let image = artwork.map(|change| match change {
                sources::ArtworkChange::Replace(image) => {
                    Some((image.bytes.as_slice(), image.content_type.as_deref()))
                }
                sources::ArtworkChange::Remove => None,
            });
            owner
                .shared
                .database
                .update_playlist(source, playlist, name, image)
                .await
                .map_err(|error| sources::SourceMetadataError::Write(error.to_string()))
        }
        PlaylistOwner::Server(source, _) => {
            async {
                let error =
                    |error: SourceError| sources::SourceMetadataError::Write(error.to_string());
                if artwork.is_some() && !source.supports_playlist_artwork() {
                    return Err(sources::SourceMetadataError::Unavailable);
                }
                if name.is_some() || public.is_some() {
                    match &source.implementation {
                        Implementation::Plex(provider) => {
                            if let Some(name) = name {
                                provider.rename_playlist(&id, name).await.map_err(error)?;
                            }
                        }
                        Implementation::JellyfinEmby(provider) => provider
                            .update_playlist(&id, name, public)
                            .await
                            .map_err(error)?,
                        Implementation::OpenSubsonic(provider) => provider
                            .update_playlist(&id, name, public)
                            .await
                            .map_err(error)?,
                        _ => unreachable!(),
                    }
                }
                let result = if let Some(artwork) = artwork {
                    match &source.implementation {
                        Implementation::Plex(provider) => {
                            provider.change_artwork(&id, artwork).await
                        }
                        Implementation::JellyfinEmby(provider) => {
                            provider.change_artwork(&id, artwork).await
                        }
                        Implementation::OpenSubsonic(provider) => {
                            provider.change_playlist_artwork(&id, artwork).await
                        }
                        _ => unreachable!(),
                    }
                    .map_err(error)
                } else {
                    Ok(())
                };
                if name.is_none() && artwork.is_none() {
                    return result.map(|()| false);
                }
                let refresh = refresh_playlist(&source, &owner.shared.database, Some(id), None)
                    .await
                    .map_err(|error| {
                        sources::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    });
                sources::finish_metadata_save(result, refresh, name.is_some() || public.is_some())
                    .map(|()| true)
            }
            .await
        }
    };
    let changed = match &result {
        Ok(changed) => *changed,
        Err(sources::SourceMetadataError::PartiallySaved { .. })
        | Err(sources::SourceMetadataError::SavedRefreshFailed(_)) => true,
        Err(error) => {
            owner.shared.warn_nonfatal(&error.to_string());
            false
        }
    };
    if changed && let Some(binding) = previous {
        let cache = owner.shared.artwork.clone();
        match tokio::task::spawn_blocking(move || {
            cache.invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
        })
        .await
        {
            Ok(Ok(true)) => {
                if let Some(source) = &native_source {
                    owner
                        .artwork_invalidated(source, CatalogChange::Playlists(Some(playlist)))
                        .await;
                }
            }
            Ok(Ok(false)) => {}
            error => owner
                .shared
                .warn_nonfatal(&format!("Playlist artwork cache refresh failed: {error:?}")),
        }
    }
    if changed {
        crate::playlist_files::after_edit(owner, playlist).await;
        if native_source.is_none() {
            local_playlist_changed(owner, source_key, Some(playlist)).await;
        }
    }
    result
        .map(|changed| changed || public.is_some())
        .map_err(string_error)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn delete_playlist(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> sources::SourceResult<bool> {
    let operation_owner = owner.clone();
    playlist_change(
        &owner,
        playlist,
        false,
        move |target, id, database| async move {
            crate::playlist_files::forget_file(&operation_owner, playlist)
                .await
                .map_err(SourceError::Other)?;
            let result = match target {
                PlaylistOwner::Local(source_key) => database
                    .delete_playlist(source_key, playlist)
                    .await
                    .map_err(SourceError::from),
                PlaylistOwner::Server(source, _) => {
                    match &source.implementation {
                        Implementation::Plex(provider) => provider.delete_playlist(&id).await?,
                        Implementation::JellyfinEmby(provider) => {
                            provider.delete_playlist(&id).await?
                        }
                        Implementation::OpenSubsonic(provider) => {
                            provider.delete_playlist(&id).await?
                        }
                        _ => unreachable!(),
                    }
                    refresh_playlist(&source, &database, None, Some(id))
                        .await
                        .map(|()| true)
                }
            };
            result
        },
    )
    .recv()
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
    .map_err(SourceError::Other)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn add_playlist_tracks(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    songs: Arc<library::MemberSelection>,
    skip_duplicates: bool,
) -> sources::SourceResult<usize> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let target = playlist_source(&owner, playlist, Some(true)).await;
        let source_key = target
            .as_ref()
            .ok()
            .and_then(|(target, _)| target.source_key());
        let local = matches!(&target, Ok((PlaylistOwner::Local(_), _)));
        let result = match target {
            Ok((PlaylistOwner::Local(source_key), _)) => owner
                .shared
                .database
                .add_playlist_tracks(source_key, playlist, &songs, skip_duplicates)
                .await
                .map_err(sources::SourceError::from),
            Ok((PlaylistOwner::Server(source, source_key), id)) => {
                async {
                    let skip =
                        skip_duplicates
                            || owner.configuration(source.source_id()).is_some_and(
                                |configuration| !configuration.playlist_tracks_can_repeat(),
                            );
                    let tracks = owner
                        .shared
                        .database
                        .source_playlist_track_ids(source_key, Some(playlist), &songs, skip)
                        .await?;
                    append_native_tracks(&source, &id, &tracks).await?;
                    if !tracks.is_empty() {
                        refresh_playlist(&source, &owner.shared.database, Some(id), None).await?;
                    }
                    Ok(tracks.len())
                }
                .await
            }
            Err(error) => Err(sources::SourceError::Other(error)),
        };
        let reply = match result {
            Ok(accepted) => {
                if accepted > 0 {
                    crate::playlist_files::after_edit(&owner, playlist).await;
                    if local {
                        local_playlist_changed(&owner, source_key, Some(playlist)).await;
                    }
                }
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
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
        .map_err(SourceError::Other)
}

pub fn remove_playlist_entries(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    selection: library::MemberSelection,
) -> Receiver<Result<Vec<library::TrackKey>, String>> {
    crate::playlist_files::run(owner, move |owner| async move {
        let (target, id) = playlist_source(&owner, playlist, Some(true)).await?;
        let source_key = target.source_key();
        let local = matches!(target, PlaylistOwner::Local(_));
        let database = &owner.shared.database;
        let entries = database
            .playlist_edit_entries(playlist, &selection, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        if !local
            && let library::MemberSelection::Entries(requested) = &selection
            && entries.len() != requested.len()
        {
            return Err("Playlist entry is no longer current".into());
        }
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let songs = entries
            .iter()
            .map(|entry| entry.track_key)
            .collect::<Vec<_>>();
        match target {
            PlaylistOwner::Local(source) => {
                let keys = entries
                    .iter()
                    .map(|entry| entry.playlist_entry_key)
                    .collect::<Vec<_>>();
                if database
                    .remove_playlist_entries(source, playlist, &keys)
                    .await
                    .map_err(string_error)?
                    == 0
                {
                    return Ok(Vec::new());
                }
            }
            PlaylistOwner::Server(source, _) => {
                // Later positions go first because Subsonic identifies entries by position.
                let ids = entries
                    .into_iter()
                    .rev()
                    .map(|entry| entry.object_id)
                    .collect::<Vec<_>>();
                for ids in ids.chunks(256) {
                    match &source.implementation {
                        Implementation::Plex(provider) => {
                            provider.remove_playlist_entries(&id, ids).await
                        }
                        Implementation::JellyfinEmby(provider) => {
                            provider.remove_playlist_entries(&id, ids).await
                        }
                        Implementation::OpenSubsonic(provider) => {
                            provider.remove_playlist_entries(&id, ids).await
                        }
                        _ => unreachable!(),
                    }
                    .map_err(string_error)?;
                }
                refresh_playlist(&source, database, Some(id), None)
                    .await
                    .map_err(string_error)?;
            }
        }
        crate::playlist_files::after_edit(&owner, playlist).await;
        if local {
            local_playlist_changed(&owner, source_key, Some(playlist)).await;
        }
        Ok(songs)
    })
}

pub fn can_remove_unavailable(owner: &SourceOwner, playlist: &library::PlaylistRow) -> bool {
    !playlist.source_owned
        && playlist.writable
        && playlist.source_id.as_ref().is_none_or(|id| {
            owner
                .configuration(&SourceId::new(id))
                .is_some_and(|source| source.is_file_library())
        })
}

pub fn remove_unavailable_tracks(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<usize, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = async {
            let (PlaylistOwner::Local(source), _) =
                playlist_source(&owner, playlist, Some(true)).await?
            else {
                return Err("This playlist belongs to a server".to_string());
            };
            let database = &owner.shared.database;
            let mut removals = library::PlaylistEntryRemovals::new().map_err(string_error)?;
            let mut after = -1;
            loop {
                let page = database
                    .playlist_entry_uri_page(playlist, after)
                    .await
                    .map_err(string_error)?;
                let Some(last) = page.last() else { break };
                after = last.1;
                let uris: Vec<_> = page.iter().map(|(_, _, uri)| uri.clone()).collect();
                let missing = owner.missing_media(&uris).await?;
                for (entry, _, uri) in page {
                    if missing.contains(&uri) {
                        removals.push(entry).map_err(string_error)?;
                    }
                }
            }
            let removed = removals
                .apply(database, source, playlist)
                .await
                .map_err(string_error)?;
            if removed > 0 {
                crate::playlist_files::after_edit(&owner, playlist).await;
                local_playlist_changed(&owner, source, Some(playlist)).await;
            }
            Ok(removed)
        }
        .await;
        if let Err(error) = &result {
            owner.shared.warn_nonfatal(error);
        }
        let _ = sender.send(result).await;
    });
    receiver
}

pub fn move_playlist_entry(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    entry: PlaylistEntryKey,
    position: usize,
) -> Receiver<Result<bool, String>> {
    playlist_change(
        &owner,
        playlist,
        true,
        move |target, id, database| async move {
            match target {
                PlaylistOwner::Local(source_key) => database
                    .move_playlist_entry(source_key, playlist, entry, position)
                    .await
                    .map_err(SourceError::from),
                PlaylistOwner::Server(source, _) => {
                    let entry = database
                        .playlist_edit_entries(
                            playlist,
                            &library::MemberSelection::Entries(vec![entry].into()),
                            &ReadCancellation::new(),
                        )
                        .await?
                        .pop()
                        .ok_or(SourceError::NotFound)?;
                    if entry.position == position {
                        return Ok(false);
                    }
                    match &source.implementation {
                        Implementation::Plex(provider) => {
                            provider
                                .move_playlist_entry(
                                    &id,
                                    &entry.object_id,
                                    entry.position,
                                    position,
                                )
                                .await?
                        }
                        Implementation::JellyfinEmby(provider) => {
                            provider
                                .move_playlist_entry(&id, &entry.object_id, position)
                                .await?
                        }
                        Implementation::OpenSubsonic(provider) => {
                            provider
                                .move_playlist_entry(&id, &entry.object_id, position)
                                .await?
                        }
                        _ => unreachable!(),
                    }
                    refresh_playlist(&source, &database, Some(id), None)
                        .await
                        .map(|()| true)
                }
            }
        },
    )
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
    edit_tracks: bool,
    change: F,
) -> Receiver<Result<bool, String>>
where
    F: FnOnce(PlaylistOwner, String, Arc<Database>) -> Work + Send + 'static,
    Work: Future<Output = Result<bool, SourceError>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let target = playlist_source(&owner, playlist, Some(edit_tracks)).await;
        let source_key = target
            .as_ref()
            .ok()
            .and_then(|(target, _)| target.source_key());
        let local = matches!(&target, Ok((PlaylistOwner::Local(_), _)));
        let result = match target {
            Ok((target, id)) => change(target, id, Arc::clone(&owner.shared.database)).await,
            Err(error) => Err(SourceError::Other(error)),
        };
        match &result {
            Ok(true) => {
                crate::playlist_files::after_edit(&owner, playlist).await;
                if local {
                    local_playlist_changed(&owner, source_key, Some(playlist)).await;
                }
            }
            Err(error) => owner.shared.warn_nonfatal(&error.to_string()),
            Ok(false) => {}
        }
        let _ = sender.try_send(result.map_err(string_error));
    });
    receiver
}

async fn playlist_source(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: Option<bool>,
) -> Result<(PlaylistOwner, String), String> {
    let access = owner
        .shared
        .database
        .playlist_access(playlist, &ReadCancellation::new())
        .await
        .map_err(string_error)?
        .ok_or("Playlist no longer exists")?;
    if access.source_owned
        && edit_tracks.is_some_and(|tracks| {
            if tracks {
                !access.writable
            } else {
                !access.metadata_writable
            }
        })
    {
        return Err("The server does not allow this playlist change".into());
    }
    let target = if access.source_owned {
        let key = access.source_key.ok_or("Playlist owner is unavailable")?;
        let id = access.source_id.ok_or("Playlist owner is unavailable")?;
        PlaylistOwner::Server(owner.client(&SourceId::new(id))?, key)
    } else {
        PlaylistOwner::Local(access.source_key)
    };
    Ok((target, access.object_id))
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

pub(crate) async fn enrich_imported_playlist(
    owner: &SourceOwner,
    playlist: library::PlaylistKey,
) -> Result<(), String> {
    if owner
        .shared
        .database
        .imported_local_uri_page(Some(playlist), "")
        .await
        .map_err(string_error)?
        .is_empty()
    {
        return Ok(());
    }
    owner
        .ensure_local_source()
        .await?
        .import_playlist_files(&owner.shared.database, playlist)
        .await
        .map_err(string_error)
}

async fn local_playlist_changed(
    owner: &SourceOwner,
    source: Option<SourceKey>,
    playlist: Option<PlaylistKey>,
) {
    if let Err(error) = owner
        .shared
        .database
        .refresh_incomplete_playlist_artwork(playlist, false)
        .await
    {
        owner.shared.warn_nonfatal(&error.to_string());
    }
    owner
        .shared
        .send(SourceEvent::CatalogPublished(CatalogPublication {
            source_key: source,
            favorite: None,
            change: CatalogChange::Playlists(playlist),
        }))
        .await;
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

#[derive(Clone)]
pub enum SmartPlaylistChange {
    Create {
        name: String,
        definition: library::SmartPlaylistDefinition,
    },
    Update {
        key: library::SmartPlaylistKey,
        name: String,
        definition: library::SmartPlaylistDefinition,
        artwork: Option<sources::ArtworkChange>,
    },
    Delete(library::SmartPlaylistKey),
    Move {
        dragged: library::SmartPlaylistKey,
        target: library::SmartPlaylistKey,
    },
}

async fn append_native_tracks(
    source: &Source,
    playlist: &str,
    tracks: &[String],
) -> Result<(), SourceError> {
    for tracks in tracks.chunks(256) {
        match &source.implementation {
            Implementation::Plex(provider) => {
                provider.add_playlist_tracks(playlist, tracks).await?
            }
            Implementation::JellyfinEmby(provider) => {
                provider.add_playlist_tracks(playlist, tracks).await?
            }
            Implementation::OpenSubsonic(provider) => {
                provider.add_playlist_tracks(playlist, tracks).await?
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

async fn refresh_playlist(
    source: &Source,
    database: &Database,
    affected: Option<String>,
    deleted: Option<String>,
) -> Result<(), SourceError> {
    let object = |id: String| {
        let raw = id.rsplit(':').next().unwrap_or(&id);
        if matches!(source.implementation, Implementation::OpenSubsonic(_)) {
            format!("playlist:{raw}")
        } else {
            raw.to_owned()
        }
    };
    source
        .apply_items(
            database,
            affected.into_iter().map(object).collect(),
            deleted.into_iter().map(object).collect(),
        )
        .await
}
