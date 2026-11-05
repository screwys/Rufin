use super::*;
use library::{PlaylistListRow, ReadCancellation, Row};
use rufin_core::settings::SidebarPin;

fn artwork_revision<'a>(bindings: impl IntoIterator<Item = &'a [u8]>) -> String {
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
    let sidebar = &settings.sidebar;
    let configured = products.source.list_sources();
    let rows = products
        .library
        .pin_rows(&sidebar.pins, catalog::now(), &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let mut items = Vec::new();
    for (pin, row) in rows {
        let source = match &pin {
            SidebarPin::Album { source_id, .. }
            | SidebarPin::Artist { source_id, .. }
            | SidebarPin::Genre { source_id, .. } => Some(source_id.clone()),
            _ => None,
        };
        let scope = page::PageToken::at_source(source.clone());
        let mut item = browser::row_data(&products, &settings, &row, &scope);
        let collection = row.collection();
        item["artwork_revision"] = json!(artwork_revision(
            collection
                .iter()
                .flat_map(|shown| shown.artwork.iter().copied())
        ));
        item["duration_text"] = json!(rufin_core::settings::presentation::format_duration_units(
            collection.map_or(0, |shown| (shown.duration_millis.max(0) / 1000) as u32)
        ));
        if let Row::Playlist(PlaylistListRow::Playlist(_) | PlaylistListRow::SmartPlaylist(_)) =
            &row
        {
            let source = configured
                .sources
                .iter()
                .find(|source| Some(source.id.as_str()) == item["source"].as_str());
            item["badge"] = match source {
                Some(source) => json!({"kind":source.kind,"name":source.name}),
                None => json!({"kind":null,"name":"Rufin"}),
            };
        }
        item["source"] =
            json!(source.or_else(|| item["source"].as_str().map(sources::SourceId::new)));
        item["pin"] = json!(pin);
        items.push(item);
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
