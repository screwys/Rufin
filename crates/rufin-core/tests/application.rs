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
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let projection = inputs.receivers.playback.recv().await.unwrap();
                        if projection.view.controls.muted {
                            break;
                        }
                    }
                })
                .await
                .expect("shared playback commands publish without GTK");
            });
            if !reopening {
                runtime.block_on(exercise_http_api(inputs.products.clone(), directory.path()));
            }
            inputs.receivers.playback.close();
            inputs.receivers.visualizer.close();
            drop(inputs.receivers);
            inputs.products.playback.transport.shutdown();
        })
        .unwrap();
    }
}

async fn exercise_http_api(products: rufin_core::runtime::ProductHandles, root: &std::path::Path) {
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
    let mut native = products.playback.state.clone();
    native.borrow_and_update();
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
    native.changed().await.unwrap();
    assert!(!native.borrow().as_ref().unwrap().controls.muted);
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
            if native
                .borrow_and_update()
                .as_ref()
                .is_some_and(|state| state.queue.total == 2)
            {
                break;
            }
            native.changed().await.unwrap();
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
