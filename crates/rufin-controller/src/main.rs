use std::process::ExitCode;

fn main() -> ExitCode {
    rufin_controller::configure_allocator();
    if let Some(result) = rufin_controller::discovery_worker_argument() {
        return result;
    }
    #[cfg(unix)]
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    rufin_controller::run(std::env::args_os().skip(1))
}
