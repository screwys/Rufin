use std::io::{self, Write};
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use rufin_core::{app, diagnostics::Diagnostics, paths};

pub fn run(arguments: impl Iterator<Item = std::ffi::OsString>) -> ExitCode {
    match run_inner(arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    localization::initialize()?;
    const USAGE: &str = "Usage: rufin --headless [--listen [ADDRESS:PORT]]\n       rufin-controller [--listen [ADDRESS:PORT]]\nTo enable web and API access, pass --listen and set RUFIN_API_TOKEN.\nDefault address: 127.0.0.1:1717. GET /api lists the available commands.";
    let address: Option<SocketAddr> = match arguments.next() {
        Some(option) if option == "--help" => {
            let _ = writeln!(io::stdout().lock(), "{USAGE}");
            return Ok(());
        }
        Some(option) if option == "--listen" => Some(
            arguments
                .next()
                .unwrap_or_else(|| "127.0.0.1:1717".into())
                .to_str()
                .ok_or(USAGE)?
                .parse()
                .map_err(|error| format!("Invalid listen address: {error}"))?,
        ),
        Some(_) => return Err(USAGE.into()),
        None => None,
    };
    if arguments.next().is_some() {
        return Err(USAGE.to_string());
    }
    let token = if address.is_some() {
        Some(
            std::env::var("RUFIN_API_TOKEN")
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or("Set RUFIN_API_TOKEN before enabling HTTP access")?,
        )
    } else {
        None
    };
    let paths = paths::roots();
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
        inputs.receivers.visualizer.close();
        drop(inputs.receivers);
        let result = runtime.block_on(async {
            if let Some(address) = address {
                let listener = tokio::net::TcpListener::bind(address).await?;
                let _ = writeln!(io::stdout().lock(), "Rufin API listening on http://{}", listener.local_addr()?);
                tokio::select! {
                    result = rufin_core::api::serve(listener, inputs.products.clone(), token.unwrap()) => result,
                    result = wait_for_shutdown() => result,
                }
            } else {
                let _ = writeln!(io::stdout().lock(), "Rufin is running without a UI. Press Ctrl+C to stop.");
                wait_for_shutdown().await
            }
        });
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
