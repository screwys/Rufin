use super::*;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/connect", get(status).post(action))
        .route("/api/connect/events", get(events))
        .route("/api/connect/continuation", get(continuation))
        .route("/api/connect/roots", get(roots))
}

#[derive(serde::Deserialize)]
struct RootQuery {
    source: String,
}

async fn roots(
    State(products): State<ProductHandles>,
    Query(query): Query<RootQuery>,
) -> Result<axum::Json<Vec<library::ConnectRoot>>, Error> {
    products
        .connect
        .roots(&query.source)
        .await
        .map(axum::Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, axum::Json(json!({"error":error}))))
}

async fn continuation(
    State(products): State<ProductHandles>,
) -> Result<axum::Json<Option<rufin_core::connect::Device>>, Error> {
    products
        .connect
        .continuation_offer()
        .await
        .map(axum::Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, axum::Json(json!({"error":error}))))
}

async fn status(State(products): State<ProductHandles>) -> axum::Json<rufin_core::connect::Status> {
    axum::Json(products.connect.status())
}

async fn action(
    State(products): State<ProductHandles>,
    axum::Json(action): axum::Json<rufin_core::connect::Action>,
) -> Result<axum::Json<rufin_core::connect::Status>, Error> {
    products
        .connect
        .execute(action)
        .await
        .map(axum::Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, axum::Json(json!({"error":error}))))
}

async fn events(State(products): State<ProductHandles>) -> Response<Body> {
    let stream =
        futures_util::stream::unfold(products.connect.subscribe(), |mut updates| async move {
            updates.changed().await.ok()?;
            let data = serde_json::to_string(&*updates.borrow_and_update()).ok()?;
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: connect\ndata: {data}\n\n"
                )))),
                updates,
            ))
        });
    event_response(Body::new(StreamBody::new(stream)))
}
