//! Radio stations: the Radio page as core reads it, and what its cards and dialogs do.

use super::*;
use rufin_core::route::Route;
use rufin_core::runtime::SelectedLibrary;
use rufin_core::stations::{RadioPage, Station, StationInput};

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route(
            "/api/stations",
            list_route("stations", (0, 0))
                .post(add)
                .patch(edit)
                .delete(delete),
        )
        .route("/api/stations/listed", post(add_listed))
        .route("/api/stations/order", post(reorder))
        .route("/api/stations/picture", get(picture))
        .route("/api/stations/probe", post(probe))
        .route("/api/stations/search", get(search))
        .route("/api/queue/station", post(play))
}

/// The Radio page as it read.
pub(super) struct Opened {
    pub page: RadioPage,
    /// What committed while it read, as an update frame shows it on this page.
    pub missed: Option<Value>,
}

/// Reads the Radio page while `selected` is the selected library, whose stations it shows. A
/// read that opens it also reads the server's stations into the Store, without waiting: the
/// stored ones show at once, and the change that read commits reads the page again.
pub(super) async fn open(
    products: &ProductHandles,
    selected: Option<SelectedLibrary>,
    opening: bool,
) -> Result<Opened, Error> {
    if opening {
        let source = products.source.clone();
        tokio::spawn(async move {
            if let Err(error) = rufin_core::stations::refresh(&source).await {
                tracing::debug!(%error, "could not read the server's radio stations");
            }
        });
    }
    let reading = products.source.reading();
    let page = rufin_core::stations::page(&products.source, &products.playback)
        .await
        .map_err(internal)?;
    let missed = reading.missed().map(|missed| {
        let mut value = json!({});
        let current = products.source.selected_library();
        let refresh = rufin_core::view::refresh_unread(
            &Route::Radio,
            selected.as_ref(),
            current.as_ref(),
            &missed,
        );
        show_changed(&mut value, Some(refresh), &missed);
        value
    });
    Ok(Opened {
        page,
        missed: missed.filter(|value| value.as_object().is_some_and(|value| !value.is_empty())),
    })
}

/// A station as the API lists it. `picture` names its logo for `/api/artwork?picture=`, and
/// `pin` is what `/api/pins` takes to pin it or a channel of it, null where it has no pin.
pub(super) fn station_json(products: &ProductHandles, station: &Station) -> Value {
    let channels = station.channels.iter().map(|channel| {
        json!({
            "name":channel.name,"playing":channel.playing,"own":channel.own,
            "pin":channel.pin,"pinned":channel.pinned,
        })
    });
    json!({
        "key":station.key,"listed":station.listed,"name":station.name,"description":station.description,
        "genres":station.genres,"homepage":station.homepage,"stream_url":station.stream_url,
        "channels":channels.collect::<Vec<_>>(),"playing":station.playing,
        "picture":station.artwork_binding.as_deref().map(media::picture),
        "artwork_previews":artwork_previews(products, station.artwork_binding.iter()),
        "pin":station.pin,"pinned":station.pinned,"channel":station.channel,
    })
}

/// The Radio page's JSON: `{on_air, stations, suggested, saved_on}`.
pub(super) fn json(products: &ProductHandles, token: &page::PageToken, opened: &Opened) -> Value {
    let page = &opened.page;
    let stations = |stations: &[Station]| {
        stations
            .iter()
            .map(|station| station_json(products, station))
            .collect::<Vec<_>>()
    };
    let on_air = page.on_air.as_ref().map(|air| {
        let heard = air.heard.iter().map(
            |song| json!({"at":song.at_unix_seconds,"title":song.title,"artist":song.artist}),
        );
        json!({
            "station":station_json(products, &air.station),"channel":air.channel,"title":air.title,
            "artist":air.artist,"format":air.format,"playing":air.playing,"tuned":air.tuned,"heard":heard.collect::<Vec<_>>(),
        })
    });
    let mut value = json!({
        "page":token,"on_air":on_air,"stations":stations(&page.stations),
        "suggested":stations(&page.suggested),"saved_on":page.saved_on,
    });
    if let Some(missed) = &opened.missed {
        value["missed"] = missed.clone();
    }
    value
}

async fn play(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        key: Option<i64>,
        listed: Option<String>,
        channel: Option<String>,
    }
    let input: Input = body(request).await?;
    rufin_core::stations::play(
        &products.source,
        &products.playback,
        input.key,
        input.listed,
        input.channel,
    )
    .await
    .map_err(bad_request)?;
    Ok(accepted())
}

#[derive(Deserialize)]
struct Entered {
    name: String,
    #[serde(default)]
    stream_url: String,
    #[serde(default)]
    homepage: String,
}

impl Entered {
    /// `logo` is the address of the picture the search that found the station gave it.
    fn input(self, logo: String) -> StationInput {
        StationInput {
            name: self.name,
            stream_url: self.stream_url,
            homepage: self.homepage,
            logo,
        }
    }
}

async fn add(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        #[serde(flatten)]
        entered: Entered,
        #[serde(default)]
        logo: String,
    }
    let input: Input = body(request).await?;
    rufin_core::stations::add(&products.source, input.entered.input(input.logo))
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn add_listed(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let input: Identifier = body(request).await?;
    rufin_core::stations::add_listed(&products.source, input.id)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

/// What Edit does with the picture chosen for a station.
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum PictureEdit {
    /// An uploaded image, base64 encoded.
    Upload {
        data: String,
    },
    Remove,
}

async fn edit(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        key: i64,
        #[serde(flatten)]
        entered: Entered,
        channel: Option<String>,
        picture: Option<PictureEdit>,
    }
    let input: Input = body(request).await?;
    let picture = match input.picture {
        Some(PictureEdit::Upload { data }) => {
            let bytes =
                <_ as base64::Engine>::decode(&base64::engine::general_purpose::STANDARD, data)
                    .map_err(bad_request)?;
            let image = completion(rufin_core::metadata::prepare_artwork(
                &products.source,
                bytes,
            ))
            .await?;
            Some(sources::ArtworkChange::Replace(image))
        }
        Some(PictureEdit::Remove) => Some(sources::ArtworkChange::Remove),
        None => None,
    };
    rufin_core::stations::edit(
        &products.source,
        input.key,
        input.entered.input(String::new()),
        input.channel,
        picture,
    )
    .await
    .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

/// The pictures Edit shows for a station: `chosen`, the one a person chose for it, and
/// `automatic`, the one it has without; each as a station lists its picture, or null.
async fn picture(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let editing = rufin_core::stations::picture_editing(&products.source, key(&parameters, "key")?)
        .await
        .map_err(bad_request)?;
    let shown = |binding: Option<&Vec<u8>>| {
        binding.map(|binding| {
            json!({
                "picture":media::picture(binding),
                "artwork_previews":artwork_previews(&products, [binding]),
            })
        })
    };
    Ok(json_response(
        StatusCode::OK,
        json!({"chosen":shown(editing.binding.as_ref()),"automatic":shown(editing.automatic_binding.as_ref())}),
    ))
}

async fn reorder(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        keys: Vec<i64>,
    }
    let input: Input = body(request).await?;
    rufin_core::stations::reorder(&products.source, input.keys)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn delete(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    rufin_core::stations::delete(&products.source, key(&parameters, "key")?)
        .await
        .map_err(bad_request)?;
    Ok(json_response(StatusCode::OK, json!({"changed":true})))
}

async fn probe(request: Request<Body>) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        address: String,
    }
    let input: Input = body(request).await?;
    let probe = rufin_core::stations::probe(input.address)
        .await
        .map_err(bad_request)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"stream_url":probe.stream_url,"name":probe.name,"genre":probe.genre,"format":probe.format,"host":probe.host}),
    ))
}

async fn search(
    State(products): State<ProductHandles>,
    Query(parameters): Query<HashMap<String, String>>,
) -> Result<Response<Body>, Error> {
    let query = parameters.get("q").cloned().unwrap_or_default();
    let found = rufin_core::stations::search(&products.source, query)
        .await
        .map_err(bad_request)?;
    let stations = found.iter().map(|station| {
        json!({
            "name":station.name,"stream_url":station.stream_url,"homepage":station.homepage,
            "detail":station.detail,"logo":station.logo,"added":station.added,
            "picture":station.artwork_binding.as_deref().map(media::picture),
            "artwork_previews":artwork_previews(&products, station.artwork_binding.iter()),
        })
    });
    Ok(json_response(
        StatusCode::OK,
        json!({"stations":stations.collect::<Vec<_>>()}),
    ))
}
