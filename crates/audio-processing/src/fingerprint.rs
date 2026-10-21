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
        "audio/x-raw,format=S16LE,layout=interleaved ! appsink name=sink sync=false max-buffers=8 drop=false",
    )?;
    let sink = pipeline
        .sink
        .clone()
        .downcast::<AppSink>()
        .expect("fingerprint sink");
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
    let mut printer = chromaprint::Fingerprinter::new(chromaprint::Algorithm::default());
    let mut samples = Vec::new();
    let mut format = None;
    let mut consumed = 0;
    let mut limit = 0;
    while let Some(sample) = pipeline.sample(&sink, || check(progress))? {
        let info = AudioInfo::from_caps(sample.caps().ok_or("audio sample has no format")?)
            .map_err(|error| error.to_string())?;
        if format.is_none() {
            printer
                .start(info.rate(), info.channels() as u16)
                .map_err(|error| error.to_string())?;
            limit = u64::from(info.rate()) * u64::from(info.channels()) * 120;
            format = Some(info);
        }
        let map = sample
            .buffer()
            .ok_or("audio sample has no buffer")?
            .map_readable()
            .map_err(|error| error.to_string())?;
        samples.clear();
        samples.extend(
            map.as_slice()
                .chunks_exact(2)
                .take((limit - consumed) as usize)
                .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]])),
        );
        printer.feed(&samples).map_err(|error| error.to_string())?;
        consumed += samples.len() as u64;
        progress = Instant::now();
        if consumed >= limit {
            break;
        }
    }
    let info = format.ok_or("audio stream produced no samples")?;
    printer.finish().map_err(|error| error.to_string())?;
    if printer.fingerprint().is_empty() {
        return Err("audio is too short to fingerprint".into());
    }
    let duration = pipeline
        .duration_millis()
        .filter(|duration| *duration > 0)
        .unwrap_or(if duration_millis > 0 {
            duration_millis
        } else if consumed < limit {
            consumed * 1000 / u64::from(info.rate()) / u64::from(info.channels())
        } else {
            0
        });
    if duration == 0 {
        return Err("audio duration is unavailable".into());
    }
    Ok(AudioFingerprint {
        encoded: printer.encode(),
        duration_seconds: (duration / 1000).max(1),
    })
}
