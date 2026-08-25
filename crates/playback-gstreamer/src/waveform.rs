//! Cancellable waveform decoding for one prepared audio stream.

use super::engine::{EventMailbox, PipelineId, SharedBackendState, Slot, push_event};
use super::{ensure_gstreamer_initialized, lock_recover};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
use playback::ResolvedStream;
use playback::{BackendEvent, RunId};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::warn;

const VISUALIZER_CHANNEL_CAPACITY: usize = 2;
const VISUALIZER_COPY_FRAMES: usize = 4_096;
const VISUALIZER_FFT_SIZE: usize = 2_048;
const VISUALIZER_MIN_EMIT_INTERVAL: Duration = Duration::from_millis(33);
const VISUALIZER_NOISE_FLOOR_DB: f32 = -72.0;
const VISUALIZER_CEILING_DB: f32 = -6.0;
const WAVEFORM_GENERATION_TIMEOUT: Duration = Duration::from_secs(180);
const WAVEFORM_BUS_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(250);

pub fn generate_waveform_peaks_cancellable(
    stream: &ResolvedStream,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<(f64, f64)>, String> {
    ensure_gstreamer_initialized()?;
    if cancelled() {
        return Err("waveform generation cancelled".to_string());
    }

    let pipeline =
        gst::parse::launch("uridecodebin name=decoder ! audioconvert ! audio/x-raw,channels=2 ! level name=level interval=250000000 ! fakesink name=sink")
            .map_err(|error| error.to_string())?;
    let bin = pipeline
        .downcast_ref::<gst::Bin>()
        .ok_or_else(|| "waveform pipeline is not a bin".to_string())?;
    let decoder = bin
        .by_name("decoder")
        .ok_or_else(|| "waveform pipeline is missing decoder".to_string())?;
    let trust_invalid_certificate = stream.trust_invalid_certificate();
    super::connect_server_certificate_policy(&decoder, move || trust_invalid_certificate);
    decoder.set_property("uri", stream.uri());

    let sink = bin
        .by_name("sink")
        .ok_or_else(|| "waveform pipeline is missing sink".to_string())?;
    sink.set_property("qos", false);
    sink.set_property("sync", false);

    let bus = pipeline
        .bus()
        .ok_or_else(|| "waveform pipeline is missing bus".to_string())?;

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

        let mut peaks = Vec::new();
        loop {
            check_waveform_deadline(&cancelled, started)?;
            let Some(message) = bus.timed_pop(WAVEFORM_BUS_POLL) else {
                continue;
            };
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(..) => return Ok(peaks),
                MessageView::Error(error) => return Err(error.error().to_string()),
                MessageView::Element(element) => {
                    if let Some(structure) = element.structure()
                        && structure.has_name("level")
                        && let Ok(values) = structure.get::<&gst::glib::ValueArray>("peak")
                        && values.len() >= 2
                        && let (Ok(left), Ok(right)) =
                            (values[0].get::<f64>(), values[1].get::<f64>())
                    {
                        peaks.push((db_to_amplitude(left), db_to_amplitude(right)));
                    }
                }
                _ => {}
            }
        }
    })();

    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn wait_for_preroll(
    bus: &gst::Bus,
    cancelled: &impl Fn() -> bool,
    started: Instant,
) -> Result<(), String> {
    loop {
        check_waveform_deadline(cancelled, started)?;
        let Some(message) = bus.timed_pop(WAVEFORM_BUS_POLL) else {
            continue;
        };
        use gst::MessageView;
        match message.view() {
            MessageView::AsyncDone(..) => return Ok(()),
            MessageView::Error(error) => return Err(error.error().to_string()),
            MessageView::Eos(..) => {
                return Err("waveform source ended before its requested window".to_string());
            }
            _ => {}
        }
    }
}

fn check_waveform_deadline(cancelled: &impl Fn() -> bool, started: Instant) -> Result<(), String> {
    if cancelled() {
        return Err("waveform generation cancelled".to_string());
    }
    if started.elapsed() > WAVEFORM_GENERATION_TIMEOUT {
        return Err("waveform generation timed out".to_string());
    }
    Ok(())
}

fn db_to_amplitude(value: f64) -> f64 {
    10.0_f64.powf(value / 20.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn waveform_decode_is_bounded_to_the_prepared_stream_window() {
        let directory = tempfile::tempdir().expect("waveform fixture directory");
        let path = directory.path().join("window.wav");
        write_stereo_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("waveform fixture URI");
        let stream = ResolvedStream::new(uri.as_str()).with_window(1_000, 1_500);

        let peaks = generate_waveform_peaks_cancellable(&stream, || false)
            .expect("decode waveform source window");

        assert!(!peaks.is_empty());
        assert!(peaks.len() <= 3, "unexpected full-file waveform: {peaks:?}");
        assert!(
            peaks
                .iter()
                .take(2)
                .all(|(left, right)| *left > 0.5 && *right > 0.5),
            "unexpected pre-window waveform: {peaks:?}"
        );
    }

    fn write_stereo_wave(path: &std::path::Path) {
        const SAMPLE_RATE: u32 = 8_000;
        const CHANNELS: u16 = 2;
        const BITS_PER_SAMPLE: u16 = 16;
        let frames = SAMPLE_RATE * 2;
        let bytes_per_frame = u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
        let data_len = frames * bytes_per_frame;
        let mut file = File::create(path).expect("create waveform fixture");
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
        file.write_all(&SAMPLE_RATE.to_le_bytes())
            .expect("write sample rate");
        file.write_all(&(SAMPLE_RATE * bytes_per_frame).to_le_bytes())
            .expect("write byte rate");
        file.write_all(&(bytes_per_frame as u16).to_le_bytes())
            .expect("write block alignment");
        file.write_all(&BITS_PER_SAMPLE.to_le_bytes())
            .expect("write sample depth");
        file.write_all(b"data").expect("write data tag");
        file.write_all(&data_len.to_le_bytes())
            .expect("write data length");

        for frame in 0..frames {
            let sample = if frame < SAMPLE_RATE {
                0_i16
            } else if frame % 2 == 0 {
                26_000_i16
            } else {
                -26_000_i16
            };
            file.write_all(&sample.to_le_bytes())
                .expect("write left sample");
            file.write_all(&sample.to_le_bytes())
                .expect("write right sample");
        }
    }
}

#[derive(Clone)]
pub(super) struct VisualizerTap {
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
    sender: SyncSender<VisualizerFrame>,
}
impl VisualizerTap {
    pub(super) fn install(&self, pad: &gst::Pad) -> Option<gst::PadProbeId> {
        let tap = self.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let Some(buffer) = info.buffer() else {
                return gst::PadProbeReturn::Ok;
            };
            let Some(samples) = copy_visualizer_samples(pad, buffer) else {
                return gst::PadProbeReturn::Ok;
            };
            let frame = VisualizerFrame {
                slot: tap.slot,
                pipeline_id: tap.pipeline_id,
                run: tap.run,
                samples,
            };
            match tap.sender.try_send(frame) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return gst::PadProbeReturn::Remove,
            }
            gst::PadProbeReturn::Ok
        })
    }
}
struct VisualizerFrame {
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
    samples: Vec<f32>,
}
pub(super) struct VisualizerAnalyzer {
    sender: SyncSender<VisualizerFrame>,
}
impl VisualizerAnalyzer {
    pub(super) fn new(
        events: Arc<Mutex<EventMailbox>>,
        shared: Arc<Mutex<SharedBackendState>>,
    ) -> Self {
        let (sender, receiver) = sync_channel(VISUALIZER_CHANNEL_CAPACITY);
        let _ = thread::Builder::new()
            .name("rufin-visualizer-fft".to_string())
            .spawn(move || run_visualizer_worker(receiver, events, shared))
            .inspect_err(|error| warn!(%error, "failed to start visualizer FFT worker"));
        Self { sender }
    }

    pub(super) fn tap(&self, slot: Slot, pipeline_id: PipelineId, run: RunId) -> VisualizerTap {
        VisualizerTap {
            slot,
            pipeline_id,
            run,
            sender: self.sender.clone(),
        }
    }
}
fn copy_visualizer_samples(pad: &gst::Pad, buffer: &gst::Buffer) -> Option<Vec<f32>> {
    let caps = pad.current_caps()?;
    let info = gst_audio::AudioInfo::from_caps(caps.as_ref()).ok()?;
    let map = buffer.map_readable().ok()?;
    copy_audio_samples(map.as_slice(), &info, VISUALIZER_COPY_FRAMES)
}
fn copy_audio_samples(
    bytes: &[u8],
    info: &gst_audio::AudioInfo,
    max_frames: usize,
) -> Option<Vec<f32>> {
    if info.layout() != gst_audio::AudioLayout::Interleaved {
        return None;
    }
    let channels = usize::try_from(info.channels()).ok()?.max(1);
    let frame_size = usize::try_from(info.bpf()).ok()?;
    let sample_size = visualizer_sample_size(info.format())?;
    if frame_size == 0 || sample_size == 0 || sample_size.saturating_mul(channels) > frame_size {
        return None;
    }
    let frames = (bytes.len() / frame_size).min(max_frames);
    let mut samples = Vec::with_capacity(frames);
    for frame_index in 0..frames {
        let frame_start = frame_index * frame_size;
        let mut total = 0.0;
        for channel in 0..channels {
            let sample_start = frame_start + channel * sample_size;
            let sample_end = sample_start + sample_size;
            let sample = bytes
                .get(sample_start..sample_end)
                .and_then(|slice| decode_visualizer_sample(info.format(), slice))
                .unwrap_or(0.0);
            total += sample.clamp(-1.0, 1.0);
        }
        let mono = total / channels as f32;
        samples.push(mono);
    }
    (!samples.is_empty()).then_some(samples)
}
fn visualizer_sample_size(format: gst_audio::AudioFormat) -> Option<usize> {
    Some(match format {
        gst_audio::AudioFormat::S8 | gst_audio::AudioFormat::U8 => 1,
        gst_audio::AudioFormat::S16le | gst_audio::AudioFormat::U16le => 2,
        gst_audio::AudioFormat::S24le | gst_audio::AudioFormat::U24le => 3,
        gst_audio::AudioFormat::S2432le
        | gst_audio::AudioFormat::U2432le
        | gst_audio::AudioFormat::S32le
        | gst_audio::AudioFormat::U32le
        | gst_audio::AudioFormat::F32le => 4,
        gst_audio::AudioFormat::F64le => 8,
        _ => return None,
    })
}
fn decode_visualizer_sample(format: gst_audio::AudioFormat, bytes: &[u8]) -> Option<f32> {
    let sample = match format {
        gst_audio::AudioFormat::S8 => i8::from_ne_bytes([bytes[0]]) as f32 / i8::MAX as f32,
        gst_audio::AudioFormat::U8 => (bytes[0] as f32 - 128.0) / 128.0,
        gst_audio::AudioFormat::S16le => {
            i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / i16::MAX as f32
        }
        gst_audio::AudioFormat::U16le => {
            (u16::from_le_bytes([bytes[0], bytes[1]]) as f32 - 32_768.0) / 32_768.0
        }
        gst_audio::AudioFormat::S24le => decode_s24le(bytes) as f32 / 8_388_607.0,
        gst_audio::AudioFormat::U24le => (decode_u24le(bytes) as f32 - 8_388_608.0) / 8_388_608.0,
        gst_audio::AudioFormat::S2432le => {
            (i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >> 8) as f32 / 8_388_607.0
        }
        gst_audio::AudioFormat::U2432le => {
            ((u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >> 8) as f32
                - 8_388_608.0)
                / 8_388_608.0
        }
        gst_audio::AudioFormat::S32le => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / i32::MAX as f32
        }
        gst_audio::AudioFormat::U32le => {
            (u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 - 2_147_483_648.0)
                / 2_147_483_648.0
        }
        gst_audio::AudioFormat::F32le => {
            f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        }
        gst_audio::AudioFormat::F64le => f64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]) as f32,
        _ => return None,
    };
    Some(if sample.is_finite() { sample } else { 0.0 })
}
fn decode_s24le(bytes: &[u8]) -> i32 {
    let sign = if bytes[2] & 0x80 == 0 { 0x00 } else { 0xff };
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], sign])
}
fn decode_u24le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}
fn run_visualizer_worker(
    receiver: Receiver<VisualizerFrame>,
    events: Arc<Mutex<EventMailbox>>,
    shared: Arc<Mutex<SharedBackendState>>,
) {
    let mut fft = VisualizerFft::new();
    let mut current_pipeline = None;
    while let Ok(frame) = receiver.recv() {
        if !visualizer_pipeline_is_live(&shared, frame.slot, frame.pipeline_id, frame.run) {
            continue;
        }
        let pipeline = (frame.pipeline_id, frame.run);
        if current_pipeline != Some(pipeline) {
            fft.clear();
            current_pipeline = Some(pipeline);
        }
        fft.push_samples(&frame.samples);
        let Some(levels) = fft.maybe_levels() else {
            continue;
        };
        if !visualizer_pipeline_is_live(&shared, frame.slot, frame.pipeline_id, frame.run) {
            continue;
        }
        push_event(
            &events,
            BackendEvent::Visualizer {
                run: frame.run,
                levels,
            },
        );
    }
}
pub(super) fn visualizer_pipeline_is_live(
    shared: &Arc<Mutex<SharedBackendState>>,
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
) -> bool {
    let shared = lock_recover(shared);
    shared.visualizer_enabled
        && shared.pipeline_is_current(slot, pipeline_id)
        && shared
            .current
            .as_ref()
            .is_some_and(|current| current.run == run)
}
struct VisualizerFft {
    samples: VecDeque<f32>,
    input: Vec<Complex<f32>>,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    last_emit: Option<Instant>,
}
impl VisualizerFft {
    fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(VISUALIZER_FFT_SIZE);
        let window = (0..VISUALIZER_FFT_SIZE)
            .map(|index| {
                let position = index as f32 / (VISUALIZER_FFT_SIZE - 1) as f32;
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * position).cos()
            })
            .collect();
        Self {
            samples: VecDeque::with_capacity(VISUALIZER_FFT_SIZE),
            input: vec![Complex::new(0.0, 0.0); VISUALIZER_FFT_SIZE],
            window,
            fft,
            last_emit: None,
        }
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.last_emit = None;
    }

    fn push_samples(&mut self, samples: &[f32]) {
        let start = samples.len().saturating_sub(VISUALIZER_FFT_SIZE);
        for sample in &samples[start..] {
            if self.samples.len() == VISUALIZER_FFT_SIZE {
                self.samples.pop_front();
            }
            self.samples.push_back(*sample);
        }
    }

    fn maybe_levels(&mut self) -> Option<Vec<f64>> {
        if self.samples.len() < VISUALIZER_FFT_SIZE {
            return None;
        }
        let now = Instant::now();
        if self
            .last_emit
            .is_some_and(|last| now.duration_since(last) < VISUALIZER_MIN_EMIT_INTERVAL)
        {
            return None;
        }
        self.last_emit = Some(now);
        Some(self.levels())
    }

    fn levels(&mut self) -> Vec<f64> {
        for ((slot, sample), window) in self
            .input
            .iter_mut()
            .zip(self.samples.iter().copied())
            .zip(self.window.iter().copied())
        {
            *slot = Complex::new(sample * window, 0.0);
        }
        self.fft.process(&mut self.input);
        fft_levels(&self.input)
    }
}
fn fft_levels(input: &[Complex<f32>]) -> Vec<f64> {
    let half = VISUALIZER_FFT_SIZE / 2;
    input
        .iter()
        .take(half)
        .enumerate()
        .map(|(index, value)| {
            if index == 0 {
                return 0.0;
            }
            let magnitude = value.norm() / VISUALIZER_FFT_SIZE as f32;
            let db = 20.0 * magnitude.max(1.0e-6).log10();
            let level = ((db - VISUALIZER_NOISE_FLOOR_DB)
                / (VISUALIZER_CEILING_DB - VISUALIZER_NOISE_FLOOR_DB))
                .clamp(0.0, 1.0);
            f64::from(level.powf(1.25))
        })
        .collect()
}

#[cfg(test)]
mod visualizer_tests {
    use super::*;
    use std::sync::{MutexGuard, OnceLock};

    static GST_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn gst_test_guard() -> MutexGuard<'static, ()> {
        let guard = lock_recover(GST_TEST_LOCK.get_or_init(|| Mutex::new(())));
        ensure_gstreamer_initialized().expect("initialize GStreamer");
        guard
    }

    #[test]
    fn visualizer_pcm_copy_mixes_supported_stereo_formats() {
        let _gst = gst_test_guard();
        let float_info = gst_audio::AudioInfo::builder(gst_audio::AudioFormat::F32le, 48_000, 2)
            .layout(gst_audio::AudioLayout::Interleaved)
            .build()
            .expect("float audio info");
        let mut float_bytes = Vec::new();
        for sample in [(0.5_f32, -0.25_f32), (2.0_f32, 0.0_f32)] {
            float_bytes.extend_from_slice(&sample.0.to_le_bytes());
            float_bytes.extend_from_slice(&sample.1.to_le_bytes());
        }
        let float_samples =
            copy_audio_samples(&float_bytes, &float_info, 8).expect("copy float samples");
        assert!((float_samples[0] - 0.125).abs() < 0.001);
        assert!((float_samples[1] - 0.5).abs() < 0.001);

        let integer_info = gst_audio::AudioInfo::builder(gst_audio::AudioFormat::S16le, 48_000, 2)
            .layout(gst_audio::AudioLayout::Interleaved)
            .build()
            .expect("integer audio info");
        let mut integer_bytes = Vec::new();
        for sample in [(16_384_i16, -8_192_i16), (i16::MAX, 0_i16)] {
            integer_bytes.extend_from_slice(&sample.0.to_le_bytes());
            integer_bytes.extend_from_slice(&sample.1.to_le_bytes());
        }
        let integer_samples =
            copy_audio_samples(&integer_bytes, &integer_info, 8).expect("copy integer samples");
        assert!((integer_samples[0] - 0.125).abs() < 0.001);
        assert!((integer_samples[1] - 0.5).abs() < 0.001);
    }

    #[test]
    fn visualizer_fft_distinguishes_silence_from_bounded_energy() {
        let mut fft = VisualizerFft::new();
        fft.push_samples(&vec![0.0; VISUALIZER_FFT_SIZE]);
        let silence = fft.levels();
        assert!(silence.iter().all(|level| (0.0..=0.01).contains(level)));

        fft.clear();
        let samples = (0..VISUALIZER_FFT_SIZE)
            .map(|index| {
                let phase =
                    2.0 * std::f32::consts::PI * 16.0 * index as f32 / VISUALIZER_FFT_SIZE as f32;
                phase.sin() * 0.8
            })
            .collect::<Vec<_>>();
        fft.push_samples(&samples);
        let levels = fft.levels();
        assert!(levels.iter().copied().fold(0.0_f64, f64::max) > 0.3);
        assert!(levels.iter().all(|level| (0.0..=1.0).contains(level)));
    }
}
