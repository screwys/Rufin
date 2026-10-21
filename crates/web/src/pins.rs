use super::*;
use library::ReadCancellation;
use rufin_core::settings::SidebarPin;

fn artwork_revision<'a>(bindings: impl IntoIterator<Item = &'a Vec<u8>>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for binding in bindings {
        artwork::ArtworkBinding::opaque(binding)
            .stable_identity()
            .hash(&mut hash);
    }
    format!("{:x}", hash.finish())
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new().route("/api/pins", get(list).post(set).patch(reorder))
}

async fn list(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let settings = products.settings.load();
    let sidebar = settings.sidebar;
    let database = &products.library;
    let cancellation = ReadCancellation::new();
    let selected = products.source.selected_library();
    let configured = products.source.list_sources();
    let source = selected.as_ref().map(|s| s.source_key);
    let folder = selected.as_ref().and_then(|s| s.music_folder_key);
    let mut items = Vec::new();
    for pin in &sidebar.pins {
        let row = match pin {
            SidebarPin::Album {
                album_id,
                source_id,
            } => {
                let uri = library::source_entity_uri(source_id, "album", album_id);
                let row = database
                    .album(&uri, false, &cancellation)
                    .await
                    .map_err(internal)?;
                row.map(|row| json!({"kind":"album","id":row.album_key,"object_id":row.object_id,"source":source_id,"title":row.metadata.title,"uri":row.media_uri,"favorite":row.state.favorite,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.iter()),"artwork_previews":artwork_previews(&products, row.artwork_binding.iter())}))
            }
            SidebarPin::Artist {
                artist_id,
                source_id,
                album_artist,
            } => {
                let uri = library::source_entity_uri(source_id, "artist", artist_id);
                let row = database
                    .artist(&uri, *album_artist, false, &cancellation)
                    .await
                    .map_err(internal)?;
                row.map(|row| json!({"kind":"artist","id":row.artist_key,"object_id":row.object_id,"source":source_id,"name":row.metadata.name,"uri":row.media_uri,"favorite":row.state.favorite,"album_artists":album_artist,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.iter()),"artwork_previews":artwork_previews(&products, row.artwork_binding.iter())}))
            }
            SidebarPin::Genre {
                genre_id,
                source_id,
            } => {
                let uri = library::source_entity_uri(source_id, "genre", genre_id);
                let row = database
                    .genre(&uri, None, &cancellation)
                    .await
                    .map_err(internal)?;
                row.map(|row| json!({"kind":"genre","id":row.genre_key,"object_id":row.object_id,"source":source_id,"name":row.metadata.name,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"artwork_count":row.artwork_binding.as_ref().map_or(row.representative_artwork.len(), |_| 1),"artwork_revision":artwork_revision(row.artwork_binding.as_ref().map(std::slice::from_ref).unwrap_or(&row.representative_artwork)),"artwork_previews":artwork_previews(&products, row.artwork_binding.as_ref().map(std::slice::from_ref).unwrap_or(&row.representative_artwork))}))
            }
            SidebarPin::Playlist {
                source_id,
                playlist_id,
            } => {
                let Some(key) = database
                    .playlist_key_by_identity(source_id.as_ref(), playlist_id, &cancellation)
                    .await
                    .map_err(internal)?
                else {
                    continue;
                };
                let row = database
                    .playlists(&[key], &cancellation)
                    .await
                    .map_err(internal)?
                    .pop();
                row.map(|row| {
                        let artwork = row.artwork_binding.as_slice();
                        json!({"kind":"playlist","id":row.playlist_key,"object_id":row.object_id,"source":row.source_id,"name":row.name,"writable":row.writable,"metadata_writable":row.metadata_writable,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":artwork.len(),"artwork_revision":artwork_revision(artwork),"artwork_previews":artwork_previews(&products, artwork)})
                    })
            }
            SidebarPin::SmartPlaylist { playlist_id } => {
                let Some(key) = database
                    .smart_playlist_key_by_object(playlist_id, &cancellation)
                    .await
                    .map_err(internal)?
                else {
                    continue;
                };
                let row = database
                    .smart_playlist_rows(source, &[key], folder, catalog::now(), &cancellation)
                    .await
                    .map_err(internal)?
                    .pop();
                row.map(|row| json!({"kind":"smart-playlist","id":row.smart_playlist_key,"object_id":row.object_id,"name":rufin_core::playlists::smart_playlist_display_name(&row),"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":row.artwork_binding.as_slice().len(),"artwork_revision":artwork_revision(row.artwork_binding.as_slice()),"artwork_previews":artwork_previews(&products, row.artwork_binding.as_slice()),"source":selected.as_ref().filter(|_| row.definition.current).map(|s| &s.source_id)}))
            }
        };
        if let Some(mut row) = row {
            if matches!(
                pin,
                SidebarPin::Playlist { .. } | SidebarPin::SmartPlaylist { .. }
            ) {
                let source = configured
                    .sources
                    .iter()
                    .find(|source| Some(source.id.as_str()) == row["source"].as_str());
                row["badge"] = match source {
                    Some(source) => json!({"kind":source.kind,"name":source.name}),
                    None => json!({"kind":null,"name":"Rufin"}),
                };
            }
            row["pin"] = json!(pin);
            items.push(row);
        }
    }
    Ok(json_response(
        StatusCode::OK,
        json!({"visible":sidebar.pins_visible,"pins":sidebar.pins,"items":items}),
    ))
}

async fn set(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        pin: SidebarPin,
        pinned: bool,
    }
    let input: Input = body(request).await?;
    let settings = products.settings.clone();
    let changed = tokio::task::spawn_blocking(move || settings.set_pinned(input.pin, input.pinned))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}

async fn reorder(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        moved: SidebarPin,
        target: SidebarPin,
    }
    let input: Input = body(request).await?;
    let settings = products.settings.clone();
    let changed =
        tokio::task::spawn_blocking(move || settings.reorder_pin(&input.moved, &input.target))
            .await
            .map_err(internal)?
            .map_err(internal)?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}
