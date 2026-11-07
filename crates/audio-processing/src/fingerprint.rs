use super::decode::DecodePipeline;
use crate::ResolvedStream;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app::AppSink;
use gstreamer_audio::AudioInfo;
use std::time::{Duration, Instant};

pub const FINGERPRINT_VERSION: &str = "chromaprint-test2-120s-v1";

pub struct AudioFingerprint {
    pub encoded: String,
    pub duration_seconds: u64,
}

pub fn fingerprint_audio(
    stream: &ResolvedStream,
    duration_millis: u64,
    cancelled: impl Fn() -> bool,
) -> Result<AudioFingerprint, String> {
    let pipeline = DecodePipeline::new(
        stream,
        "chromaprint name=printer duration=120 ! appsink name=sink sync=false max-buffers=8 drop=false",
    )?;
    let sink = pipeline
        .sink
        .clone()
        .downcast::<AppSink>()
        .expect("fingerprint sink");
    let printer = pipeline
        .pipeline
        .downcast_ref::<gst::Bin>()
        .expect("fingerprint pipeline")
        .by_name("printer")
        .expect("fingerprint element");
    let mut progress = Instant::now();
    let check = |progress: Instant| {
        if cancelled() {
            Err("audio scan cancelled".to_string())
        } else if progress.elapsed() > Duration::from_secs(60) {
            Err("audio scan stopped making progress".into())
        } else {
            Ok(())
        }
    };
    pipeline.start(|| check(progress))?;
    let mut frames = 0;
    let mut rate = 0;
    while let Some(sample) = pipeline.sample(&sink, || check(progress))? {
        let info = AudioInfo::from_caps(sample.caps().ok_or("audio sample has no format")?)
            .map_err(|error| error.to_string())?;
        let buffer = sample.buffer().ok_or("audio sample has no buffer")?;
        rate = u64::from(info.rate());
        frames += buffer.size() as u64 / u64::from(info.bpf());
        progress = Instant::now();
        if frames >= rate * 120 {
            break;
        }
    }
    let encoded = printer
        .property::<Option<String>>("fingerprint")
        .filter(|value| !value.is_empty())
        .ok_or("audio is too short to fingerprint")?;
    let duration = pipeline
        .duration_millis()
        .filter(|duration| *duration > 0)
        .unwrap_or(if duration_millis > 0 {
            duration_millis
        } else if sink.is_eos() {
            frames * 1000 / rate
        } else {
            0
        });
    if duration == 0 {
        return Err("audio duration is unavailable".into());
    }
    Ok(AudioFingerprint {
        encoded,
        duration_seconds: (duration / 1000).max(1),
    })
}
