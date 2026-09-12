use std::env;
use std::io::{self, Write};
use std::process::{Command, ExitCode};

/// Establish libsoup's process setting before the host starts worker threads.
pub fn restart_with_http1() -> Option<ExitCode> {
    if env::var_os("SOUP_FORCE_HTTP1").is_some() {
        return None;
    }
    let executable = env::current_exe().ok()?;
    let mut command = Command::new(executable);
    command
        .args(env::args_os().skip(1))
        .env("SOUP_FORCE_HTTP1", "1");

    use std::os::unix::process::CommandExt as _;

    let error = command.exec();
    let _ = writeln!(
        io::stderr().lock(),
        "Could not enable GStreamer HTTP/1; continuing with the system default: {error}"
    );
    None
}
