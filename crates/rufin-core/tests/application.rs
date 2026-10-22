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
    std::fs::create_dir_all(paths.store_file().parent().unwrap()).unwrap();
    std::fs::write(paths.store_file(), b"unreadable database").unwrap();
    app::with_runtime(|runtime| {
        let opened = runtime.block_on(app::runtime_inputs(
            diagnostics.clone(),
            false,
            app::startup_settings(&paths),
            paths.clone(),
            |_| Ok(Box::new(IdleBackend) as Box<dyn PlaybackBackend>),
            Vec::new,
            Arc::new(|_| {}),
            Arc::new(|_, _| {}),
        ));
        assert!(opened.is_err());
        assert_eq!(
            std::fs::read(paths.store_file()).unwrap(),
            b"unreadable database"
        );
    })
    .unwrap();
    let settings = std::fs::read(paths.settings_file()).unwrap();
    let artwork = paths.artwork_dir().join("keep.img");
    std::fs::create_dir_all(artwork.parent().unwrap()).unwrap();
    std::fs::write(&artwork, b"cached artwork").unwrap();
    for path in [paths.legacy_store_file(), paths.catalog_file()] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"old database").unwrap();
    }
    rufin_core::settings::StorageResetRequest::Database
        .apply(&paths)
        .unwrap();
    assert!(!paths.store_file().exists());
    assert!(!paths.legacy_store_file().exists());
    assert!(!paths.catalog_file().exists());
    assert_eq!(std::fs::read(paths.settings_file()).unwrap(), settings);
    assert_eq!(std::fs::read(&artwork).unwrap(), b"cached artwork");
    for reopening in [false, true] {
        app::with_runtime(|runtime| {
            let inputs = runtime
                .block_on(runtime.spawn(app::runtime_inputs(
                    diagnostics.clone(),
                    false,
                    app::startup_settings(&paths),
                    paths.clone(),
                    |_| Ok(Box::new(IdleBackend) as Box<dyn PlaybackBackend>),
                    Vec::new,
                    Arc::new(|_| {}),
                    Arc::new(|_, _| {}),
                )))
                .unwrap()
                .unwrap();
            assert!(!inputs.temporary_store);
            assert_eq!(inputs.settings.load().application.private_mode, reopening);
            let settings = inputs.settings.clone();
            runtime
                .block_on(runtime.spawn_blocking(move || {
                    let mut saved = settings.load();
                    saved.application.private_mode = true;
                    settings.save(&saved).unwrap();
                }))
                .unwrap();
            inputs.products.playback.set_muted(true);
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
            inputs.receivers.visualizer.close();
            drop(inputs.receivers);
            inputs.products.playback.shutdown();
        })
        .unwrap();
    }
}
