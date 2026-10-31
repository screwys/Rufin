//! Playlist routing and publication.
use crate::runtime::SourceEvent;
use crate::runtime::source::PlaylistExport;
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
use futures_util::{StreamExt, TryStreamExt};
use library::{Catalog, PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey};
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
                .import_local_paths(&owner.shared.catalog, paths, &name, &progress)
                .await;
            let playlist = result.as_ref().copied().map_err(string_error);
            if let Some(token) = token {
                owner
                    .finish_source_refresh(source.source_id(), &token, result.map(|_| ()))
                    .await;
            }
            playlist
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
        let result = async {
            let (source_id, location, access) =
                crate::playlist_files::local_location(&owner, &path);
            let target =
                crate::playlist_files::import_target(&owner, source_id.as_ref(), &location, linked)
                    .await?;
            let report = sources::import_local_playlist_file(
                &owner.shared.catalog,
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
        match result {
            Ok(report) => {
                let playlist = report.playlist;
                if let Err(error) = enrich_imported_playlist(&owner, playlist).await {
                    owner.shared.warn_nonfatal(&error);
                }
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
    let task = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task;
        let result = async {
            let path = crate::playlist_files::source_location(&owner, &source_id, &path)?;
            let target =
                crate::playlist_files::import_target(&owner, Some(&source_id), &path, linked)
                    .await?;
            let report = owner
                .client(&source_id)?
                .import_playlist_file(&owner.shared.catalog, &path, target)
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
            .catalog
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
                .catalog
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
        .catalog
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
            export_source_playlist_on(&owner, source, &location, target, mode, None).await?;
        } else {
            crate::playlist_files::export_local_on(&owner, &path, target, mode, None).await?;
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
    mode: library::PlaylistPathMode,
    expected_revision: Option<&str>,
) -> Result<(), String> {
    let source = owner.client(source_id)?;
    if let Some(access) = source.local_playlist_path(path) {
        return crate::playlist_files::export_local_on(
            owner,
            &access,
            target,
            mode,
            expected_revision,
        )
        .await;
    }
    let file = export_contents(owner, std::path::Path::new(path), target, mode).await?;
    source
        .save_playlist_file(&owner.shared.catalog, path, file, mode, expected_revision)
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
    mode: library::PlaylistPathMode,
) -> Result<tempfile::TempPath, String> {
    use std::io::Write;
    let file = tempfile::NamedTempFile::new().map_err(string_error)?;
    let mut output = std::io::BufWriter::new(file.reopen().map_err(string_error)?);
    match target {
        PlaylistExport::Playlist(key) => {
            owner
                .shared
                .catalog
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
                .catalog
                .export_smart_playlist_file(key, now, path, mode, &mut output)
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
    pub playlist_key: Option<PlaylistKey>,
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
        let catalog = &owner.shared.catalog;
        let pin_source = source_id.clone();
        let target = match source_id {
            None => PlaylistOwner::Local,
            Some(id) => {
                catalog.loaded().await;
                let cached = catalog
                    .cached_source(id.as_str())
                    .ok_or("Source access is unavailable")?;
                configured_playlist_owner(&owner, &id, cached.source)?
            }
        };
        let (playlist, object_id) = match target {
            PlaylistOwner::Local => {
                let Some((playlist, object)) = catalog
                    .create_playlist(&name, &songs)
                    .await
                    .map_err(string_error)?
                else {
                    return Ok(None);
                };
                (Some(playlist), object)
            }
            PlaylistOwner::Server(source, key) => {
                let tracks = catalog
                    .source_playlist_track_ids(key, None, &songs, false)
                    .await
                    .map_err(string_error)?
                    .into_iter()
                    .map(|(_, id)| id)
                    .collect::<Vec<_>>();
                let first = &tracks[..tracks.len().min(256)];
                let object = match &source.implementation {
                    // Subsonic answers with the new playlist, which is written from it.
                    Implementation::OpenSubsonic(provider) => provider
                        .create_playlist(
                            catalog,
                            source.source_id().as_str(),
                            &name,
                            &tracks,
                            public,
                        )
                        .await
                        .map_err(string_error)?,
                    _ => {
                        let object = match &source.implementation {
                            Implementation::Plex(provider) => {
                                provider.create_playlist(&name, first).await
                            }
                            Implementation::JellyfinEmby(provider) => {
                                provider.create_playlist(&name, first, public).await
                            }
                            _ => unreachable!(),
                        }
                        .map_err(string_error)?;
                        append_native_tracks(&source, &object, &tracks[first.len()..])
                            .await
                            .map_err(string_error)?;
                        refresh_playlist(&source, catalog, Some(object.clone()), None)
                            .await
                            .map_err(string_error)?;
                        object
                    }
                };
                let playlist = match catalog
                    .catalog_keys(
                        library::CatalogKind::Playlist,
                        Some(key),
                        std::slice::from_ref(&object),
                        &ReadCancellation::new(),
                    )
                    .await
                {
                    Ok(keys) => keys.first().copied().map(PlaylistKey::from_raw),
                    Err(error) => {
                        owner.shared.warn_nonfatal(&error.to_string());
                        None
                    }
                };
                (playlist, object)
            }
        };
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
            playlist_key: playlist,
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
    owner.reply(move |owner, _catalog| async move {
        let (target, id) = playlist_source(&owner, playlist, None).await?;
        match target {
            PlaylistOwner::Local => Ok(None),
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
    owner.reply(move |owner, catalog| async move {
        let row = catalog
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
    owner.reply(move |owner, catalog| async move {
        let bindings = catalog
            .random_playlist_artwork(playlist)
            .await
            .map_err(string_error)?;
        generate_artwork(&owner, bindings).await
    })
}

pub fn regenerate_smart_playlist_artwork(
    owner: &SourceOwner,
    key: library::SmartPlaylistKey,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, catalog| async move {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let bindings = catalog
            .random_smart_playlist_artwork(key, now)
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
                artwork::ArtworkRequest::new(artwork::ArtworkBinding::opaque(&binding), 512)
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
    let _edit = owner.edit_playlist(playlist).await;
    let (target, id) = playlist_source(
        owner,
        playlist,
        (name.is_some() || artwork.is_some()).then_some(false),
    )
    .await?;
    let native_source = match &target {
        PlaylistOwner::Local => None,
        PlaylistOwner::Server(source, _) => Some(source.source_id().clone()),
    };
    let previous = if artwork.is_some() {
        owner
            .shared
            .catalog
            .playlist_artwork_binding(playlist)
            .await
            .map_err(string_error)?
    } else {
        None
    };
    let result = match target {
        PlaylistOwner::Local => {
            let image = artwork.map(|change| match change {
                sources::ArtworkChange::Replace(image) => {
                    Some((image.bytes.as_slice(), image.content_type.as_deref()))
                }
                sources::ArtworkChange::Remove => None,
            });
            owner
                .shared
                .catalog
                .update_playlist(playlist, name, image)
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
                let refresh = refresh_playlist(&source, &owner.shared.catalog, Some(id), None)
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
                if native_source.is_some() {
                    owner.notice(crate::source::feed::Notice::Playlist(playlist));
                }
            }
            Ok(Ok(false)) => {}
            error => owner
                .shared
                .warn_nonfatal(&format!("Playlist artwork cache refresh failed: {error:?}")),
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
        move |target, id, catalog| async move {
            crate::playlist_files::forget_file(&operation_owner, playlist)
                .await
                .map_err(SourceError::Other)?;
            let result = match target {
                PlaylistOwner::Local => catalog
                    .delete_playlists(&[playlist])
                    .await
                    .map(|deleted| deleted > 0)
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
                    refresh_playlist(&source, &catalog, None, Some(id))
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
    let task = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task;
        let _edit = owner.edit_playlist(playlist).await;
        let target = playlist_source(&owner, playlist, Some(true)).await;
        let result = match target {
            Ok((PlaylistOwner::Local, _)) => owner
                .shared
                .catalog
                .add_playlist_tracks(playlist, &songs, skip_duplicates)
                .await
                .map_err(sources::SourceError::from),
            Ok((PlaylistOwner::Server(source, source_key), id)) => {
                async {
                    let skip =
                        skip_duplicates
                            || owner.configuration(source.source_id()).is_some_and(
                                |configuration| !configuration.playlist_tracks_can_repeat(),
                            );
                    let catalog = &owner.shared.catalog;
                    let (tracks, ids): (Vec<_>, Vec<_>) = catalog
                        .source_playlist_track_ids(source_key, Some(playlist), &songs, skip)
                        .await?
                        .into_iter()
                        .unzip();
                    append_native_tracks(&source, &id, &ids).await?;
                    let count = tracks.len();
                    if count > 0 {
                        if matches!(source.implementation, Implementation::OpenSubsonic(_)) {
                            // Subsonic names no entries, so the resident order is the server's.
                            catalog
                                .edit_source_playlist(
                                    playlist,
                                    library::SourceEntryEdit::Add { tracks },
                                )
                                .await?;
                        } else {
                            // The server names the new entries, so they are read back.
                            refresh_playlist(&source, catalog, Some(id), None).await?;
                        }
                    }
                    Ok(count)
                }
                .await
            }
            Err(error) => Err(sources::SourceError::Other(error)),
        };
        let reply = match result {
            Ok(accepted) => Ok(accepted),
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
        let _edit = owner.edit_playlist(playlist).await;
        let (target, id) = playlist_source(&owner, playlist, Some(true)).await?;
        let local = matches!(target, PlaylistOwner::Local);
        let catalog = &owner.shared.catalog;
        let entries = catalog
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
            PlaylistOwner::Local => {
                let keys = entries
                    .iter()
                    .map(|entry| entry.playlist_entry_key)
                    .collect::<Vec<_>>();
                if catalog
                    .remove_playlist_entries(playlist, &keys)
                    .await
                    .map_err(string_error)?
                    == 0
                {
                    return Ok(Vec::new());
                }
            }
            PlaylistOwner::Server(source, _) => {
                if let Implementation::OpenSubsonic(provider) = &source.implementation {
                    // Subsonic names entries by position, later ones first.
                    let positions = entries
                        .iter()
                        .rev()
                        .map(|entry| entry.position)
                        .collect::<Vec<_>>();
                    provider
                        .edit_playlist_entries(&id, &positions, &[])
                        .await
                        .map_err(string_error)?;
                }
                let ids = entries
                    .iter()
                    .map(|entry| entry.object_id.clone())
                    .collect::<Vec<_>>();
                for ids in ids.chunks(256) {
                    match &source.implementation {
                        Implementation::Plex(provider) => {
                            provider.remove_playlist_entries(&id, ids).await
                        }
                        Implementation::JellyfinEmby(provider) => {
                            provider.remove_playlist_entries(&id, ids).await
                        }
                        _ => Ok(()),
                    }
                    .map_err(string_error)?;
                }
                catalog
                    .edit_source_playlist(
                        playlist,
                        library::SourceEntryEdit::Remove {
                            entries: entries
                                .iter()
                                .map(|entry| entry.playlist_entry_key)
                                .collect(),
                        },
                    )
                    .await
                    .map_err(string_error)?;
            }
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
    let task = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task;
        let result = async {
            let (PlaylistOwner::Local, _) = playlist_source(&owner, playlist, Some(true)).await?
            else {
                return Err("This playlist belongs to a server".to_string());
            };
            let catalog = &owner.shared.catalog;
            let mut removals = Vec::new();
            let mut offset = 0;
            loop {
                let page = catalog
                    .playlist_entry_uri_page(playlist, offset)
                    .await
                    .map_err(string_error)?;
                if page.is_empty() {
                    break;
                }
                offset += page.len();
                let uris: Vec<_> = page.iter().map(|(_, uri)| uri.clone()).collect();
                let missing = owner.missing_media(&uris).await?;
                for (entry, uri) in page {
                    if missing.contains(&uri) {
                        removals.push(entry);
                    }
                }
            }
            let removed = catalog
                .remove_playlist_entries(playlist, &removals)
                .await
                .map_err(string_error)?;
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
        move |target, id, catalog| async move {
            match target {
                PlaylistOwner::Local => catalog
                    .move_playlist_entry(playlist, entry, position)
                    .await
                    .map_err(SourceError::from),
                PlaylistOwner::Server(source, _) => {
                    let entry = catalog
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
                            // Subsonic appends only: the tail from the lower index goes and
                            // comes back in its new order.
                            let low = entry.position.min(position);
                            let mut tail = catalog.playlist_track_ids_from(playlist, low).await?;
                            let moved = tail.remove(entry.position - low);
                            tail.insert((position - low).min(tail.len()), moved);
                            let remove = (low..low + tail.len()).rev().collect::<Vec<_>>();
                            provider.edit_playlist_entries(&id, &remove, &tail).await?
                        }
                        _ => unreachable!(),
                    }
                    catalog
                        .edit_source_playlist(
                            playlist,
                            library::SourceEntryEdit::Move {
                                entry: entry.playlist_entry_key,
                                position,
                            },
                        )
                        .await?;
                    Ok(true)
                }
            }
        },
    )
}

enum PlaylistOwner {
    Local,
    Server(Arc<Source>, SourceKey),
}

fn playlist_change<F, Work>(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: bool,
    change: F,
) -> Receiver<Result<bool, String>>
where
    F: FnOnce(PlaylistOwner, String, Arc<Catalog>) -> Work + Send + 'static,
    Work: Future<Output = Result<bool, SourceError>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    let task = owner.clone();
    owner.shared.runtime.spawn(async move {
        let owner = task;
        let _edit = owner.edit_playlist(playlist).await;
        let target = playlist_source(&owner, playlist, Some(edit_tracks)).await;
        let result = match target {
            Ok((target, id)) => change(target, id, Arc::clone(&owner.shared.catalog)).await,
            Err(error) => Err(SourceError::Other(error)),
        };
        if let Err(error) = &result {
            owner.shared.warn_nonfatal(&error.to_string());
        }
        let _ = sender.try_send(result.map_err(string_error));
    });
    receiver
}

/// Who owns a playlist, with the server's id of a server playlist.
async fn playlist_source(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: Option<bool>,
) -> Result<(PlaylistOwner, String), String> {
    let access = owner
        .shared
        .catalog
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
    if !access.source_owned {
        return Ok((PlaylistOwner::Local, String::new()));
    }
    let unavailable = "Playlist owner is unavailable";
    let key = access.source_key.ok_or(unavailable)?;
    let source = access.source_id.ok_or(unavailable)?;
    let id = access.object_id.ok_or(unavailable)?;
    Ok((
        PlaylistOwner::Server(owner.client(&SourceId::new(source))?, key),
        id,
    ))
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
        Ok(PlaylistOwner::Local)
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
        .catalog
        .playlist_import_page(playlist, "")
        .await
        .map_err(string_error)?
        .is_empty()
    {
        return Ok(());
    }
    owner
        .ensure_local_source()
        .await?
        .import_playlist_files(&owner.shared.catalog, playlist)
        .await
        .map_err(string_error)
}

pub fn smart_playlist_display_name(object_id: &str, name: &str) -> String {
    use localization::{msgid, tr};
    match object_id {
        "builtin:most_played" => tr(msgid("Most Played")),
        "builtin:never_played" => tr(msgid("Never Played")),
        "builtin:most_skipped" => tr(msgid("Most Skipped")),
        _ => name.to_owned(),
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn create_playlist_folder(
    owner: &SourceOwner,
    name: String,
    smart: bool,
) -> sources::SourceResult<library::PlaylistFolderKey> {
    owner
        .reply(move |_, catalog| async move {
            let key = catalog.create_playlist_folder(&name, smart).await?;
            Ok::<_, SourceError>(key)
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn rename_playlist_folder(
    owner: &SourceOwner,
    key: library::PlaylistFolderKey,
    name: String,
) -> sources::SourceResult<()> {
    owner
        .reply(move |_, catalog| async move {
            catalog.rename_playlist_folder(key, &name).await?;
            Ok::<_, SourceError>(())
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn delete_playlist_folder(
    owner: &SourceOwner,
    key: library::PlaylistFolderKey,
) -> sources::SourceResult<()> {
    owner
        .reply(move |owner, catalog| async move {
            let folder = catalog.delete_playlist_folders(&[key]).await?.pop();
            if let Some((folder_id, smart)) = folder {
                let settings = owner.shared.settings.clone();
                tokio::task::spawn_blocking(move || {
                    settings.update(|stored| {
                        stored.ui.sidebar.set_pinned(
                            crate::settings::SidebarPin::PlaylistFolder { folder_id, smart },
                            false,
                        );
                        Ok(())
                    })
                })
                .await
                .map_err(|error| SourceError::Other(error.to_string()))?
                .map_err(SourceError::Other)?;
            }
            Ok::<_, SourceError>(())
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub async fn move_playlist_to_folder(
    owner: &SourceOwner,
    member: library::PlaylistFolderMember,
    folder: Option<library::PlaylistFolderKey>,
) -> sources::SourceResult<()> {
    owner
        .reply(move |_, catalog| async move {
            catalog.move_playlist_to_folder(member, folder).await?;
            Ok::<_, SourceError>(())
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
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
}

/// Applies a Smart Playlist change. A created or updated playlist goes into `folder` when
/// one is given. Returns `None` when the playlist no longer exists.
pub async fn apply_smart_playlist_change(
    owner: &SourceOwner,
    change: SmartPlaylistChange,
    folder: Option<library::PlaylistFolderKey>,
) -> sources::SourceResult<Option<library::SmartPlaylistKey>> {
    owner
        .reply(move |_, catalog| async move {
            let applied = match change {
                SmartPlaylistChange::Create { name, definition } => Some(
                    catalog
                        .create_smart_playlist(&name, &definition, folder)
                        .await?,
                ),
                SmartPlaylistChange::Update {
                    key,
                    name,
                    definition,
                    artwork,
                } => {
                    let artwork = artwork.as_ref().map(|change| match change {
                        sources::ArtworkChange::Replace(image) => Some(image.bytes.as_slice()),
                        sources::ArtworkChange::Remove => None,
                    });
                    let updated = catalog
                        .update_smart_playlist(key, &name, &definition, artwork)
                        .await?;
                    if updated && folder.is_some() {
                        catalog
                            .move_playlist_to_folder(
                                library::PlaylistFolderMember::SmartPlaylist { key },
                                folder,
                            )
                            .await?;
                    }
                    updated.then_some(key)
                }
                SmartPlaylistChange::Delete(key) => {
                    (catalog.delete_smart_playlists(&[key]).await? > 0).then_some(key)
                }
            };
            Ok::<_, SourceError>(applied)
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
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
                provider
                    .edit_playlist_entries(playlist, &[], tracks)
                    .await?
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

async fn refresh_playlist(
    source: &Source,
    catalog: &Catalog,
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
        .apply_playlists(
            catalog,
            affected.into_iter().map(object).collect(),
            deleted.into_iter().map(object).collect(),
        )
        .await
}
