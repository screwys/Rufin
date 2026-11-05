//! The theme, Home refreshes and folder listings.

use super::*;
use library::{FolderKey, Read, ReadCancellation, Row, SourceKey};

/// A cached folder of a source by object id.
pub(super) async fn folder_key(
    products: &ProductHandles,
    source: SourceKey,
    object: &str,
) -> Result<FolderKey, Error> {
    products
        .library
        .catalog_keys(
            library::CatalogKind::Folder,
            Some(source),
            &[object.to_owned()],
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?
        .first()
        .map(|key| FolderKey::from_raw(*key))
        .ok_or_else(|| bad_request("Folder not found"))
}

pub(super) async fn scope(
    products: &ProductHandles,
    source: &str,
    folder: Option<&str>,
) -> Result<(SourceKey, Option<FolderKey>), Error> {
    if source.is_empty() {
        return Err(bad_request("source is required"));
    }
    products.library.loaded().await;
    let source = products
        .library
        .source_key(&sources::SourceId::new(source))
        .ok_or_else(|| bad_request("Source not found"))?;
    let folder = match folder {
        Some(folder) => Some(folder_key(products, source, folder).await?),
        None => None,
    };
    Ok((source, folder))
}

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/appearance", get(appearance))
        .route("/api/folders", get(folders))
        .route("/api/folders/live", get(live_folders))
}

/// The theme and colors the app shows: `theme` is the selected theme's color scheme, or
/// System when the app follows the platform.
pub(super) fn appearance_data(products: &ProductHandles) -> Value {
    let colors = products.appearance.borrow();
    let theme = colors.get("color-scheme").map_or("System", String::as_str);
    json!({"theme":theme,"accent":products.settings.load().appearance.accent_preference,"colors":*colors})
}

async fn appearance(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(StatusCode::OK, appearance_data(&products)))
}

pub(super) async fn refresh_home(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        source: sources::SourceId,
        block: library::HomeSectionKind,
    }
    let input: Input = body(request).await?;
    let selected = source::ensure_selected(&products, input.source).await?;
    completion(products.source.refresh_home(selected, input.block)).await?;
    Ok(accepted())
}

pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

async fn folders(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let offset = number(parameters, "offset", 0)?;
    let limit = number(parameters, "limit", 100)?.min(128);
    let (source, folder) = scope(
        products,
        required(parameters, "source")?,
        parameters.get("folder").map(String::as_str),
    )
    .await?;
    let parent = match parameters.get("parent") {
        Some(parent) => Some(folder_key(products, source, parent).await?),
        None => folder,
    };
    let read = Read::Folders(library::FolderRead {
        source,
        parent,
        filter: String::new(),
        descending: false,
    });
    let order = products
        .library
        .order(&read, &ReadCancellation::new())
        .await
        .map_err(internal)?;
    let folders = products
        .library
        .rows(
            &order,
            offset..offset.saturating_add(limit),
            false,
            &ReadCancellation::new(),
        )
        .await
        .map_err(internal)?
        .into_iter()
        .filter_map(|row| match row {
            Row::Folder(row) => Some(
                json!({"id":row.object_id,"name":(!row.name.is_empty()).then_some(row.name),"track_count":row.summary.track_count}),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(json_response(
        StatusCode::OK,
        json!({"offset":offset,"limit":limit,"folders":folders}),
    ))
}

async fn live_folders(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let products = &products;
    let parameters = &parameters;
    let source_id = sources::SourceId::new(required(parameters, "source")?);
    let page = products
        .source
        .browse_folder(
            source_id,
            parameters.get("parent").cloned(),
            parameters.get("folder").cloned(),
        )
        .await
        .map_err(internal)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"folders":page.folders.iter().map(|folder| json!({"id":folder.object_id,"name":folder.name})).collect::<Vec<_>>(),"track_ids":page.tracks}),
    ))
}
