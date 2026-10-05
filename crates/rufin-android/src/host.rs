use std::sync::{Arc, Mutex, OnceLock};

use crate::media_session::{AndroidPlaybackSubscription, AndroidRepeatMode};
use crate::source_state::AndroidSourceSubscription;
use gstreamer::{self as gst, prelude::*};
use jni::{Env, objects::JObject};
use rufin_core::{
    app,
    diagnostics::{Diagnostics, StderrGuard},
    paths::Paths,
    runtime::{DiagnosticsHandle, ProductHandles},
};

// Diagnostics installs process-wide tracing and GLib handlers. A service may restart.
static DIAGNOSTICS: OnceLock<(DiagnosticsHandle, Mutex<StderrGuard>)> = OnceLock::new();

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
pub fn install_catalogs(catalogs: Vec<String>) -> Result<(), AndroidError> {
    localization::install_catalogs(&catalogs.iter().map(String::as_str).collect::<Vec<_>>())
        .map_err(error)
}

#[uniffi::export]
pub fn translate(message: String) -> String {
    localization::tr(&message)
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

/// The Android service retains this object independently of screen navigation.
#[derive(uniffi::Object)]
pub struct AndroidRuntime {
    products: ProductHandles,
    diagnostics: DiagnosticsHandle,
    player: Arc<crate::player::AndroidPlayer>,
    preferences: Arc<crate::preferences::AndroidPreferences>,
    more: Arc<crate::more::AndroidMore>,
    controller_owner: Arc<web::Controller>,
    controller: Arc<crate::controller::AndroidController>,
    source_setup: Arc<crate::source_setup::AndroidSourceSetup>,
    downloads: Arc<crate::downloads::AndroidDownloads>,
    library_events: Arc<crate::library_events::AndroidLibraryEvents>,
    runtime: Option<tokio::runtime::Runtime>,
}

#[uniffi::export]
impl AndroidRuntime {
    #[uniffi::constructor]
    pub fn new(files_dir: String, cache_dir: String) -> Result<Arc<Self>, AndroidError> {
        let files = std::path::PathBuf::from(files_dir);
        let paths = Paths {
            config: files.join("config"),
            data: files.join("data"),
            state: files.join("state"),
            cache: cache_dir.into(),
        };
        let settings = app::startup_settings(&paths);
        let theme_directory = paths.config_dir().join("themes");
        let (diagnostics, _) = DIAGNOSTICS.get_or_init(|| {
            let (diagnostics, guard) = Diagnostics::install(paths.state_dir());
            (diagnostics, Mutex::new(guard))
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
                paths,
                || {
                    playback_gstreamer::GStreamerPlaybackBackend::new()
                        .map(|backend| Box::new(backend) as Box<dyn playback::PlaybackBackend>)
                        .map_err(|error| error.to_string())
                },
                playback_gstreamer::available_audio_outputs,
                Arc::new(|_, _| {}),
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
        let more = Arc::new(crate::more::AndroidMore::new(
            &inputs.products,
            inputs.release_history,
            inputs.receivers.release_updates,
        ));
        let controller_owner = Arc::new(web::Controller::new(inputs.products.clone()));
        let source_setup = Arc::new(crate::source_setup::AndroidSourceSetup::new(
            &inputs.products,
            inputs.receivers.source_discovery,
        ));
        let downloads = Arc::new(crate::downloads::AndroidDownloads::new(
            &inputs.products,
            inputs.receivers.downloads,
        ));
        let library_events = Arc::new(crate::library_events::AndroidLibraryEvents::new(
            inputs.receivers.source,
        ));
        let controller = Arc::new(crate::controller::AndroidController::new(
            controller_owner.clone(),
            &inputs.products,
        ));
        Ok(Arc::new(Self {
            products: inputs.products,
            diagnostics: Arc::clone(diagnostics),
            player,
            preferences,
            more,
            controller_owner,
            controller,
            source_setup,
            downloads,
            library_events,
            runtime: Some(runtime),
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
    pub fn diagnostic_revision(&self) -> u64 {
        self.diagnostics.revision()
    }
    pub fn more(&self) -> Arc<crate::more::AndroidMore> {
        self.more.clone()
    }

    pub fn controller(&self) -> Arc<crate::controller::AndroidController> {
        self.controller.clone()
    }

    pub fn connect(&self) -> Arc<crate::connect::AndroidConnect> {
        Arc::new(crate::connect::AndroidConnect::new(&self.products))
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

    pub fn source_setup(&self) -> Arc<crate::source_setup::AndroidSourceSetup> {
        self.source_setup.clone()
    }
    pub fn downloads(&self) -> Arc<crate::downloads::AndroidDownloads> {
        self.downloads.clone()
    }

    pub fn has_document_root(&self, uri: String) -> Result<bool, AndroidError> {
        self.products
            .source
            .document_roots()
            .map(|roots| roots.iter().any(|root| root.uri == uri))
            .map_err(error)
    }

    pub fn backups(&self) -> Arc<crate::backups::AndroidBackups> {
        Arc::new(crate::backups::AndroidBackups::new(
            self.products.backup.clone(),
        ))
    }

    pub async fn set_download_document_directory(
        &self,
        source_id: String,
        uri: Option<String>,
    ) -> Result<(), AndroidError> {
        let settings = self.products.settings.clone();
        self.products
            .runtime
            .spawn_blocking(move || {
                settings.set_download_location(
                    sources::SourceId::new(source_id),
                    uri.map(|uri| downloads::DownloadDirectory::Document { uri }),
                )
            })
            .await
            .map_err(error)?
            .map_err(error)
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

    pub fn subscribe_sources(&self) -> Arc<AndroidSourceSubscription> {
        Arc::new(AndroidSourceSubscription::new(self.products.source.clone()))
    }

    pub fn subscribe_network(&self) -> Arc<crate::network::AndroidNetworkSubscription> {
        Arc::new(crate::network::AndroidNetworkSubscription::new(
            self.products.connect.subscribe(),
        ))
    }

    pub async fn refresh_source(&self, source_id: String) -> Result<(), AndroidError> {
        self.products
            .source
            .refresh_source(sources::SourceId::new(source_id))
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn open_uris(&self, uris: Vec<String>) -> Result<(), AndroidError> {
        let queue = self.products.playback.queue.clone();
        let database = self.products.library.clone();
        let local = self.products.settings.local_configuration();
        self.products
            .runtime
            .spawn(async move {
                rufin_core::open::arguments(
                    &queue,
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

    pub fn play(&self) {
        self.products.playback.transport.play();
    }
    pub fn pause(&self) {
        self.products.playback.transport.pause();
    }
    pub fn stop(&self) {
        self.products.playback.transport.stop();
    }
    pub fn next(&self) {
        self.products.playback.transport.next();
    }
    pub fn previous(&self) {
        self.products.playback.transport.previous();
    }
    pub fn seek_millis(&self, millis: u64) {
        self.products.playback.transport.seek_millis(millis);
    }
    pub fn set_volume(&self, volume: f64) {
        self.products.playback.transport.set_volume(volume);
        self.products.playback.transport.persist_volume(volume);
    }
    pub fn set_shuffle(&self, enabled: bool) {
        self.products.playback.transport.set_shuffle(enabled);
    }
    pub fn set_repeat(&self, mode: AndroidRepeatMode) {
        self.products.playback.transport.set_repeat(mode.into());
    }
}

impl Drop for AndroidRuntime {
    fn drop(&mut self) {
        self.controller_owner.stop();
        self.products.playback.transport.shutdown();
        self.products.scrobbling.shutdown();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        }
    }
}
