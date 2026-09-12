use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(unix)]
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    rufin_controller::run(std::env::args_os().skip(1))
}
