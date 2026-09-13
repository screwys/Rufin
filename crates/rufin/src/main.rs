#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use rufin_core::{app, diagnostics, paths};
use std::env;
use std::ffi::OsStr;
use std::io::{self, Write};
use std::process::{Command, ExitCode};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::info;

const UPDATED_RESTART_VERSION_ENV: &str = "RUFIN_UPDATED_RESTART_VERSION";

fn main() -> ExitCode {
    let headless = env::args_os().nth(1).as_deref() == Some(OsStr::new("--headless"));
    #[cfg(target_os = "windows")]
    if headless {
        desktop_integration::Platform::attach_parent_console();
    }
    #[cfg(target_os = "macos")]
    if let Some(result) = restart_in_macos_bundle() {
        return result;
    }
    #[cfg(unix)]
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    if let Some(result) = discovery_worker_argument() {
        return result;
    }
    let updated_restart = match updated_restart_argument() {
        Some(Ok(())) => true,
        Some(Err(message)) => {
            let _ = writeln!(io::stderr().lock(), "{message}");
            return ExitCode::FAILURE;
        }
        None => false,
    };
    let settings = app::startup_settings(&paths::roots());
    if let Some(result) = restart_with_language(&settings.load().ui.language, updated_restart) {
        return result;
    }
    if headless {
        let _desktop_platform = desktop_integration::Platform::initialize();
        return rufin_controller::run(env::args_os().skip(2));
    }
    if let Err(error) = localization::initialize() {
        let _ = writeln!(io::stderr().lock(), "Could not initialize gettext: {error}");
    }
    let (diagnostics, _stderr_guard) = diagnostics::Diagnostics::install(paths::state_dir());
    let _desktop_platform = desktop_integration::Platform::initialize();
    info!("starting Rufin native shell");

    let initial_settings = settings.load().ui.clone();
    let bootstrap = move || async move {
        let discord = Arc::new(desktop_integration::Discord::new());
        let presence = Arc::clone(&discord);
        app::runtime_inputs(
            diagnostics,
            !updated_restart,
            settings,
            paths::roots(),
            || {
                playback_gstreamer::GStreamerPlaybackBackend::new()
                    .map(|backend| Box::new(backend) as Box<dyn playback::PlaybackBackend>)
                    .map_err(|error| error.to_string())
            },
            playback_gstreamer::available_audio_outputs,
            Arc::new(move |projection, discontinuity| {
                presence.observe(projection.map(|p| &p.view), discontinuity)
            }),
            Arc::new(move |settings, projection| {
                discord.update(
                    settings.rich_presence.clone(),
                    !settings.private_mode,
                    &settings.lastfm_api_key,
                    projection.map(|p| &p.view),
                )
            }),
        )
        .await
    };
    match app::with_runtime(|_| {
        if updated_restart {
            ui_shell::run_application_after_update(initial_settings, bootstrap, || {})
        } else {
            ui_shell::run_application(initial_settings, bootstrap)
        }
    }) {
        Ok(exit) => exit,
        Err(error) => ui_shell::run_startup_error_application(error.to_string()),
    }
}

fn restart_with_language(saved_language: &str, updated_restart: bool) -> Option<ExitCode> {
    let language = localization::process_language(saved_language)?;
    if env::var("LANGUAGE").ok().as_deref() == Some(language.as_str()) {
        return None;
    }
    let executable = env::current_exe().ok()?;
    let mut command = Command::new(executable);
    command
        .args(env::args_os().skip(1))
        .env("LANGUAGE", language);
    if updated_restart {
        // The helper has already acknowledged this version before the language relaunch.
        command.env(UPDATED_RESTART_VERSION_ENV, env!("CARGO_PKG_VERSION"));
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        let error = command.exec();
        let _ = writeln!(
            io::stderr().lock(),
            "Could not select Rufin language: {error}"
        );
        Some(ExitCode::FAILURE)
    }
    #[cfg(not(unix))]
    {
        match command.spawn() {
            Ok(mut child) => {
                if env::args_os().nth(1).as_deref() == Some(OsStr::new("--headless")) {
                    return Some(match child.wait() {
                        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
                        Err(error) => {
                            let _ =
                                writeln!(io::stderr().lock(), "Could not wait for Rufin: {error}");
                            ExitCode::FAILURE
                        }
                    });
                }
                #[cfg(target_os = "windows")]
                if updated_restart
                    && let Err(error) =
                        desktop_integration::Platform::allow_foreground_activation(child.id())
                {
                    let _ = writeln!(io::stderr().lock(), "{error}");
                }
                Some(ExitCode::SUCCESS)
            }
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "Could not select Rufin language: {error}"
                );
                Some(ExitCode::FAILURE)
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn restart_in_macos_bundle() -> Option<ExitCode> {
    const BUNDLE_ENVIRONMENT_READY: &str = "RUFIN_MACOS_BUNDLE_ENVIRONMENT_READY";

    if env::var_os(BUNDLE_ENVIRONMENT_READY).is_some() {
        return None;
    }
    let executable = env::current_exe().ok()?;
    let macos_dir = executable.parent()?;
    let contents_dir = macos_dir.parent()?;
    if macos_dir.file_name() != Some(OsStr::new("MacOS"))
        || contents_dir.file_name() != Some(OsStr::new("Contents"))
    {
        return None;
    }
    let resources_dir = contents_dir.join("Resources");
    let frameworks_dir = contents_dir.join("Frameworks");
    if !resources_dir.is_dir() || !frameworks_dir.is_dir() {
        return None;
    }

    Some(
        match macos_bundle_command(&executable, macos_dir, &resources_dir, &frameworks_dir) {
            Ok(mut command) => {
                use std::os::unix::process::CommandExt as _;

                let error = command.exec();
                let _ = writeln!(io::stderr().lock(), "Could not start Rufin: {error}");
                ExitCode::FAILURE
            }
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "Could not prepare Rufin: {error}");
                ExitCode::FAILURE
            }
        },
    )
}

#[cfg(target_os = "macos")]
fn macos_bundle_command(
    executable: &Path,
    macos_dir: &Path,
    resources_dir: &Path,
    frameworks_dir: &Path,
) -> Result<Command, String> {
    let loader_dir = resources_dir.join("lib/gdk-pixbuf-2.0/loaders");
    let loader_cache = paths::project_cache_dir()
        .unwrap_or_else(|| env::temp_dir().join(app_identity::APP_ID))
        .join("gdk-pixbuf-loaders.cache");
    if let Some(parent) = loader_cache.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not prepare the image loader cache: {error}"))?;
    }
    let mut loader_modules = fs::read_dir(&loader_dir)
        .map_err(|error| format!("could not read image loaders: {error}"))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension() == Some(OsStr::new("so")))
        .collect::<Vec<_>>();
    loader_modules.sort();
    if loader_modules.is_empty() {
        return Err("no bundled image loaders were found".to_string());
    }
    let loader_output = Command::new(macos_dir.join("gdk-pixbuf-query-loaders"))
        .args(loader_modules)
        .output()
        .map_err(|error| format!("could not inspect image loaders: {error}"))?;
    if !loader_output.status.success() {
        return Err(format!(
            "image loader inspection failed with status {}",
            loader_output.status
        ));
    }
    fs::write(&loader_cache, loader_output.stdout)
        .map_err(|error| format!("could not write the image loader cache: {error}"))?;

    let registry_path = env::var_os("RUFIN_GST_REGISTRY_1_0")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let registry_file = format!(
                "gstreamer-registry-{}.bin",
                env::consts::ARCH.replace("aarch64", "arm64")
            );
            paths::project_cache_dir()
                .map(|cache_dir| cache_dir.join(&registry_file))
                .unwrap_or_else(|| env::temp_dir().join(registry_file))
        });
    let mut command = Command::new(executable);
    command
        .args(env::args_os().skip(1))
        .env("RUFIN_MACOS_BUNDLE_ENVIRONMENT_READY", "1")
        .env("GDK_PIXBUF_MODULEDIR", &loader_dir)
        .env("GDK_PIXBUF_MODULE_FILE", loader_cache)
        .env("GIO_MODULE_DIR", resources_dir.join("lib/gio/modules"))
        .env(
            "GSETTINGS_SCHEMA_DIR",
            resources_dir.join("share/glib-2.0/schemas"),
        )
        .env(
            "GST_PLUGIN_SCANNER_1_0",
            macos_dir.join("gst-plugin-scanner"),
        )
        .env("GST_PLUGIN_PATH", "")
        .env("GST_PLUGIN_PATH_1_0", "")
        .env(
            "GST_PLUGIN_SYSTEM_PATH_1_0",
            resources_dir.join("lib/gstreamer-1.0"),
        )
        .env("RUFIN_LOCALEDIR", resources_dir.join("share/locale"))
        .env("XDG_DATA_DIRS", resources_dir.join("share"));
    command.env("SOUP_FORCE_HTTP1", "1");
    if registry_path
        .parent()
        .is_some_and(|parent| fs::create_dir_all(parent).is_ok())
    {
        command.env("GST_REGISTRY_1_0", registry_path);
    }
    let library_path = match env::var_os("DYLD_LIBRARY_PATH") {
        Some(existing) if !existing.is_empty() => {
            let mut paths = vec![frameworks_dir.to_path_buf()];
            paths.extend(env::split_paths(&existing));
            env::join_paths(paths)
                .map_err(|error| format!("could not prepare the library path: {error}"))?
        }
        _ => frameworks_dir.as_os_str().to_owned(),
    };
    command.env("DYLD_LIBRARY_PATH", library_path);
    Ok(command)
}

#[cfg(target_os = "windows")]
fn updated_restart_argument() -> Option<Result<(), String>> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new("--updated-restart")) {
        return None;
    }
    Some((|| {
        let version = arguments
            .next()
            .ok_or("Usage: rufin --updated-restart VERSION")?;
        if arguments.next().is_some() {
            return Err("Usage: rufin --updated-restart VERSION".to_string());
        }
        if version != OsStr::new(env!("CARGO_PKG_VERSION")) {
            return Err(
                "The reopened Rufin version does not match the installed update.".to_string(),
            );
        }
        if env::var_os(UPDATED_RESTART_VERSION_ENV).as_deref() == Some(version.as_os_str()) {
            Ok(())
        } else {
            windows_updater::wait_for_updated_restart()
        }
    })())
}

#[cfg(not(target_os = "windows"))]
fn updated_restart_argument() -> Option<Result<(), String>> {
    None
}

fn discovery_worker_argument() -> Option<ExitCode> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new("--discovery-worker")) {
        return None;
    }
    let result = (|| {
        let timeout = arguments
            .next()
            .and_then(|value| value.to_str().and_then(|value| value.parse::<u64>().ok()))
            .ok_or("Usage: rufin --discovery-worker TIMEOUT_SECONDS".to_string())?;
        if arguments.next().is_some() {
            return Err("Usage: rufin --discovery-worker TIMEOUT_SECONDS".to_string());
        }
        sources::run_discovery_worker(timeout).map_err(|error| error.to_string())
    })();
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    })
}
