//! Audio decoding jobs and bounded signal processing shared by playback hosts.

use gst::prelude::*;
use gstreamer as gst;
use std::sync::OnceLock;

mod loudness;
mod transcode;
mod visualizer;
mod waveform;

pub use loudness::{
    AnalyzedLoudness, LoudnessAnalysis, album_loudness, analyze_loudness_cancellable,
};
pub use transcode::TranscodedAudioReader;
pub use waveform::generate_waveform_peaks_cancellable;

pub use visualizer::VisualizerFft;

/// Initialize GStreamer once before playback or waveform work starts.
pub fn ensure_gstreamer_initialized() -> Result<(), String> {
    static INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();
    INITIALIZED
        .get_or_init(|| gst::init().map_err(|error| error.to_string()))
        .clone()
}

pub fn connect_server_certificate_policy(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

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
