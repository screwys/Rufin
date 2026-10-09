use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    if !rufin_controller::configure_allocator() {
        let _ = writeln!(
            io::stderr().lock(),
            "Could not apply all system allocator settings"
        );
    }
    if let Some(result) = rufin_controller::discovery_worker_argument() {
        return result;
    }
    #[cfg(unix)]
    if let Some(result) = playback_gstreamer::restart_with_http1() {
        return result;
    }
    rufin_controller::run(std::env::args_os().skip(1))
}
