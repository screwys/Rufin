use super::*;
use library::{PinRow, ReadCancellation};
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
    let catalog = &products.library;
    let cancellation = ReadCancellation::new();
    let configured = products.source.list_sources();
    let pins = sidebar
        .pins
        .iter()
        .filter(|pin| !matches!(pin, SidebarPin::PlaylistFolder { .. }))
        .cloned()
        .collect::<Vec<_>>();
    let mut rows = catalog
        .pin_rows(&pins, &cancellation)
        .await
        .map_err(internal)?;
    catalog
        .complete_pin_rows(&mut rows, catalog::now(), &cancellation)
        .await
        .map_err(internal)?;
    let mut items = Vec::new();
    for (pin, row) in rows {
        let source = match &pin {
            SidebarPin::Album { source_id, .. }
            | SidebarPin::Artist { source_id, .. }
            | SidebarPin::Genre { source_id, .. } => Some(source_id),
            _ => None,
        };
        let mut row = match row {
            PinRow::Album(row) => {
                json!({"kind":"album","id":row.album_key,"object_id":row.object_id,"source":source,"title":row.metadata.title,"uri":row.media_uri,"favorite":row.state.favorite,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.iter()),"artwork_previews":artwork_previews(&products, row.artwork_binding.iter())})
            }
            PinRow::Artist(row) => {
                let album_artist = matches!(
                    pin,
                    SidebarPin::Artist {
                        album_artist: true,
                        ..
                    }
                );
                json!({"kind":"artist","id":row.artist_key,"object_id":row.object_id,"source":source,"name":row.metadata.name,"uri":row.media_uri,"favorite":row.state.favorite,"album_artists":album_artist,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"artwork_revision":artwork_revision(row.artwork_binding.iter()),"artwork_previews":artwork_previews(&products, row.artwork_binding.iter())})
            }
            PinRow::Genre(row) => {
                json!({"kind":"genre","id":row.genre_key,"object_id":row.object_id,"source":source,"name":row.metadata.name,"track_count":row.summary.track_count,"duration_ms":row.summary.duration_millis,"artwork_count":row.artwork_binding.as_ref().map_or(row.representative_artwork.len(), |_| 1),"artwork_revision":artwork_revision(row.artwork_binding.as_ref().map(std::slice::from_ref).unwrap_or(&row.representative_artwork)),"artwork_previews":artwork_previews(&products, row.artwork_binding.as_ref().map(std::slice::from_ref).unwrap_or(&row.representative_artwork))})
            }
            PinRow::Playlist(row) => {
                let artwork = row.artwork_binding.as_slice();
                json!({"kind":"playlist","id":row.playlist_key,"object_id":row.object_id,"source":row.source_id,"name":row.name,"writable":row.writable,"metadata_writable":row.metadata_writable,"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":artwork.len(),"artwork_revision":artwork_revision(artwork),"artwork_previews":artwork_previews(&products, artwork)})
            }
            PinRow::SmartPlaylist(row) => {
                json!({"kind":"smart-playlist","id":row.smart_playlist_key,"object_id":row.object_id,"name":rufin_core::playlists::smart_playlist_display_name(&row.object_id, &row.name),"track_count":row.track_count,"duration_ms":row.duration_millis,"artwork_count":row.artwork_binding.as_slice().len(),"artwork_revision":artwork_revision(row.artwork_binding.as_slice()),"artwork_previews":artwork_previews(&products, row.artwork_binding.as_slice()),"source":None::<String>})
            }
            // Filtered out above: the web lists no folder pins.
            PinRow::PlaylistFolder(_) => continue,
        };
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
