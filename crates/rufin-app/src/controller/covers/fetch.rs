use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use rufin_core::ImageRef;
use rufin_provider::{ImageKind, ImageRequest, MusicProvider};
use rufin_secrets::SecretStore;
use rufin_store::{CoverCacheEntry, SavedServer, image_cache_key};
use tokio::runtime::Runtime;

use crate::controller::{
    IMAGE_TAG_UNTAGGED, StoreHandle, cover_cache_path_for_key, load_settings_from_store,
    provider_for_saved,
};
use crate::external_metadata;

pub(super) fn fetch_and_cache_cover(
    store: &StoreHandle,
    runtime: &Runtime,
    secrets: &Arc<dyn SecretStore>,
    saved: &SavedServer,
    image_ref: ImageRef,
    size: u32,
) -> Result<PathBuf, String> {
    if let Some(art) = external_metadata::album_art_from_image_ref(&image_ref) {
        let settings = load_settings_from_store(store);
        if !external_metadata::enabled(&settings) {
            return Err("external metadata lookup is disabled".to_string());
        }
        let bytes =
            external_metadata::fetch_album_cover(&art, size, settings.lastfm_api_key.trim())?;
        return save_cover_bytes(store, saved, image_ref, size, bytes);
    } else if external_metadata::is_external_artist_image_ref(&image_ref) {
        return Err("external artist image lookup is disabled".to_string());
    }
    let provider = provider_for_saved(store, runtime, secrets, saved)?;
    fetch_and_cache_provider_cover(
        store,
        runtime,
        saved,
        provider.as_music_provider(),
        image_ref,
        size,
    )
}

pub(super) fn fetch_and_cache_provider_cover(
    store: &StoreHandle,
    runtime: &Runtime,
    saved: &SavedServer,
    provider: &dyn MusicProvider,
    image_ref: ImageRef,
    size: u32,
) -> Result<PathBuf, String> {
    let image = runtime
        .block_on(provider.image_bytes(ImageRequest {
            item_id: image_ref.item_id.clone(),
            kind: ImageKind::Primary,
            tag: image_ref.tag.clone(),
            size,
        }))
        .map_err(|error| error.to_string())?;
    if image.bytes.is_empty() {
        return Err("cover response was empty".to_string());
    }
    save_cover_bytes(store, saved, image_ref, size, image.bytes)
}

fn save_cover_bytes(
    store: &StoreHandle,
    saved: &SavedServer,
    image_ref: ImageRef,
    size: u32,
    bytes: Vec<u8>,
) -> Result<PathBuf, String> {
    let tag = image_ref
        .tag
        .clone()
        .unwrap_or_else(|| IMAGE_TAG_UNTAGGED.to_string());
    let key = image_cache_key(&saved.server.id, &image_ref.item_id, &tag, size);
    let path = cover_cache_path_for_key(&key)
        .ok_or_else(|| "cache directory is unavailable".to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temp_path, &path).map_err(|error| error.to_string())?;

    store.with_store(|store| {
        store.save_cover_cache_entry(&CoverCacheEntry {
            server_id: saved.server.id.clone(),
            item_id: image_ref.item_id,
            image_tag: tag,
            size,
            path: path.to_string_lossy().to_string(),
        })
    })?;

    Ok(path)
}

pub(in crate::controller) fn is_provider_not_found_error(error: &str) -> bool {
    error == "provider item was not found"
}
