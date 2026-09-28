//! Audio decoding jobs and bounded signal processing shared by playback hosts.

use gst::prelude::*;
use gstreamer as gst;
use std::sync::OnceLock;

mod loudness;
#[cfg(target_os = "macos")]
mod tls;
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
        .get_or_init(|| {
            gst::init().map_err(|error| error.to_string())?;
            #[cfg(target_os = "macos")]
            if let Err(error) = tls::configure_native_trust() {
                tracing::warn!(%error, "Could not load native certificates for media HTTPS");
            }
            Ok(())
        })
        .clone()
}

pub fn configure_sources(
    element: &gst::Element,
    trust_invalid_certificate: impl Fn() -> bool + Send + Sync + 'static,
) {
    element.connect("source-setup", false, move |values| {
        let source = values[1].get::<gst::Element>().expect("decoder source");
        if source.find_property("ssl-strict").is_some() {
            source.set_property("ssl-strict", !trust_invalid_certificate());
        }
        if source
            .factory()
            .is_some_and(|factory| factory.name() == "souphttpsrc")
        {
            // Soup doubles fast reads. Parsers can copy the whole read to join
            // a split frame, so keep transfer blocks small independently of buffering.
            source
                .static_pad("src")
                .expect("HTTP source output")
                .add_probe(gst::PadProbeType::BUFFER, |pad, _| {
                    let source = pad.parent_element().expect("HTTP source");
                    if source.property::<u32>("blocksize") > 64 * 1024 {
                        source.set_property("blocksize", 64_u32 * 1024);
                    }
                    gst::PadProbeReturn::Ok
                });
        }
        None
    });
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
        configure_sources(&playbin, move || policy.load(Ordering::SeqCst));

        playbin.emit_by_name::<()>("source-setup", &[&source]);
        assert!(source.property::<bool>("ssl-strict"));

        trust_invalid_certificate.store(true, Ordering::SeqCst);
        playbin.emit_by_name::<()>("source-setup", &[&source]);
        assert!(!source.property::<bool>("ssl-strict"));
    }
}
