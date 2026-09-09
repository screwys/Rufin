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
            inputs.receivers.playback.close();
            inputs.receivers.visualizer.close();
            drop(inputs.receivers);
            inputs.products.playback.transport.shutdown();
        })
        .unwrap();
    }
}
