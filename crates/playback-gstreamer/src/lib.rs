use audio_processing::{connect_server_certificate_policy, ensure_gstreamer_initialized};
use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use playback::ResolvedStream;
use playback::*;
use std::collections::VecDeque;
use std::f64::consts::FRAC_PI_2;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, instrument, warn};

mod audio;
mod engine;
mod pipeline;
#[cfg(unix)]
mod process;
mod visualizer;

pub use audio::available_audio_outputs;
pub use engine::GStreamerPlaybackBackend;
#[cfg(unix)]
pub use process::restart_with_http1;

fn gstreamer_error_details(
    message: &gst::Message,
    stage: &str,
    audio_sink: Option<&str>,
) -> Option<String> {
    let gst::MessageView::Error(error) = message.view() else {
        return None;
    };
    let source = message
        .src()
        .map(|source| source.path_string().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let debug = error
        .debug()
        .map(|debug| debug.to_string())
        .unwrap_or_else(|| "unavailable".to_string());
    let audio_sink = audio_sink.unwrap_or("unconfigured");
    Some(format!(
        "GStreamer {stage} failed; element={source}; audio_sink={audio_sink}; error={}; debug={debug}",
        error.error()
    ))
}

const SEEK_SETTLE_WINDOW: Duration = Duration::from_millis(1_000);
const TRACK_START_SETTLE_WINDOW: Duration = Duration::from_millis(10_000);
const STARTUP_SEEK_SETTLE_WINDOW: Duration = Duration::from_millis(10_000);
const SEEK_POSITION_TOLERANCE_MILLIS: u64 = 1_500;

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
