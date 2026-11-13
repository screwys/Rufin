use super::*;

/// The largest file an import reads: a ListenBrainz export ZIP of a long history.
const IMPORT_LIMIT: usize = 512 * 1024 * 1024;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/activity/export", get(export))
        .route(
            "/api/activity/import",
            post(import).layer(axum::extract::DefaultBodyLimit::max(IMPORT_LIMIT)),
        )
}

async fn export(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let format = match parameters.get("format").map(String::as_str) {
        Some("listenbrainz") => library::ActivityExportFormat::ListenBrainz,
        Some("jsonl") => library::ActivityExportFormat::Jsonl,
        _ => library::ActivityExportFormat::LastFm,
    };
    let source = parameters
        .get("source")
        .filter(|source| !source.is_empty())
        .map(library::SourceId::new);
    let mut output = Vec::new();
    products
        .library
        .export_activity(&mut output, format, source.as_ref())
        .await
        .map_err(internal)?;
    let (name, kind) = match format {
        library::ActivityExportFormat::Jsonl => ("activity.jsonl", "application/jsonl"),
        _ => ("activity.csv", "text/csv; charset=utf-8"),
    };
    Ok(Response::builder()
        .header("content-type", kind)
        .header(
            "content-disposition",
            format!("attachment; filename=\"{name}\""),
        )
        .header("cache-control", "no-store")
        .body(Body::from(output))
        .expect("static response headers"))
}

async fn import(
    State(products): State<ProductHandles>,
    body: Bytes,
) -> Result<Response<Body>, Error> {
    let report = products
        .library
        .import_activity(&body, None)
        .await
        .map_err(bad_request)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"message": rufin_core::backup::activity_import_summary(&report)}),
    ))
}
