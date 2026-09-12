use std::sync::Arc;
use std::time::Duration;

use playback::{BackendCommand, BackendError, BackendEvent, PlaybackBackend};
use rufin_core::{app, diagnostics::Diagnostics, paths::Paths};

struct IdleBackend;

impl PlaybackBackend for IdleBackend {
    fn send(&mut self, _: BackendCommand) -> Result<(), BackendError> {
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        Vec::new()
    }
}

#[test]
fn application_starts_on_a_worker_and_reopens_saved_settings_without_a_ui() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: directory.path().join("config"),
        cache: directory.path().join("cache"),
        data: directory.path().join("data"),
        state: directory.path().join("state"),
    };
    let (diagnostics, _stderr) = Diagnostics::install(paths.state_dir());
    for reopening in [false, true] {
        app::with_runtime(|runtime| {
            let inputs = runtime
                .block_on(runtime.spawn(app::runtime_inputs(
                    diagnostics.clone(),
                    false,
                    app::startup_settings(&paths),
                    paths.clone(),
                    || Ok(Box::new(IdleBackend) as Box<dyn PlaybackBackend>),
                    Vec::new,
                    Arc::new(|_, _| {}),
                    Arc::new(|_, _| {}),
                )))
                .unwrap()
                .unwrap();
            assert!(!inputs.temporary_store);
            assert_eq!(inputs.settings.load().private_mode, reopening);
            let settings = inputs.settings.clone();
            runtime
                .block_on(runtime.spawn_blocking(move || {
                    let mut saved = settings.load();
                    saved.private_mode = true;
                    settings.save(&saved).unwrap();
                }))
                .unwrap();
            inputs.products.playback.transport.set_muted(true);
            let mut playback = inputs.products.playback.updates.subscribe();
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let Some(projection) = playback.recv().await.unwrap() else {
                            continue;
                        };
                        if projection.view.controls.muted {
                            break;
                        }
                    }
                })
                .await
                .expect("shared playback commands publish without GTK");
            });
            if !reopening {
                runtime.block_on(exercise_desktop_controller(&inputs));
                runtime.block_on(exercise_http_api(
                    inputs.products.clone(),
                    inputs.settings.clone(),
                    directory.path(),
                ));
            }
            inputs.receivers.visualizer.close();
            drop(inputs.receivers);
            inputs.products.playback.transport.shutdown();
        })
        .unwrap();
    }
}

async fn exercise_desktop_controller(inputs: &rufin_core::runtime::RuntimeInputs) {
    use rufin_core::api::Controller;
    inputs
        .products
        .source
        .change_secret_storage(secrets::SecretStorageMode::ConfigFile)
        .recv()
        .await
        .unwrap()
        .unwrap();
    let controller = Controller::new(inputs.products.clone());
    let mut status = controller.status();
    assert!(status.borrow().address.is_none());
    let mut settings = inputs.settings.load();
    assert!(!settings.web_controller.enabled);
    assert_eq!(settings.web_controller.port, 1717);
    settings.web_controller.enabled = true;
    settings.web_controller.port = 0;
    inputs.settings.save(&settings).unwrap();
    let started = controller_status(&mut status, |state| state.address.is_some()).await;
    assert_eq!(started.token.len(), 64);
    let address = started.address.unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://{address}/api/playback");
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let playback: serde_json::Value = client
        .get(&url)
        .bearer_auth(&started.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(playback["muted"], true);
    let mut events = client
        .get(format!("http://{address}/api/events"))
        .bearer_auth(&started.token)
        .send()
        .await
        .unwrap();
    assert!(events.chunk().await.unwrap().is_some());

    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    settings.web_controller.port = occupied.local_addr().unwrap().port();
    inputs.settings.save(&settings).unwrap();
    controller_status(&mut status, |state| state.error.is_some()).await;
    assert!(
        client
            .get(&url)
            .bearer_auth(&started.token)
            .send()
            .await
            .is_err()
    );
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(_)) = events.chunk().await {}
    })
    .await;
    assert!(closed.is_ok(), "rebinding closes existing event streams");

    settings.web_controller.port = 0;
    inputs.settings.save(&settings).unwrap();
    let rebound = controller_status(&mut status, |state| state.address.is_some()).await;
    assert_eq!(rebound.token, started.token);
    let mut connected = client
        .get(format!("http://{}/api/events", rebound.address.unwrap()))
        .bearer_auth(&rebound.token)
        .send()
        .await
        .unwrap();
    assert!(connected.chunk().await.unwrap().is_some());
    let regenerated = controller.regenerate_token().await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Ok(Some(_)) = connected.chunk().await {}
    })
    .await
    .expect("regeneration disconnects existing controller clients");
    assert_ne!(regenerated, started.token);
    assert_eq!(regenerated.len(), 64);
    let renewed = controller_status(&mut status, |state| {
        state.address.is_some() && state.token == regenerated
    })
    .await;
    let url = format!("http://{}/api/playback", renewed.address.unwrap());
    assert_eq!(
        client
            .get(&url)
            .bearer_auth(&started.token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(&url)
            .bearer_auth(&regenerated)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    drop(controller);
    tokio::time::timeout(Duration::from_secs(5), async {
        while status.changed().await.is_ok() {}
    })
    .await
    .unwrap();
    let controller = Controller::new(inputs.products.clone());
    let mut status = controller.status();
    let reopened = controller_status(&mut status, |state| state.address.is_some()).await;
    assert_eq!(
        reopened.token, regenerated,
        "regenerated token survives host restart"
    );
    settings.web_controller.enabled = false;
    inputs.settings.save(&settings).unwrap();
    controller_status(&mut status, |state| state.address.is_none()).await;
    assert!(
        client
            .get(format!("http://{}/api", reopened.address.unwrap()))
            .send()
            .await
            .is_err()
    );
    let mut playback = inputs.products.playback.updates.subscribe();
    for muted in [false, true] {
        inputs.products.playback.transport.set_muted(muted);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if playback
                    .recv()
                    .await
                    .unwrap()
                    .is_some_and(|state| state.view.controls.muted == muted)
                {
                    break;
                }
            }
        })
        .await
        .expect("disabling web leaves playback running");
    }
}

async fn controller_status(
    status: &mut tokio::sync::watch::Receiver<rufin_core::api::ControllerStatus>,
    predicate: impl Fn(&rufin_core::api::ControllerStatus) -> bool,
) -> rufin_core::api::ControllerStatus {
    tokio::time::timeout(Duration::from_secs(5), status.wait_for(predicate))
        .await
        .expect("controller status settled")
        .unwrap()
        .clone()
}

async fn exercise_http_api(
    products: rufin_core::runtime::ProductHandles,
    settings: rufin_core::SettingsHandle,
    root: &std::path::Path,
) {
    use serde_json::{Value, json};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}/api");
    let token = "application-test-token";
    let server = tokio::spawn(rufin_core::api::serve(
        listener,
        products.clone(),
        token.into(),
    ));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let api = |method, route: &str, value: Value| {
        let request = client
            .request(method, format!("{base}/{route}"))
            .bearer_auth(token)
            .json(&value);
        async move {
            let response = request.send().await.unwrap();
            let status = response.status();
            let value: Value = response.json().await.unwrap();
            assert!(status.is_success(), "{status}: {value}");
            value
        }
    };
    assert_eq!(
        client.get(&base).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(format!("{base}/playback/stop"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let help: Value = client
        .get(&base)
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(help["routes"]["GET /api/playback"].is_string());
    let page = client
        .get(format!("http://{address}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), reqwest::StatusCode::OK);
    let html = page.text().await.unwrap();
    assert!(html.contains("id=\"login-dialog\""));
    assert!(!html.contains(token));
    for path in [
        "events",
        "lyrics",
        "lyrics/events",
        "artwork?uri=missing",
        "artists?source=missing",
        "smart-playlists",
    ] {
        assert_eq!(
            client
                .get(format!("{base}/{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        api(reqwest::Method::GET, "lyrics", Value::Null).await["state"],
        "empty"
    );
    let mut combined = client
        .get(format!("{base}/events"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let initial = playback_event(&mut combined).await;
    assert_eq!(initial["playback"]["muted"], true);
    assert_eq!(initial["source"]["state"], "idle");
    assert_eq!(initial["lyrics"]["state"], "empty");
    drop(combined);
    api(reqwest::Method::POST, "lyrics", json!({})).await;

    let mut first = client
        .get(format!("{base}/playback/events"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let mut second = client
        .get(format!("{base}/playback/events"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(playback_event(&mut first).await["muted"], true);
    assert_eq!(playback_event(&mut second).await["muted"], true);
    let mut native = products.playback.updates.subscribe();
    native.recv().await.unwrap();
    assert_eq!(
        client
            .post(format!("{base}/playback/mute"))
            .bearer_auth(token)
            .json(&json!({"muted":false}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::ACCEPTED
    );
    assert_eq!(playback_event(&mut first).await["muted"], false);
    assert_eq!(playback_event(&mut second).await["muted"], false);
    assert!(!native.recv().await.unwrap().unwrap().view.controls.muted);
    let error = playback::PlaybackNotice::OperationFailed("Media unavailable".into());
    products
        .playback
        .updates
        .publish(playback::PlaybackProjection {
            view: (*products.playback.updates.current().unwrap()).clone(),
            notices: vec![error.clone()],
        });
    assert_eq!(native.recv().await.unwrap().unwrap().notices, [error]);
    for subscriber in [&mut first, &mut second] {
        assert_eq!(
            playback_event(subscriber).await["notices"],
            json!([
                {"type":"operation_failed","error":"Media unavailable"}
            ])
        );
    }
    drop(first);
    drop(second);
    let late: Value = client
        .get(format!("{base}/playback"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(late["muted"], false);
    for route in ["sources/select", "queue/activate"] {
        assert_eq!(
            client
                .post(format!("{base}/{route}"))
                .bearer_auth(token)
                .json(&json!({"id":""}))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        client
            .post(format!("{base}/playback/seek"))
            .bearer_auth(token)
            .json(&json!({"position_ms":"bad"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );

    let mut sources = Vec::new();
    for name in ["first", "second"] {
        let music = root.join(name);
        std::fs::create_dir(&music).unwrap();
        let response = client
            .post(format!("{base}/sources/local"))
            .bearer_auth(token)
            .json(&json!({"paths":[music]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let value: Value = response.json().await.unwrap();
        sources.push(value["id"].as_str().unwrap().to_owned());
    }
    let selected = products.source.selected_library().unwrap().source_id;
    assert_eq!(selected.as_str(), sources[1]);
    assert_eq!(
        api(
            reqwest::Method::GET,
            &format!("artists?source={}", sources[0]),
            Value::Null
        )
        .await["artists"],
        json!([])
    );
    let smart = api(reqwest::Method::GET, "smart-playlists", Value::Null).await;
    assert!(!smart["smart_playlists"].as_array().unwrap().is_empty());
    let random = api(
        reqwest::Method::GET,
        &format!("random?source={}", sources[0]),
        Value::Null,
    )
    .await;
    assert_eq!(random["genres"], json!([]));
    assert_eq!(
        api(
            reqwest::Method::POST,
            "queue/random",
            json!({"source":sources[0],"count":1,"mode":"replace"})
        )
        .await["empty"],
        true
    );
    let invalid = client
        .post(format!("{base}/sources/refresh"))
        .bearer_auth(token)
        .json(&json!({"id":"missing"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        api(reqwest::Method::GET, "sources/progress", Value::Null).await["state"],
        "failed"
    );
    assert_eq!(
        api(
            reqwest::Method::POST,
            "sources/refresh",
            json!({"id":sources[0]})
        )
        .await["refreshed"],
        true
    );
    assert_eq!(
        api(reqwest::Method::GET, "sources/progress", Value::Null).await["state"],
        "idle"
    );
    let response: Value = client
        .get(format!("{base}/tracks"))
        .bearer_auth(token)
        .query(&[("source", &sources[0])])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["tracks"], json!([]));
    assert_eq!(
        products.source.selected_library().unwrap().source_id,
        selected
    );

    let items = [
        "https://example.invalid/one.flac",
        "https://example.invalid/two.flac",
    ];
    assert_eq!(
        client
            .post(format!("{base}/queue"))
            .bearer_auth(token)
            .json(&json!({"uris":items,"mode":"append"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if native.recv().await.unwrap().unwrap().view.queue.total == 2 {
                break;
            }
        }
    })
    .await
    .unwrap();
    let queue: Value = client
        .get(format!("{base}/queue"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue["total"], 2);
    let limited = api(reqwest::Method::GET, "queue?limit=1", Value::Null).await;
    assert_eq!(limited["total"], 2);
    assert_eq!(limited["window"].as_array().unwrap().len(), 1);
    let moved = queue["window"][0]["id"].clone();
    api(
        reqwest::Method::POST,
        "queue/reorder",
        json!({"ids":[moved],"before":null}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if api(reqwest::Method::GET, "queue", Value::Null).await["current_index"] == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let playlist = api(
        reqwest::Method::POST,
        "playlists",
        json!({"name":"HTTP <script> playlist & \"quotes\"","uris":items}),
    )
    .await["id"]
        .clone();
    assert!(playlist.is_i64());
    let html = client
        .get(format!("{base}/playlists"))
        .bearer_auth(token)
        .header("HX-Request", "true")
        .send()
        .await
        .unwrap();
    assert_eq!(html.headers()["content-type"], "text/html; charset=utf-8");
    assert_eq!(html.headers()["vary"], "HX-Request");
    let html = html.text().await.unwrap();
    assert!(html.contains("HTTP &#60;script&#62; playlist"));
    assert!(!html.contains("<script>"));
    assert!(
        api(reqwest::Method::GET, "playlists", Value::Null).await["playlists"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "HTTP <script> playlist & \"quotes\"")
    );
    assert_eq!(
        api(
            reqwest::Method::PATCH,
            "playlists",
            json!({"id":playlist,"name":"Renamed"})
        )
        .await["changed"],
        true
    );
    assert_eq!(
        api(
            reqwest::Method::POST,
            "playlists/entries",
            json!({"id":playlist,"uris":[items[0]]})
        )
        .await["added"],
        1
    );
    assert_eq!(
        api(
            reqwest::Method::POST,
            "playlists/entries",
            json!({"id":playlist,"uris":[items[0]],"skip_duplicates":true})
        )
        .await["added"],
        0
    );
    let entries = api(
        reqwest::Method::GET,
        &format!("playlists/entries?id={playlist}"),
        Value::Null,
    )
    .await["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(entries.len(), 3);
    assert_ne!(entries[0]["id"], entries[2]["id"]);
    assert_eq!(
        api(
            reqwest::Method::PATCH,
            "playlists/entries",
            json!({"id":playlist,"entry":entries[1]["id"],"position":0})
        )
        .await["changed"],
        true
    );
    let reordered = api(
        reqwest::Method::GET,
        &format!("playlists/entries?id={playlist}&limit=1"),
        Value::Null,
    )
    .await;
    assert_eq!(reordered["entries"][0]["uri"], items[1]);
    assert_eq!(
        api(
            reqwest::Method::DELETE,
            "playlists/entries",
            json!({"id":playlist,"entries":[entries[2]["id"]]})
        )
        .await["changed"],
        true
    );
    assert_eq!(
        api(
            reqwest::Method::GET,
            "playlists/entries?id=-9223372036854775808",
            Value::Null
        )
        .await["entries"],
        json!([])
    );
    api(
        reqwest::Method::POST,
        "queue/playlist",
        json!({"id":playlist,"mode":"replace","anchor_entry":entries[0]["id"]}),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let queue = api(reqwest::Method::GET, "queue", Value::Null).await;
            let playing = api(reqwest::Method::GET, "playback", Value::Null).await;
            if queue["window"][0]["track"]["uri"] == items[1]
                && queue["total"] == 2
                && playing["current"]["track"]["uri"] == items[0]
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        api(
            reqwest::Method::DELETE,
            &format!("playlists?id={playlist}"),
            Value::Null
        )
        .await["changed"],
        true
    );
    let mut wav = b"RIFF".to_vec();
    wav.extend(16036_u32.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16_u32.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(8000_u32.to_le_bytes());
    wav.extend(16000_u32.to_le_bytes());
    wav.extend(2_u16.to_le_bytes());
    wav.extend(16_u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(16000_u32.to_le_bytes());
    wav.resize(16044, 0);
    for index in 0..150 {
        std::fs::write(
            root.join("second").join(format!("random-{index:03}.wav")),
            &wav,
        )
        .unwrap();
    }
    api(
        reqwest::Method::POST,
        "sources/refresh",
        json!({"id":sources[1]}),
    )
    .await;
    assert_eq!(
        api(
            reqwest::Method::POST,
            "queue/random",
            json!({"source":sources[1],"count":500,"mode":"replace"})
        )
        .await["empty"],
        false
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if api(reqwest::Method::GET, "queue", Value::Null).await["total"] == 150 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let tracks = api(
        reqwest::Method::GET,
        &format!("tracks?source={}", sources[1]),
        Value::Null,
    )
    .await;
    let home_route = format!("home?source={}&block=Explore", sources[1]);
    let home = api(reqwest::Method::GET, &home_route, Value::Null).await;
    assert_eq!(
        home,
        api(reqwest::Method::GET, &home_route, Value::Null).await
    );
    let uri = tracks["tracks"][0]["uri"].as_str().unwrap();
    assert_eq!(
        api(
            reqwest::Method::POST,
            "favorite",
            json!({"kind":"track","uri":uri,"favorite":true})
        )
        .await["favorite"],
        true
    );
    let metadata = client
        .get(format!("{base}/media"))
        .bearer_auth(token)
        .query(&[("uri", uri)])
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(metadata["favorite"], true);
    for include_current in [false, true] {
        let mut saved = settings.load();
        saved.clear_queue_includes_current = include_current;
        settings.save(&saved).unwrap();
        api(reqwest::Method::DELETE, "queue", Value::Null).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if api(reqwest::Method::GET, "queue", Value::Null).await["total"]
                    == if include_current { 0 } else { 1 }
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Clear follows the app's current-track setting");
    }

    for id in &sources {
        let response = client
            .delete(format!("{base}/sources"))
            .bearer_auth(token)
            .query(&[("id", id)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }
    assert!(products.source.list_sources().sources.is_empty());
    let mut stopped_native = products.playback.updates.subscribe();
    stopped_native.recv().await.unwrap();
    let mut stopped_http = client
        .get(format!("{base}/playback/events"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert!(!playback_event(&mut stopped_http).await.is_null());
    let transport = products.playback.transport.clone();
    tokio::task::spawn_blocking(move || transport.shutdown())
        .await
        .unwrap();
    while stopped_native.recv().await.unwrap().is_some() {}
    while !playback_event(&mut stopped_http).await.is_null() {}
    let stopped: Value = client
        .get(format!("{base}/playback"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(stopped.is_null());
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

async fn playback_event(response: &mut reqwest::Response) -> serde_json::Value {
    let mut data = Vec::new();
    loop {
        data.extend(response.chunk().await.unwrap().unwrap());
        let text = std::str::from_utf8(&data).unwrap();
        if let Some((event, _)) = text.split_once("\n\n") {
            let json = event
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap();
            return serde_json::from_str(json).unwrap();
        }
    }
}
