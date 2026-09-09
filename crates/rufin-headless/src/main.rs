use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rufin_core::{app, diagnostics::Diagnostics, paths::Paths};

fn main() -> ExitCode {
    #[cfg(unix)]
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let root = PathBuf::from(
        arguments
            .next()
            .ok_or("Usage: rufin-headless PROFILE_DIRECTORY")?,
    );
    if arguments.next().is_some() {
        return Err("Usage: rufin-headless PROFILE_DIRECTORY".to_string());
    }
    let paths = Paths {
        config: root.join("config"),
        cache: root.join("cache"),
        data: root.join("data"),
        state: root.join("state"),
    };
    let settings = app::startup_settings(&paths);
    let (diagnostics, _stderr) = Diagnostics::install(paths.state_dir());
    app::with_runtime(|runtime| {
        let inputs = runtime.block_on(app::runtime_inputs(
            diagnostics,
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
        ))?;
        // An unattached presentation has no event backlog to retain.
        inputs.receivers.playback.close();
        inputs.receivers.visualizer.close();
        drop(inputs.receivers);
        let _ = writeln!(
            io::stdout().lock(),
            "Rufin is running without a UI. Press Ctrl+C to stop."
        );
        let result = runtime.block_on(wait_for_shutdown());
        inputs.products.playback.transport.shutdown();
        result.map_err(|error| error.to_string())
    })
    .map_err(|error| error.to_string())?
}

async fn wait_for_shutdown() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
