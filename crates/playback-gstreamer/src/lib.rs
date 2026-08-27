use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use playback::ResolvedStream;
use playback::*;
use std::collections::VecDeque;
use std::f64::consts::FRAC_PI_2;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, instrument, warn};

mod audio;
mod engine;
mod loudness;
mod pipeline;
mod transcode;
mod waveform;

pub use audio::available_audio_outputs;
pub use engine::GStreamerPlaybackBackend;
pub use loudness::{
    AnalyzedLoudness, LoudnessAnalysis, album_loudness, analyze_loudness_cancellable,
};
pub use transcode::TranscodedAudioReader;
pub use waveform::generate_waveform_peaks_cancellable;

pub fn verify_audio_file(path: &Path) -> Result<(), String> {
    ensure_gstreamer_initialized()?;
    if gst::ElementFactory::find("souphttpsrc").is_none() {
        return Err("GStreamer HTTP playback support (souphttpsrc) is unavailable".to_string());
    }
    let uri = glib::filename_to_uri(path, None).map_err(|error| error.to_string())?;
    let pipeline = pipeline::make_playbin("rufin-media-verification")?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| "GStreamer media verification has no message bus".to_string())?;
    let video_sink = gst::ElementFactory::make("fakesink")
        .name("rufin-verification-video-output")
        .build()
        .map_err(|error| error.to_string())?;
    let settings = BackendAudioSettings {
        audio_output: Some("fakesink".to_string()),
        ..BackendAudioSettings::default()
    };
    let audio_graph = audio::AudioGraph::new(&settings, DEFAULT_PLAYBACK_RATE)?;
    let decoded_audio_buffers = Arc::new(AtomicUsize::new(0));
    let decoded_audio_buffers_for_probe = Arc::clone(&decoded_audio_buffers);
    audio_graph
        .root()
        .static_pad("sink")
        .ok_or_else(|| "GStreamer media verification audio chain has no input pad".to_string())?
        .add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |_, _| {
                decoded_audio_buffers_for_probe.fetch_add(1, Ordering::Relaxed);
                gst::PadProbeReturn::Ok
            },
        );
    pipeline::configure_playbin_for_audio(&pipeline);
    pipeline.set_property("video-sink", &video_sink);
    pipeline.set_property("audio-sink", audio_graph.root());
    pipeline.set_property("uri", uri.as_str());

    let result = match pipeline.set_state(gst::State::Playing) {
        Err(error) => Err(bus
            .pop_filtered(&[gst::MessageType::Error])
            .and_then(|message| {
                gstreamer_error_details(&message, "media verification startup", Some("fakesink"))
            })
            .unwrap_or_else(|| format!("GStreamer media verification could not start: {error}"))),
        Ok(_) => match bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(30),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(message) if matches!(message.view(), gst::MessageView::Eos(_)) => {
                verified_eos(decoded_audio_buffers.load(Ordering::Relaxed))
            }
            Some(message) => {
                Err(
                    gstreamer_error_details(&message, "media verification", Some("fakesink"))
                        .unwrap_or_else(|| "GStreamer media verification failed".to_string()),
                )
            }
            None => {
                Err("GStreamer media verification did not finish within 30 seconds".to_string())
            }
        },
    };
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn verified_eos(decoded_audio_buffers: usize) -> Result<(), String> {
    if decoded_audio_buffers == 0 {
        return Err(
            "GStreamer media verification reached end of stream without decoded audio".to_string(),
        );
    }
    Ok(())
}

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

/// Initialize GStreamer once before playback or waveform work starts.
fn ensure_gstreamer_initialized() -> Result<(), String> {
    static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();
    INITIALIZED
        .get_or_init(|| gst::init().map_err(|error| error.to_string()))
        .clone()
}

fn connect_server_certificate_policy(
    element: &gst::Element,
    trust_invalid_certificate: impl Fn() -> bool + Send + Sync + 'static,
) {
    let _ = element.connect("source-setup", false, move |values| {
        if let Some(source) = values
            .get(1)
            .and_then(|value| value.get::<gst::Element>().ok())
        {
            apply_server_certificate_policy(&source, trust_invalid_certificate());
        }
        None
    });
}

fn apply_server_certificate_policy(source: &gst::Element, trust_invalid_certificate: bool) {
    if source.find_property("ssl-strict").is_some() {
        source.set_property("ssl-strict", !trust_invalid_certificate);
    }
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
    fn media_verification_requires_decoded_audio_before_end_of_stream() {
        assert!(verified_eos(1).is_ok());
        assert_eq!(
            verified_eos(0),
            Err(
                "GStreamer media verification reached end of stream without decoded audio"
                    .to_string()
            )
        );
    }

    #[test]
    fn http_source_uses_the_prepared_certificate_policy() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        let playbin = gst::ElementFactory::make("playbin")
            .build()
            .expect("GStreamer playbin");
        let source = gst::ElementFactory::make("souphttpsrc")
            .build()
            .expect("GStreamer HTTP source");
        let trust_invalid_certificate = Arc::new(AtomicBool::new(false));
        let policy = Arc::clone(&trust_invalid_certificate);
        connect_server_certificate_policy(&playbin, move || policy.load(Ordering::SeqCst));

        playbin.emit_by_name::<()>("source-setup", &[&source]);
        assert!(source.property::<bool>("ssl-strict"));

        trust_invalid_certificate.store(true, Ordering::SeqCst);
        playbin.emit_by_name::<()>("source-setup", &[&source]);
        assert!(!source.property::<bool>("ssl-strict"));
    }
}
