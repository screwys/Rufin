use super::*;
use crate::settings::SidebarPin;
use library::ReadCancellation;

fn artwork_revision<'a>(bindings: impl IntoIterator<Item = &'a Vec<u8>>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for binding in bindings {
        binding.hash(&mut hash);
    }
    format!("{:x}", hash.finish())
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new().route("/api/pins", get(list).post(set).patch(reorder))
}

async fn list(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    let settings = products.source.shared.settings.load().ui;
    let sidebar = settings.sidebar;
    let database = &products.library;
    let cancellation = ReadCancellation::new();
    let selected = products.source.selected_library();
    let configured = products.source.list_sources();
    let source = selected.as_ref().map(|s| s.source_key);
    let folder = selected.as_ref().and_then(|s| s.music_folder_key);
    let mut items = Vec::new();
    for pin in &sidebar.pins {
        let source_key = match pin {
            SidebarPin::Album { source_id, .. }
            | SidebarPin::Artist { source_id, .. }
            | SidebarPin::Genre { source_id, .. } => database
                .source_identity_key(source_id)
                .await
                .map_err(internal)?,
            _ => None,
        };
        let row = match pin {
            SidebarPin::Album {
                album_id,
                source_id,
            } => {
                let Some(source) = source_key else { continue };
                let Some(key) = database
                    .album_key_by_object(source, album_id, &cancellation)
                    .await
                    .map_err(internal)?
                else {
                    continue;
                };
                database.album_rows(source, &[key], None, &cancellation).await.map_err(internal)?.pop()
                    .map(|row| json!({"kind":"album","id":row.album_key,"object_id":row.object_id,"source":source_id,"title":row.title,"uri":row.media_uri,"favorite":row.favorite,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.iter())}))
            }
            SidebarPin::Artist {
                artist_id,
                source_id,
                album_artist,
            } => {
                let Some(source) = source_key else { continue };
                let Some(key) = database
                    .artist_key_by_object(source, artist_id, &cancellation)
                    .await
                    .map_err(internal)?
                else {
                    continue;
                };
                database.artist_rows(source, &[key], *album_artist, None, &cancellation).await.map_err(internal)?.pop()
                    .map(|row| json!({"kind":"artist","id":row.artist_key,"object_id":row.object_id,"source":source_id,"name":row.name,"uri":row.media_uri,"favorite":row.favorite,"album_artists":album_artist,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.iter())}))
            }
            SidebarPin::Genre {
                genre_id,
                source_id,
            } => {
                let Some(source) = source_key else { continue };
                let Some(key) = database
                    .genre_key_by_object(source, genre_id, &cancellation)
                    .await
                    .map_err(internal)?
                else {
                    continue;
                };
                database.genre_rows(source, &[key], None, &cancellation).await.map_err(internal)?.pop()
                    .map(|row| json!({"kind":"genre","id":row.genre_key,"object_id":row.object_id,"source":source_id,"name":row.name,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.as_ref().or_else(|| row.representative_artwork.first()))}))
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
                database.playlist_rows(&[key], &cancellation).await.map_err(internal)?.pop()
                    .map(|row| {
                        let artwork = crate::playlists::playlist_artwork_bindings(&row, settings.prefer_server_playlist_covers);
                        json!({"kind":"playlist","id":row.playlist_key,"object_id":row.object_id,"source":row.source_id,"name":row.name,"writable":row.writable,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":artwork.len(),"artwork_revision":artwork_revision(artwork)})
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
                database.smart_playlist_rows(source, &[key], folder, catalog::now(), &cancellation).await.map_err(internal)?.pop()
                    .map(|row| json!({"kind":"smart-playlist","id":row.smart_playlist_key,"object_id":row.object_id,"name":crate::playlists::smart_playlist_display_name(&row),"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":row.artwork_bindings.len(),"artwork_revision":artwork_revision(&row.artwork_bindings),"source":selected.as_ref().filter(|_| row.definition.current).map(|s| &s.source_id)}))
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
    let settings = products.source.shared.settings.clone();
    let changed = tokio::task::spawn_blocking(move || {
        settings.update(|stored| Ok(stored.ui.sidebar.set_pinned(input.pin, input.pinned)))
    })
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
    let settings = products.source.shared.settings.clone();
    let changed = tokio::task::spawn_blocking(move || {
        settings.update(|stored| {
            let Some(after) = stored
                .ui
                .sidebar
                .pin_drop_after(&input.moved, &input.target)
            else {
                return Ok(false);
            };
            Ok(stored
                .ui
                .sidebar
                .reorder_pin(&input.moved, &input.target, after))
        })
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(json_response(StatusCode::OK, json!({"changed":changed})))
}
