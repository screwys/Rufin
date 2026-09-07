//! Cancellable EBU R128 analysis for one prepared audio stream.

use super::{connect_server_certificate_policy, ensure_gstreamer_initialized};
use ebur128::{Channel, EbuR128, Mode};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_audio as gst_audio;
use playback::ResolvedStream;
use std::time::{Duration, Instant};

const ANALYSIS_STALL_TIMEOUT: Duration = Duration::from_secs(60);
const SAMPLE_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalyzedLoudness {
    pub integrated_lufs: Option<f64>,
    pub true_peak: Option<f64>,
}

pub struct LoudnessAnalysis {
    analyzer: EbuR128,
    measurement: AnalyzedLoudness,
}

impl LoudnessAnalysis {
    pub const fn measurement(&self) -> AnalyzedLoudness {
        self.measurement
    }
}

pub fn analyze_loudness_cancellable(
    stream: &ResolvedStream,
    cancelled: impl Fn() -> bool,
) -> Result<LoudnessAnalysis, String> {
    ensure_gstreamer_initialized()?;
    check_analysis_progress(&cancelled, Instant::now())?;

    let pipeline = gst::parse::launch(
        "uridecodebin name=decoder ! audioconvert ! audio/x-raw,format=F32LE,layout=interleaved ! appsink name=sink sync=false",
    )
    .map_err(|error| error.to_string())?;
    let bin = pipeline
        .downcast_ref::<gst::Bin>()
        .ok_or_else(|| "loudness pipeline is not a bin".to_string())?;
    let decoder = bin
        .by_name("decoder")
        .ok_or_else(|| "loudness pipeline is missing its decoder".to_string())?;
    let trust_invalid_certificate = stream.trust_invalid_certificate();
    connect_server_certificate_policy(&decoder, move || trust_invalid_certificate);
    decoder.set_property("uri", stream.uri());
    let sink = bin
        .by_name("sink")
        .ok_or_else(|| "loudness pipeline is missing its sample sink".to_string())?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| "loudness sample sink has the wrong type".to_string())?;
    sink.set_max_buffers(8);
    sink.set_drop(false);
    let bus = pipeline
        .bus()
        .ok_or_else(|| "loudness pipeline is missing its bus".to_string())?;

    let started = Instant::now();
    let result = (|| {
        let startup_state = if stream.window().is_some() {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        pipeline
            .set_state(startup_state)
            .map_err(|error| error.to_string())?;
        if let Some(window) = stream.window() {
            wait_for_preroll(&bus, &cancelled, started)?;
            let _ = sink.try_pull_preroll(gst::ClockTime::ZERO);
            pipeline
                .seek(
                    1.0,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(window.start_millis),
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(window.end_millis),
                )
                .map_err(|error| error.to_string())?;
            pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| error.to_string())?;
        }

        let mut analyzer = None;
        let mut decoded_samples = Vec::new();
        let mut last_progress = Instant::now();
        loop {
            check_analysis_progress(&cancelled, last_progress)?;
            if let Some(sample) = sink.try_pull_sample(SAMPLE_POLL) {
                add_sample(&mut analyzer, &sample, &mut decoded_samples)?;
                last_progress = Instant::now();
                continue;
            }
            while let Some(message) = bus.pop() {
                if let gst::MessageView::Error(error) = message.view() {
                    return Err(error.error().to_string());
                }
            }
            if sink.is_eos() {
                break;
            }
        }
        let mut analyzer =
            analyzer.ok_or_else(|| "audio stream produced no samples".to_string())?;
        let measurement = measurement(&analyzer)?;
        // Album aggregation reads the completed histogram, not the source-rate signal window.
        analyzer
            .change_parameters(1, 16)
            .map_err(|error| error.to_string())?;
        Ok(LoudnessAnalysis {
            analyzer,
            measurement,
        })
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

pub fn album_loudness(analyses: &[LoudnessAnalysis]) -> Result<AnalyzedLoudness, String> {
    if analyses.is_empty() {
        return Err("an album needs at least one loudness analysis".to_string());
    }
    let integrated_lufs =
        EbuR128::loudness_global_multiple(analyses.iter().map(|analysis| &analysis.analyzer))
            .map_err(|error| error.to_string())?;
    Ok(AnalyzedLoudness {
        integrated_lufs: integrated_lufs.is_finite().then_some(integrated_lufs),
        true_peak: analyses
            .iter()
            .filter_map(|analysis| analysis.measurement.true_peak)
            .reduce(f64::max),
    })
}

fn add_sample(
    analyzer: &mut Option<EbuR128>,
    sample: &gst::Sample,
    decoded_samples: &mut Vec<f32>,
) -> Result<(), String> {
    let caps = sample
        .caps()
        .ok_or_else(|| "decoded audio sample is missing its format".to_string())?;
    let info = gst_audio::AudioInfo::from_caps(caps).map_err(|error| error.to_string())?;
    if analyzer.is_none() {
        *analyzer = Some(new_analyzer(&info)?);
    }
    let buffer = sample
        .buffer()
        .ok_or_else(|| "decoded audio sample is missing its buffer".to_string())?;
    let map = buffer.map_readable().map_err(|error| error.to_string())?;
    let bytes = map.as_slice();
    if bytes.len() % std::mem::size_of::<f32>() != 0 {
        return Err("decoded audio buffer is not aligned to floating-point samples".to_string());
    }
    decoded_samples.clear();
    decoded_samples.extend(
        bytes
            .chunks_exact(4)
            .map(|sample| f32::from_le_bytes(sample.try_into().expect("four-byte sample"))),
    );
    analyzer
        .as_mut()
        .expect("analyzer was initialized")
        .add_frames_f32(decoded_samples)
        .map_err(|error| error.to_string())
}

fn new_analyzer(info: &gst_audio::AudioInfo) -> Result<EbuR128, String> {
    let channels = info.channels();
    let mut analyzer = EbuR128::new(
        channels,
        info.rate(),
        Mode::I | Mode::TRUE_PEAK | Mode::HISTOGRAM,
    )
    .map_err(|error| error.to_string())?;
    let positions = info
        .positions()
        .ok_or_else(|| "decoded audio has no usable channel positions".to_string())?;
    let channel_map = positions
        .iter()
        .copied()
        .map(ebur128_channel)
        .collect::<Result<Vec<_>, _>>()?;
    analyzer
        .set_channel_map(&channel_map)
        .map_err(|error| error.to_string())?;
    Ok(analyzer)
}

fn ebur128_channel(position: gst_audio::AudioChannelPosition) -> Result<Channel, String> {
    use gst_audio::AudioChannelPosition as Gst;
    Ok(match position {
        Gst::Mono => Channel::Center,
        Gst::FrontLeft => Channel::Left,
        Gst::FrontRight => Channel::Right,
        Gst::FrontCenter => Channel::Center,
        Gst::Lfe1 | Gst::Lfe2 => Channel::Unused,
        Gst::RearLeft => Channel::Mp135,
        Gst::RearRight => Channel::Mm135,
        Gst::FrontLeftOfCenter => Channel::MpSC,
        Gst::FrontRightOfCenter => Channel::MmSC,
        Gst::RearCenter => Channel::Mp180,
        Gst::SideLeft => Channel::Mp090,
        Gst::SideRight => Channel::Mm090,
        Gst::TopFrontLeft => Channel::Up030,
        Gst::TopFrontRight => Channel::Um030,
        Gst::TopFrontCenter => Channel::Up000,
        Gst::TopCenter => Channel::Tp000,
        Gst::TopRearLeft => Channel::Up135,
        Gst::TopRearRight => Channel::Um135,
        Gst::TopSideLeft => Channel::Up090,
        Gst::TopSideRight => Channel::Um090,
        Gst::TopRearCenter => Channel::Up180,
        Gst::BottomFrontCenter => Channel::Bp000,
        Gst::BottomFrontLeft => Channel::Bp045,
        Gst::BottomFrontRight => Channel::Bm045,
        Gst::WideLeft => Channel::Mp060,
        Gst::WideRight => Channel::Mm060,
        Gst::SurroundLeft => Channel::LeftSurround,
        Gst::SurroundRight => Channel::RightSurround,
        Gst::TopSurroundLeft => Channel::Up110,
        Gst::TopSurroundRight => Channel::Um110,
        _ => return Err(format!("unsupported audio channel position {position:?}")),
    })
}

fn measurement(analyzer: &EbuR128) -> Result<AnalyzedLoudness, String> {
    let integrated_lufs = analyzer
        .loudness_global()
        .map_err(|error| error.to_string())?;
    let mut true_peak = 0.0_f64;
    for channel in 0..analyzer.channels() {
        true_peak = true_peak.max(
            analyzer
                .true_peak(channel)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(AnalyzedLoudness {
        integrated_lufs: integrated_lufs.is_finite().then_some(integrated_lufs),
        true_peak: Some(true_peak),
    })
}

fn wait_for_preroll(
    bus: &gst::Bus,
    cancelled: &impl Fn() -> bool,
    started: Instant,
) -> Result<(), String> {
    loop {
        check_analysis_progress(cancelled, started)?;
        let Some(message) = bus.timed_pop(SAMPLE_POLL) else {
            continue;
        };
        match message.view() {
            gst::MessageView::AsyncDone(..) => return Ok(()),
            gst::MessageView::Error(error) => return Err(error.error().to_string()),
            gst::MessageView::Eos(..) => {
                return Err("audio source ended before its requested window".to_string());
            }
            _ => {}
        }
    }
}

fn check_analysis_progress(cancelled: &impl Fn() -> bool, started: Instant) -> Result<(), String> {
    if cancelled() {
        return Err("loudness analysis cancelled".to_string());
    }
    if started.elapsed() > ANALYSIS_STALL_TIMEOUT {
        return Err("loudness analysis stopped making progress".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn analysis_is_bounded_to_the_prepared_stream_window() {
        let directory = tempfile::tempdir().expect("loudness fixture directory");
        let path = directory.path().join("window.wav");
        write_stereo_wave(&path, &[0.8, 0.03]);
        let uri = gst::glib::filename_to_uri(&path, None).expect("loudness fixture URI");
        let stream = ResolvedStream::new(uri.as_str()).with_window(1_000, 2_000);

        let result = analyze_loudness_cancellable(&stream, || false)
            .expect("analyze prepared stream window");

        assert!(result.measurement.true_peak.is_some_and(|peak| peak < 0.05));
        assert!(result.measurement.integrated_lufs.is_some());
    }

    #[test]
    fn album_loudness_uses_combined_gating_history_and_maximum_peak() {
        let directory = tempfile::tempdir().expect("loudness fixture directory");
        let quiet_path = directory.path().join("quiet.wav");
        let loud_path = directory.path().join("loud.wav");
        write_stereo_wave(&quiet_path, &[0.05]);
        write_stereo_wave(&loud_path, &[0.5]);
        let analyses = [analyze_path(&quiet_path), analyze_path(&loud_path)];

        let album = album_loudness(&analyses).expect("calculate album loudness");
        let quiet_lufs = analyses[0]
            .measurement
            .integrated_lufs
            .expect("quiet loudness");
        let loud_lufs = analyses[1]
            .measurement
            .integrated_lufs
            .expect("loud loudness");
        let album_lufs = album.integrated_lufs.expect("album loudness");

        assert!(
            album_lufs >= quiet_lufs && album_lufs <= loud_lufs,
            "quiet {quiet_lufs}, album {album_lufs}, loud {loud_lufs}"
        );
        assert!((album_lufs - (quiet_lufs + loud_lufs) / 2.0).abs() > 1.0);
        assert_eq!(album.true_peak, analyses[1].measurement.true_peak);
    }

    #[test]
    fn completed_analysis_releases_the_source_rate_buffer() {
        let directory = tempfile::tempdir().expect("loudness fixture directory");
        let path = directory.path().join("high-rate.wav");
        write_stereo_wave_at_rate(&path, &[0.5], 192_000);

        let analysis = analyze_path(&path);

        assert_eq!(analysis.analyzer.channels(), 1);
        assert_eq!(analysis.analyzer.rate(), 16);
    }

    #[test]
    fn analysis_honors_cancellation_before_decoding() {
        let stream = ResolvedStream::new("file:///not-opened.wav");
        let error = analyze_loudness_cancellable(&stream, || true)
            .err()
            .expect("cancelled analysis");
        assert_eq!(error, "loudness analysis cancelled");
    }

    fn analyze_path(path: &std::path::Path) -> LoudnessAnalysis {
        let uri = gst::glib::filename_to_uri(path, None).expect("loudness fixture URI");
        analyze_loudness_cancellable(&ResolvedStream::new(uri.as_str()), || false)
            .expect("analyze loudness fixture")
    }

    fn write_stereo_wave(path: &std::path::Path, amplitudes: &[f32]) {
        write_stereo_wave_at_rate(path, amplitudes, 48_000);
    }

    fn write_stereo_wave_at_rate(path: &std::path::Path, amplitudes: &[f32], sample_rate: u32) {
        const CHANNELS: u16 = 2;
        const BITS_PER_SAMPLE: u16 = 16;
        let frames = sample_rate * amplitudes.len() as u32;
        let bytes_per_frame = u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
        let data_len = frames * bytes_per_frame;
        let mut file = File::create(path).expect("create loudness fixture");
        file.write_all(b"RIFF").expect("write RIFF");
        file.write_all(&(36 + data_len).to_le_bytes())
            .expect("write RIFF size");
        file.write_all(b"WAVEfmt ").expect("write WAVE format");
        file.write_all(&16_u32.to_le_bytes())
            .expect("write PCM format size");
        file.write_all(&1_u16.to_le_bytes())
            .expect("write PCM format");
        file.write_all(&CHANNELS.to_le_bytes())
            .expect("write channels");
        file.write_all(&sample_rate.to_le_bytes())
            .expect("write sample rate");
        file.write_all(&(sample_rate * bytes_per_frame).to_le_bytes())
            .expect("write byte rate");
        file.write_all(&(bytes_per_frame as u16).to_le_bytes())
            .expect("write block alignment");
        file.write_all(&BITS_PER_SAMPLE.to_le_bytes())
            .expect("write sample depth");
        file.write_all(b"data").expect("write data tag");
        file.write_all(&data_len.to_le_bytes())
            .expect("write data length");

        for amplitude in amplitudes.iter().copied() {
            for frame in 0..sample_rate {
                let phase = TAU * 1_000.0 * frame as f32 / sample_rate as f32;
                let sample = (phase.sin() * amplitude * f32::from(i16::MAX)) as i16;
                for _ in 0..CHANNELS {
                    file.write_all(&sample.to_le_bytes()).expect("write sample");
                }
            }
        }
    }
}
