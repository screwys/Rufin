use audio_processing::ResolvedStream;
use audio_processing::{configure_sources, ensure_gstreamer_initialized};
use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use playback::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, instrument, warn};

mod audio;
mod engine;
mod file_audio;
mod pipeline;
#[cfg(unix)]
mod process;
mod visualizer;

pub use audio::available_audio_outputs;
pub use engine::GStreamerPlaybackBackend;
#[cfg(unix)]
pub use process::restart_with_http1;

fn gstreamer_playback_error(
    message: &gst::Message,
    stage: &str,
    audio_sink: Option<&str>,
) -> Option<BackendFailure> {
    let gst::MessageView::Error(error) = message.view() else {
        return None;
    };
    let source = message
        .src()
        .map(|source| source.path_string().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let details = error
        .debug()
        .map(|debug| debug.to_string())
        .unwrap_or_else(|| "unavailable".to_string());
    let audio_sink = audio_sink.unwrap_or("unconfigured");
    let cause = error.error();
    let kind = failure_kind(
        &cause,
        message
            .src()
            .and_then(|source| source.downcast_ref::<gst::Element>()),
    );
    error!(stage, element = %source, audio_sink, error = %cause, ?kind, debug = %details, "GStreamer playback failed");
    Some(BackendFailure::with_kind(kind, kind.summary()))
}

fn failure_kind(error: &glib::Error, element: Option<&gst::Element>) -> FailureKind {
    let class = element
        .and_then(|element| element.factory())
        .map(|factory| factory.klass().to_string())
        .unwrap_or_default();
    let has_class = |name| class.split('/').any(|part| part == name);
    if has_class("Sink") {
        return FailureKind::Output;
    }
    if let Some(error) = error.kind::<gst::ResourceError>() {
        return match error {
            gst::ResourceError::NotFound => FailureKind::UnavailableMedia,
            gst::ResourceError::NotAuthorized => FailureKind::DeniedAccess,
            _ if has_class("Network") => FailureKind::Connection,
            gst::ResourceError::OpenRead | gst::ResourceError::Read => {
                FailureKind::UnavailableMedia
            }
            _ => FailureKind::Unclassified,
        };
    }
    if let Some(error) = error.kind::<gst::StreamError>() {
        return match error {
            gst::StreamError::CodecNotFound
            | gst::StreamError::TypeNotFound
            | gst::StreamError::WrongType
            | gst::StreamError::Format => FailureKind::UnsupportedFormat,
            _ => FailureKind::Decode,
        };
    }
    if error.kind::<gst::CoreError>() == Some(gst::CoreError::MissingPlugin) {
        return FailureKind::UnsupportedFormat;
    }
    FailureKind::Unclassified
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_errors_choose_skip_or_stop() {
        ensure_gstreamer_initialized().unwrap();
        let element = |factory| gst::ElementFactory::make(factory).build().unwrap();
        let file = element("filesrc");
        let sink = element("fakesink");
        let cases = [
            (
                glib::Error::new(gst::ResourceError::NotFound, "missing"),
                Some(&file),
                FailureKind::UnavailableMedia,
            ),
            (
                glib::Error::new(gst::ResourceError::NotAuthorized, "denied"),
                Some(&file),
                FailureKind::DeniedAccess,
            ),
            (
                glib::Error::new(gst::ResourceError::Write, "device lost"),
                Some(&sink),
                FailureKind::Output,
            ),
            (
                glib::Error::new(gst::StreamError::CodecNotFound, "no decoder"),
                None,
                FailureKind::UnsupportedFormat,
            ),
            (
                glib::Error::new(gst::StreamError::Decode, "corrupt"),
                None,
                FailureKind::Decode,
            ),
            (
                glib::Error::new(gst::CoreError::StateChange, "state"),
                None,
                FailureKind::Unclassified,
            ),
        ];
        for (error, element, kind) in cases {
            assert_eq!(failure_kind(&error, element), kind, "{error}");
        }
    }
}
