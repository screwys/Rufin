use std::sync::{Arc, Mutex, OnceLock};

use crate::media_session::AndroidPlaybackSubscription;
use gstreamer::{self as gst, prelude::*};
use jni::{Env, objects::JObject};
use rufin_core::{
    app,
    diagnostics::{Diagnostics, StderrGuard},
    paths::Paths,
    runtime::{DiagnosticsHandle, ProductHandles},
};

// Diagnostics installs process-wide tracing and GLib handlers. A service may restart.
static DIAGNOSTICS: OnceLock<(DiagnosticsHandle, Mutex<Option<StderrGuard>>)> = OnceLock::new();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AndroidError {
    #[error("{reason}")]
    Failure { reason: String },
}

pub(crate) fn error(value: impl ToString) -> AndroidError {
    AndroidError::Failure {
        reason: value.to_string(),
    }
}

#[allow(
    deprecated,
    reason = "jni 0.22.4 generates AtomicBool::fetch_update calls"
)]
const _: jni::NativeMethod = jni::native_method! {
    java_type = "io.github.screwys.rufin.NativeHost",
    extern fn configure_gstreamer() -> void,
};

fn configure_gstreamer<'local>(
    env: &mut Env<'local>,
    _this: JObject<'local>,
) -> jni::errors::Result<()> {
    gst::init().or_else(|error| env.throw(error.to_string()))?;
    crate::audio::register(env)?;
    // Use the bundled primary audio decoders before Android's MediaCodec adapters.
    for decoder in gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_AUDIO,
        gst::Rank::PRIMARY,
    ) {
        if decoder.plugin_name().as_deref() != Some("androidmedia") {
            decoder.set_rank(decoder.rank() + 2);
        }
    }
    Ok(())
}

#[allow(
    deprecated,
    reason = "jni 0.22.4 generates AtomicBool::fetch_update calls"
)]
const _: jni::NativeMethod = jni::native_method! {
    java_type = "io.github.screwys.rufin.NativeHost",
    extern fn initialize(context: android.content.Context, documents: io.github.screwys.rufin.platform.AndroidDocuments) -> void,
};

fn initialize<'local>(
    env: &mut Env<'local>,
    _this: JObject<'local>,
    context: JObject<'local>,
    documents: JObject<'local>,
) -> jni::errors::Result<()> {
    secrets::android::initialize(env, &context)?;
    crate::discovery::initialize(env, &context)?;
    crate::documents::initialize(env, &documents)?;
    crate::network::initialize(env, &context)?;
    rustls_platform_verifier::android::init_with_env(env, context)
}

#[uniffi::export]
pub fn install_catalogs(
    catalogs: Vec<String>,
) -> Result<std::collections::HashMap<String, String>, AndroidError> {
    localization::install_catalogs(&catalogs.iter().map(String::as_str).collect::<Vec<_>>())
        .map_err(error)
}

#[uniffi::export]
pub fn album_count_text(count: u64) -> String {
    localization::album_count_text(count)
}

#[uniffi::export]
pub fn track_count_text(count: u64) -> String {
    localization::track_count_text(count)
}

#[uniffi::export]
pub fn seek_preview_matches_position(target_millis: u64, position_millis: u64) -> bool {
    rufin_core::playback::seek_preview_matches_position(target_millis, position_millis)
}

fn android_paths(files_dir: String, cache_dir: String) -> Paths {
    let files = std::path::PathBuf::from(files_dir);
    Paths {
        config: files.join("config"),
        data: files.join("data"),
        state: files.join("state"),
        cache: cache_dir.into(),
    }
}

#[uniffi::export]
pub fn reset_database(files_dir: String, cache_dir: String) -> Result<(), AndroidError> {
    rufin_core::settings::StorageResetRequest::Database
        .apply(&android_paths(files_dir, cache_dir))
        .map_err(error)
}

/// The Android service retains this object independently of screen navigation.
#[derive(uniffi::Object)]
pub struct AndroidRuntime {
    pub(crate) products: ProductHandles,
    diagnostics: DiagnosticsHandle,
    player: Arc<crate::player::AndroidPlayer>,
    preferences: Arc<crate::preferences::AndroidPreferences>,
    more: Arc<crate::more::AndroidMore>,
    controller_owner: Arc<web::Controller>,
    release_events: async_channel::Receiver<rufin_core::runtime::ReleaseUpdate>,
    library_events: Arc<crate::library_events::AndroidLibraryEvents>,
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    paths: Paths,
}

#[uniffi::export]
impl AndroidRuntime {
    #[uniffi::constructor]
    pub fn new(files_dir: String, cache_dir: String) -> Result<Arc<Self>, AndroidError> {
        let paths = android_paths(files_dir, cache_dir);
        let settings = app::startup_settings(&paths);
        let theme_directory = paths.config_dir().join("themes");
        let (diagnostics, _) = DIAGNOSTICS.get_or_init(|| {
            let (diagnostics, guard) = Diagnostics::install(paths.state_dir());
            (diagnostics, Mutex::new(Some(guard)))
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rufin-async")
            .build()
            .map_err(error)?;
        let inputs = runtime
            .block_on(app::runtime_inputs(
                Arc::clone(diagnostics),
                false,
                settings,
                paths.clone(),
                |processing| {
                    playback_gstreamer::GStreamerPlaybackBackend::new(processing)
                        .map(|backend| Box::new(backend) as Box<dyn playback::PlaybackBackend>)
                        .map_err(|error| error.to_string())
                },
                playback_gstreamer::available_audio_outputs,
                Arc::new(|_| {}),
                Arc::new(|_, _| {}),
            ))
            .map_err(error)?;
        let player = Arc::new(crate::player::AndroidPlayer::new(
            &inputs.products,
            inputs.receivers.visualizer,
            inputs.receivers.lyrics,
            inputs.receivers.waveform,
        ));
        let preferences = Arc::new(crate::preferences::AndroidPreferences::new(
            &inputs.products,
            theme_directory,
        ));
        let more = Arc::new(crate::more::AndroidMore::new(&inputs.products));
        let controller_owner = Arc::new(web::Controller::new(inputs.products.clone()));
        let library_events = Arc::new(crate::library_events::AndroidLibraryEvents::new(
            inputs.receivers.source,
            inputs.products.settings.persistence_errors(),
        ));
        Ok(Arc::new(Self {
            products: inputs.products,
            diagnostics: Arc::clone(diagnostics),
            player,
            preferences,
            more,
            controller_owner,
            release_events: inputs.receivers.release_updates,
            library_events,
            runtime: Mutex::new(Some(runtime)),
            paths,
        }))
    }

    pub fn application_name(&self) -> String {
        app_identity::DISPLAY_NAME.to_string()
    }

    pub fn debug_logging(&self) -> bool {
        self.diagnostics.debug_enabled()
    }

    pub fn set_debug_logging(&self, enabled: bool) -> Result<(), AndroidError> {
        self.diagnostics.set_debug_enabled(enabled).map_err(error)
    }

    pub fn diagnostic_log(&self) -> String {
        self.diagnostics.snapshot()
    }
    pub fn diagnostic_updates(&self, cursor: u64) -> rufin_core::diagnostics::DiagnosticUpdate {
        self.diagnostics.updates(cursor)
    }
    pub fn more(&self) -> Arc<crate::more::AndroidMore> {
        self.more.clone()
    }

    pub fn controller(&self) -> Arc<web::Controller> {
        self.controller_owner.clone()
    }

    pub fn connect(&self) -> Arc<rufin_core::connect::ConnectOwner> {
        self.products.connect.clone()
    }

    pub fn library(&self) -> Arc<crate::library::AndroidLibrary> {
        Arc::new(crate::library::AndroidLibrary::new(&self.products))
    }

    pub fn library_events(&self) -> Arc<crate::library_events::AndroidLibraryEvents> {
        self.library_events.clone()
    }

    pub fn player(&self) -> Arc<crate::player::AndroidPlayer> {
        Arc::clone(&self.player)
    }

    pub fn preferences(&self) -> Arc<crate::preferences::AndroidPreferences> {
        Arc::clone(&self.preferences)
    }

    pub fn source(&self) -> Arc<rufin_core::source::SourceOwner> {
        self.products.source.clone()
    }
    pub fn storage(self: Arc<Self>) -> Arc<crate::storage::AndroidStorage> {
        Arc::new(crate::storage::AndroidStorage::new(self))
    }

    pub fn downloads(&self) -> Arc<downloads::Downloads> {
        Arc::new(self.products.downloads.clone())
    }

    pub fn has_document_root(&self, uri: String) -> Result<bool, AndroidError> {
        self.products
            .source
            .document_roots()
            .map(|roots| roots.iter().any(|root| root.uri == uri))
            .map_err(error)
    }

    pub fn backups(&self) -> rufin_core::runtime::BackupHandle {
        self.products.backup.clone()
    }

    pub async fn add_document_root(&self, uri: String) -> Result<(), AndroidError> {
        let root = self
            .products
            .runtime
            .spawn_blocking(move || sources::document_root(&uri))
            .await
            .map_err(error)?
            .map_err(error)?;
        self.products
            .source
            .add_document_roots(vec![root])
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn subscribe_playback(&self) -> Arc<AndroidPlaybackSubscription> {
        Arc::new(AndroidPlaybackSubscription::new(
            &self.products,
            self.library_events.intents(),
        ))
    }

    pub fn subscribe_network(&self) -> Arc<crate::network::AndroidNetworkSubscription> {
        Arc::new(crate::network::AndroidNetworkSubscription::new(
            self.products.connect.subscribe(),
        ))
    }

    pub async fn open_uris(&self, uris: Vec<String>) -> Result<(), AndroidError> {
        let queue = self.products.playback.clone();
        let database = self.products.library.clone();
        let local = self.products.settings.local_configuration();
        self.products
            .runtime
            .spawn(async move {
                rufin_core::open::arguments(
                    queue,
                    &database,
                    local.as_ref(),
                    uris.into_iter().map(Into::into).collect(),
                )
                .await
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn settings(&self) -> rufin_core::SettingsHandle {
        self.products.settings.clone()
    }
    pub fn scrobbling(&self) -> rufin_core::runtime::ScrobblingHandle {
        self.products.scrobbling.clone()
    }
    pub fn release_updates(&self) -> rufin_core::runtime::ReleaseUpdateHandle {
        self.products.release_updates.clone()
    }
    pub fn release_events(&self) -> Arc<rufin_core::runtime::release_update::ReleaseSubscription> {
        rufin_core::runtime::release_update::ReleaseSubscription::new(
            self.products.release_updates.initial_history(),
            self.release_events.clone(),
        )
    }
    pub fn playback_owner(&self) -> Arc<rufin_core::playback::PlaybackOwner> {
        self.products.playback.clone()
    }
    pub fn lyrics_owner(&self) -> Arc<rufin_core::lyrics::LyricsHandle> {
        Arc::new(self.products.lyrics.clone())
    }
}

impl AndroidRuntime {
    pub(crate) fn reset_storage(
        &self,
        kind: rufin_core::settings::StorageReset,
    ) -> Result<(), AndroidError> {
        let request = self.products.prepare_storage_reset(kind).map_err(error)?;
        self.controller_owner.stop();
        self.products.playback.shutdown();
        self.products.scrobbling.shutdown();
        let runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .ok_or_else(|| error("Rufin is shutting down"))?;
        let closed = runtime.block_on(self.products.close_storage());
        // File workers must finish before the reset can remove their output.
        drop(runtime);
        closed.map_err(error)?;
        if let Some((_, guard)) = DIAGNOSTICS.get() {
            guard.lock().unwrap_or_else(|p| p.into_inner()).take();
        }
        request.apply(&self.paths).map_err(error)
    }
}

impl Drop for AndroidRuntime {
    fn drop(&mut self) {
        self.controller_owner.stop();
        self.products.playback.shutdown();
        self.products.scrobbling.shutdown();
        if let Err(error) = self.products.settings.flush() {
            tracing::warn!(%error, "could not finish saving settings");
        }
        if let Some(runtime) = self
            .runtime
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        }
    }
}
