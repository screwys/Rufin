use super::*;
use rufin_core::{EqEdit, EqPresetFormat};

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/equalizer", get(view).post(edit))
        .route("/api/equalizer/presets", get(presets))
        .route("/api/equalizer/events", get(events))
        .route("/api/equalizer/response", get(response))
}
#[derive(Default, Deserialize)]
struct Target {
    #[serde(default)]
    output: String,
    #[serde(default)]
    meter: bool,
    #[serde(default)]
    spectrum: bool,
}
async fn view(
    State(products): State<ProductHandles>,
    Query(target): Query<Target>,
) -> Result<Response<Body>, Error> {
    Ok(json_response(
        StatusCode::OK,
        serde_json::to_value(products.playback.equalizer_view(target.output)).map_err(internal)?,
    ))
}
async fn presets(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(
        StatusCode::OK,
        json!(products.playback.equalizer_presets()),
    ))
}
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Edit {
    Preview {
        output: String,
        edit: EqEdit,
    },
    Commit {
        output: String,
    },
    Select {
        output: String,
        preset: Option<String>,
    },
    Save {
        output: String,
        name: Option<String>,
    },
    Duplicate {
        output: String,
        name: String,
    },
    Rename {
        id: String,
        name: String,
    },
    Delete {
        id: String,
    },
    Import {
        output: String,
        text: String,
        format: EqPresetFormat,
    },
    Export {
        output: String,
        format: EqPresetFormat,
    },
    Assign {
        output: String,
        assigned: bool,
    },
    Apply {
        output: String,
    },
}
async fn edit(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let owner = &products.playback;
    let result = match body::<Edit>(request).await? {
        Edit::Preview { output, edit } => {
            owner.preview_equalizer(output, edit).map_err(bad_request)?;
            None
        }
        Edit::Commit { output } => {
            owner.commit_equalizer(output).await.map_err(internal)?;
            None
        }
        Edit::Select { output, preset } => {
            owner
                .select_equalizer_preset(output, preset)
                .await
                .map_err(bad_request)?;
            None
        }
        Edit::Save { output, name } => {
            Some(json!({"id":owner.save_equalizer_preset(output,name).await.map_err(bad_request)?}))
        }
        Edit::Duplicate { output, name } => Some(
            json!({"id":owner.duplicate_equalizer_preset(output,name).await.map_err(bad_request)?}),
        ),
        Edit::Rename { id, name } => {
            owner
                .rename_equalizer_preset(id, name)
                .await
                .map_err(bad_request)?;
            None
        }
        Edit::Delete { id } => {
            owner
                .delete_equalizer_preset(id)
                .await
                .map_err(bad_request)?;
            None
        }
        Edit::Import {
            output,
            text,
            format,
        } => Some(
            json!({"id":owner.import_equalizer(output,text,format).await.map_err(bad_request)?}),
        ),
        Edit::Export { output, format } => {
            Some(json!({"text":owner.export_equalizer(output,format).map_err(bad_request)?}))
        }
        Edit::Assign { output, assigned } => {
            owner
                .assign_equalizer(output, assigned)
                .await
                .map_err(internal)?;
            None
        }
        Edit::Apply { output } => {
            owner.apply_equalizer(output).map_err(bad_request)?;
            None
        }
    };
    Ok(json_response(
        StatusCode::OK,
        result.unwrap_or_else(|| json!({"accepted":true})),
    ))
}
async fn events(
    State(products): State<ProductHandles>,
    Query(target): Query<Target>,
) -> Response<Body> {
    let subscription = products.playback.clone().subscribe_equalizer(target.output);
    subscription.set_measurement(target.meter, target.spectrum);
    let stream = futures_util::stream::unfold(subscription, |subscription| async move {
        let view = subscription.next().await.ok()?;
        let value = serde_json::to_string(&view).ok()?;
        Some((
            Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                "event: equalizer\ndata: {value}\n\n"
            )))),
            subscription,
        ))
    });
    event_response(Body::new(StreamBody::new(stream)))
}
async fn response(
    State(products): State<ProductHandles>,
    Query(target): Query<Target>,
) -> Result<Response<Body>, Error> {
    let view = products.playback.equalizer_view(target.output);
    let value = tokio::task::spawn_blocking(move || {
        let rate = view.sample_rate.unwrap_or(48000);
        let end = (f64::from(rate) / 2.).min(20000.);
        let frequencies: Vec<_> = (0..256)
            .map(|i| 20. * (end / 20.).powf(f64::from(i) / 255.))
            .collect();
        let gains = if view.enabled {
            audio_processing::effects::response_curve_db(&view.profile, rate, &frequencies)
        } else {
            vec![0.0; frequencies.len()]
        };
        json!({"frequencies":frequencies,"gain_db":gains,"sample_rate":rate})
    })
    .await
    .map_err(internal)?;
    Ok(json_response(StatusCode::OK, value))
}
