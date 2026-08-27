//! Live visualizer sampling and bounded spectrum analysis.

use super::engine::{EventMailbox, PipelineId, SharedBackendState, Slot, push_event};
#[cfg(test)]
use super::ensure_gstreamer_initialized;
use super::lock_recover;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
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
const VISUALIZER_BAND_COUNT: usize = 52;
const VISUALIZER_MIN_FREQUENCY_HZ: f32 = 30.0;
const VISUALIZER_MAX_FREQUENCY_HZ: f32 = 12_000.0;
const VISUALIZER_VISIBLE_FLOOR: f64 = 0.28;
const VISUALIZER_VISIBLE_CEILING: f64 = 0.95;
const VISUALIZER_NORMALIZED_PEAK: f64 = 0.90;
const VISUALIZER_MAX_GAIN: f64 = 2.35;
const VISUALIZER_HIGH_BAND_TILT: f64 = 0.65;
const VISUALIZER_MIN_EMIT_INTERVAL: Duration = Duration::from_millis(33);
const VISUALIZER_NOISE_FLOOR_DB: f32 = -72.0;
const VISUALIZER_CEILING_DB: f32 = -6.0;

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
            let Some((samples, sample_rate)) = copy_visualizer_samples(pad, buffer) else {
                return gst::PadProbeReturn::Ok;
            };
            let frame = VisualizerFrame {
                slot: tap.slot,
                pipeline_id: tap.pipeline_id,
                run: tap.run,
                samples,
                sample_rate,
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
    sample_rate: u32,
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
fn copy_visualizer_samples(pad: &gst::Pad, buffer: &gst::Buffer) -> Option<(Vec<f32>, u32)> {
    let caps = pad.current_caps()?;
    let info = gst_audio::AudioInfo::from_caps(caps.as_ref()).ok()?;
    let map = buffer.map_readable().ok()?;
    copy_audio_samples(map.as_slice(), &info, VISUALIZER_COPY_FRAMES)
        .map(|samples| (samples, info.rate()))
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
    let mut current_stream = None;
    while let Ok(frame) = receiver.recv() {
        if !visualizer_pipeline_is_live(&shared, frame.slot, frame.pipeline_id, frame.run) {
            continue;
        }
        let stream = (frame.pipeline_id, frame.run, frame.sample_rate);
        if current_stream != Some(stream) {
            fft.clear();
            current_stream = Some(stream);
        }
        fft.push_samples(&frame.samples);
        let Some(levels) = fft.maybe_levels(frame.sample_rate) else {
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
    band_sample_rate: u32,
    band_ranges: Vec<(usize, usize)>,
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
            band_sample_rate: 0,
            band_ranges: Vec::new(),
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

    fn maybe_levels(&mut self, sample_rate: u32) -> Option<Vec<f64>> {
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
        Some(self.levels(sample_rate))
    }

    fn levels(&mut self, sample_rate: u32) -> Vec<f64> {
        for ((slot, sample), window) in self
            .input
            .iter_mut()
            .zip(self.samples.iter().copied())
            .zip(self.window.iter().copied())
        {
            *slot = Complex::new(sample * window, 0.0);
        }
        self.fft.process(&mut self.input);
        if self.band_sample_rate != sample_rate {
            self.band_ranges = visualizer_band_ranges(sample_rate);
            self.band_sample_rate = sample_rate;
        }
        fft_band_levels(&self.input, &self.band_ranges)
    }
}

fn visualizer_band_ranges(sample_rate: u32) -> Vec<(usize, usize)> {
    if sample_rate == 0 {
        return Vec::new();
    }
    let half = VISUALIZER_FFT_SIZE / 2;
    let maximum = VISUALIZER_MAX_FREQUENCY_HZ.min(sample_rate as f32 / 2.0);
    let min_erb = frequency_to_erb(VISUALIZER_MIN_FREQUENCY_HZ.min(maximum));
    let max_erb = frequency_to_erb(maximum);
    (0..VISUALIZER_BAND_COUNT)
        .map(|band| {
            let lower_t = band as f32 / VISUALIZER_BAND_COUNT as f32;
            let upper_t = (band + 1) as f32 / VISUALIZER_BAND_COUNT as f32;
            let lower_hz = erb_to_frequency(min_erb + (max_erb - min_erb) * lower_t);
            let upper_hz = erb_to_frequency(min_erb + (max_erb - min_erb) * upper_t);
            let lower = ((lower_hz * VISUALIZER_FFT_SIZE as f32 / sample_rate as f32).floor()
                as usize)
                .clamp(1, half - 1);
            let upper = ((upper_hz * VISUALIZER_FFT_SIZE as f32 / sample_rate as f32).ceil()
                as usize)
                .clamp(lower + 1, half);
            (lower, upper)
        })
        .collect()
}

fn fft_band_levels(input: &[Complex<f32>], ranges: &[(usize, usize)]) -> Vec<f64> {
    let mut levels = ranges
        .iter()
        .map(|&(lower, upper)| {
            let mut total = 0.0;
            let mut peak = 0.0_f64;
            for value in &input[lower..upper] {
                let level = fft_magnitude_level(value);
                total += level;
                peak = peak.max(level);
            }
            let average = total / (upper - lower) as f64;
            average * 0.4 + peak * 0.6
        })
        .collect::<Vec<_>>();
    let last = levels.len().saturating_sub(1).max(1) as f64;
    for (index, level) in levels.iter_mut().enumerate() {
        *level *= 1.0 + VISUALIZER_HIGH_BAND_TILT * index as f64 / last;
    }
    let peak = levels.iter().copied().fold(0.0_f64, f64::max);
    if peak < 0.08 {
        levels.fill(0.0);
        return levels;
    }
    let gain = (VISUALIZER_NORMALIZED_PEAK / peak).min(VISUALIZER_MAX_GAIN);
    for level in &mut levels {
        *level = visible_visualizer_level((*level * gain).clamp(0.0, 1.0));
    }
    levels
}

fn fft_magnitude_level(value: &Complex<f32>) -> f64 {
    let magnitude = value.norm() / VISUALIZER_FFT_SIZE as f32;
    let db = 20.0 * magnitude.max(1.0e-6).log10();
    let level = ((db - VISUALIZER_NOISE_FLOOR_DB)
        / (VISUALIZER_CEILING_DB - VISUALIZER_NOISE_FLOOR_DB))
        .clamp(0.0, 1.0);
    f64::from(level.powf(1.25))
}

fn visible_visualizer_level(level: f64) -> f64 {
    ((level - VISUALIZER_VISIBLE_FLOOR) / (VISUALIZER_VISIBLE_CEILING - VISUALIZER_VISIBLE_FLOOR))
        .clamp(0.0, 1.0)
}

fn frequency_to_erb(frequency: f32) -> f32 {
    21.4 * (1.0 + 0.00437 * frequency).log10()
}

fn erb_to_frequency(erb: f32) -> f32 {
    (10.0_f32.powf(erb / 21.4) - 1.0) / 0.00437
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
        let silence = fft.levels(48_000);
        assert_eq!(silence, vec![0.0; VISUALIZER_BAND_COUNT]);

        fft.clear();
        let samples = (0..VISUALIZER_FFT_SIZE)
            .map(|index| {
                let phase =
                    2.0 * std::f32::consts::PI * 16.0 * index as f32 / VISUALIZER_FFT_SIZE as f32;
                phase.sin() * 0.8
            })
            .collect::<Vec<_>>();
        fft.push_samples(&samples);
        let levels = fft.levels(48_000);
        assert!(levels.iter().copied().fold(0.0_f64, f64::max) > 0.3);
        assert!(levels.iter().all(|level| (0.0..=1.0).contains(level)));
    }

    #[test]
    fn visualizer_bands_follow_frequency_across_sample_rates() {
        let tone = |sample_rate: u32, frequency: f32| {
            let mut fft = VisualizerFft::new();
            let samples = (0..VISUALIZER_FFT_SIZE)
                .map(|index| {
                    let phase =
                        2.0 * std::f32::consts::PI * frequency * index as f32 / sample_rate as f32;
                    phase.sin() * 0.8
                })
                .collect::<Vec<_>>();
            fft.push_samples(&samples);
            fft.levels(sample_rate)
        };
        let peak_band = |levels: &[f64]| {
            levels
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| left.total_cmp(right))
                .map(|(index, _)| index)
                .expect("visualizer band")
        };

        let at_44k = peak_band(&tone(44_100, 1_000.0));
        let at_48k = peak_band(&tone(48_000, 1_000.0));
        assert!(at_44k.abs_diff(at_48k) <= 1);
        assert!(peak_band(&tone(48_000, 8_000.0)) > at_48k);
    }

    #[test]
    fn visualizer_gate_drops_residual_energy() {
        assert_eq!(visible_visualizer_level(VISUALIZER_VISIBLE_FLOOR), 0.0);
        assert_eq!(visible_visualizer_level(0.0), 0.0);
        assert_eq!(visible_visualizer_level(VISUALIZER_VISIBLE_CEILING), 1.0);
    }

    #[test]
    fn visualizer_normalizes_quiet_bounded_energy() {
        let mut fft = VisualizerFft::new();
        let samples = (0..VISUALIZER_FFT_SIZE)
            .map(|index| {
                let phase =
                    2.0 * std::f32::consts::PI * 48.0 * index as f32 / VISUALIZER_FFT_SIZE as f32;
                phase.sin() * 0.02
            })
            .collect::<Vec<_>>();
        fft.push_samples(&samples);
        let levels = fft.levels(48_000);
        let peak = levels.iter().copied().fold(0.0_f64, f64::max);

        assert!(peak > 0.5);
        assert!(peak < 0.95);
        assert!(levels.contains(&0.0));
    }
}
