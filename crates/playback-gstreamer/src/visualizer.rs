//! Live visualizer sampling and bounded spectrum analysis.

use super::engine::{EventMailbox, PipelineId, SharedBackendState, Slot, push_event};
use super::lock_recover;
#[cfg(test)]
use audio_processing::copy_audio_samples;
use audio_processing::{VisualizerFft, copy_visualizer_samples};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
use playback::{BackendEvent, RunId};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::{debug, warn};

const VISUALIZER_CHANNEL_CAPACITY: usize = 2;
const VISUALIZER_COPY_FRAMES: usize = 32_768;
const MEASUREMENT_COPY_FRAMES: usize = 4_096;
#[derive(Clone)]
pub(super) struct VisualizerTap {
    pub(super) ignore_volume: bool,
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
    sender: SyncSender<VisualizerFrame>,
    output: Option<String>,
}
impl VisualizerTap {
    pub(super) fn measurement_key(&self) -> Option<(PipelineId, RunId, String)> {
        Some((self.pipeline_id, self.run, self.output.clone()?))
    }
    pub(super) fn measurement(mut self, output: String) -> Self {
        self.output = Some(output);
        self
    }

    pub(super) fn install(&self, pad: &gst::Pad) -> Option<gst::PadProbeId> {
        if self.output.is_some() {
            return self.install_measurement(pad);
        }
        let tap = self.clone();
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let Some(buffer) = info.buffer() else {
                return gst::PadProbeReturn::Ok;
            };
            let Some((samples, sample_rate)) =
                copy_visualizer_samples(pad, buffer, VISUALIZER_COPY_FRAMES)
            else {
                return gst::PadProbeReturn::Ok;
            };
            let frame = VisualizerFrame {
                slot: tap.slot,
                pipeline_id: tap.pipeline_id,
                run: tap.run,
                samples,
                sample_rate,
                measurement: None,
            };
            match tap.sender.try_send(frame) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return gst::PadProbeReturn::Remove,
            }
            gst::PadProbeReturn::Ok
        })
    }
    fn install_measurement(&self, pad: &gst::Pad) -> Option<gst::PadProbeId> {
        let tap = self.clone();
        let info = pad
            .current_caps()
            .and_then(|caps| gst_audio::AudioInfo::from_caps(&caps).ok());
        let pool = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
        if let Some(info) = &info {
            *lock_recover(&pool) = (0..3)
                .map(|_| Vec::with_capacity(MEASUREMENT_COPY_FRAMES * info.channels() as usize))
                .collect();
        }
        let info = Mutex::new(info);
        pad.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
            move |_, probe| {
                let mut info = lock_recover(&info);
                if let Some(event) = probe.event()
                    && let gst::EventView::Caps(caps) = event.view()
                {
                    *info = gst_audio::AudioInfo::from_caps(caps.caps()).ok();
                    if let Some(info) = info.as_ref() {
                        *lock_recover(&pool) = (0..3)
                            .map(|_| {
                                Vec::with_capacity(
                                    MEASUREMENT_COPY_FRAMES * info.channels() as usize,
                                )
                            })
                            .collect();
                    }
                }
                let Some(buffer) = probe.buffer() else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(info) = info.as_ref() else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(mut samples) = pool.try_lock().ok().and_then(|mut pool| pool.pop()) else {
                    return gst::PadProbeReturn::Ok;
                };
                if samples.capacity() < MEASUREMENT_COPY_FRAMES * info.channels() as usize {
                    return gst::PadProbeReturn::Ok;
                }
                samples.clear();
                if let Ok(map) = buffer.map_readable() {
                    let sample_size = (info.width() / 8) as usize;
                    let count = (map.len() / sample_size)
                        .min(MEASUREMENT_COPY_FRAMES * info.channels() as usize);
                    for bytes in map.as_slice()[..count * sample_size].chunks_exact(sample_size) {
                        let value = match info.format() {
                            gst_audio::AudioFormat::F64le => {
                                f64::from_le_bytes(bytes.try_into().unwrap()) as f32
                            }
                            gst_audio::AudioFormat::F64be => {
                                f64::from_be_bytes(bytes.try_into().unwrap()) as f32
                            }
                            gst_audio::AudioFormat::F32le => {
                                f32::from_le_bytes(bytes.try_into().unwrap())
                            }
                            gst_audio::AudioFormat::F32be => {
                                f32::from_be_bytes(bytes.try_into().unwrap())
                            }
                            _ => 0.0,
                        };
                        samples.push(if value.is_finite() { value } else { 0.0 });
                    }
                }
                let frame = VisualizerFrame {
                    slot: tap.slot,
                    pipeline_id: tap.pipeline_id,
                    run: tap.run,
                    samples,
                    sample_rate: info.rate(),
                    measurement: Some((
                        tap.output.clone().unwrap(),
                        info.channels() as usize,
                        Arc::clone(&pool),
                    )),
                };
                if let Err(error) = tap.sender.try_send(frame) {
                    let frame = match error {
                        TrySendError::Full(frame) | TrySendError::Disconnected(frame) => frame,
                    };
                    lock_recover(&pool).push(frame.samples);
                }
                gst::PadProbeReturn::Ok
            },
        )
    }
}
struct VisualizerFrame {
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
    samples: Vec<f32>,
    sample_rate: u32,
    measurement: Option<(String, usize, Arc<Mutex<Vec<Vec<f32>>>>)>,
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

    pub(super) fn tap(
        &self,
        slot: Slot,
        pipeline_id: PipelineId,
        run: RunId,
        ignore_volume: bool,
    ) -> VisualizerTap {
        VisualizerTap {
            ignore_volume,
            slot,
            pipeline_id,
            run,
            sender: self.sender.clone(),
            output: None,
        }
    }
}
fn run_visualizer_worker(
    receiver: Receiver<VisualizerFrame>,
    events: Arc<Mutex<EventMailbox>>,
    shared: Arc<Mutex<SharedBackendState>>,
) {
    let mut fft = VisualizerFft::new();
    let mut current_stream = None;
    let mut measured_stream = None;
    let mut channel_ffts = Vec::<VisualizerFft>::new();
    let mut last_spectrum = std::time::Instant::now();
    while let Ok(mut frame) = receiver.recv() {
        if let Some((output, channels, pool)) = frame.measurement.take() {
            let live = {
                let shared = lock_recover(&shared);
                shared.pipeline_is_current(frame.slot, frame.pipeline_id)
                    && shared
                        .current
                        .as_ref()
                        .is_some_and(|current| current.run == frame.run)
                    && shared
                        .processing
                        .as_ref()
                        .is_some_and(|p| p.spectrum && p.output == output)
            };
            if live {
                let stream = (frame.pipeline_id, frame.run, frame.sample_rate, channels);
                if measured_stream != Some(stream) {
                    channel_ffts = (0..channels).map(|_| VisualizerFft::new()).collect();
                    measured_stream = Some(stream);
                }
                for (channel, fft) in channel_ffts.iter_mut().enumerate() {
                    fft.push_channel_samples(&frame.samples, channels, channel);
                }
                if last_spectrum.elapsed() >= std::time::Duration::from_millis(33) {
                    let mut powers = Vec::<f64>::new();
                    for fft in &mut channel_ffts {
                        if let Some(channel) = fft.spectrum_power() {
                            if powers.is_empty() {
                                powers = channel;
                            } else {
                                for (sum, value) in powers.iter_mut().zip(channel) {
                                    *sum += value;
                                }
                            }
                        }
                    }
                    if !powers.is_empty() {
                        let spectrum_db = powers
                            .into_iter()
                            .map(|p| (10.0 * (p / channels as f64).max(1e-16).log10()).max(-160.0))
                            .collect();
                        push_event(
                            &events,
                            BackendEvent::ProcessingMeter {
                                output,
                                peak_db: Vec::new(),
                                spectrum_db,
                            },
                        );
                        last_spectrum = std::time::Instant::now();
                    }
                }
            }
            let mut available = lock_recover(&pool);
            if available.len() < 3 {
                available.push(frame.samples);
            }
            continue;
        }
        if !visualizer_pipeline_is_live(&shared, frame.slot, frame.pipeline_id, frame.run) {
            continue;
        }
        let stream = (frame.pipeline_id, frame.run, frame.sample_rate);
        if current_stream != Some(stream) {
            debug!(
                run = frame.run.get(),
                sample_rate = frame.sample_rate,
                block_frames = frame.samples.len(),
                "visualizer audio stream started"
            );
            fft.clear();
            current_stream = Some(stream);
        }
        fft.set_size(lock_recover(&shared).settings.visualizer_fft_size as usize);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visualizer_pcm_copy_mixes_supported_stereo_formats() {
        gst::init().expect("initialize GStreamer");
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
    fn every_raw_format_preserves_mono_amplitude_and_copy_bound() {
        gst::init().expect("initialize GStreamer");
        for format in gst_audio::AUDIO_FORMATS_ALL.iter().copied() {
            let info = gst_audio::AudioInfo::builder(format, 44_100, 2)
                .build()
                .expect("raw stereo audio info");
            let format_info = info.format_info();
            let canonical = if format_info.is_float() {
                [0.5_f64, -0.25, -0.5, 0.25, 0.0, 0.0]
                    .into_iter()
                    .flat_map(f64::to_ne_bytes)
                    .collect::<Vec<_>>()
            } else {
                [
                    1_073_741_824_i32,
                    -536_870_912,
                    -1_073_741_824,
                    536_870_912,
                    0,
                    0,
                ]
                .into_iter()
                .flat_map(i32::to_ne_bytes)
                .collect::<Vec<_>>()
            };
            let mut bytes = vec![0; 3 * info.bpf() as usize];
            format_info.pack(gst_audio::AudioPackFlags::empty(), &mut bytes, &canonical);
            let samples = copy_audio_samples(&bytes, &info, 2).expect("raw PCM samples");
            assert_eq!(samples.len(), 2, "{format:?}");
            for (actual, expected) in samples.iter().zip([0.125, -0.125]) {
                assert!((actual - expected).abs() < 0.005, "{format:?}: {samples:?}");
            }
        }
    }
}
