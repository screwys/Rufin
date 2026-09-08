//! Live visualizer sampling and bounded spectrum analysis.

use super::engine::{EventMailbox, PipelineId, SharedBackendState, Slot, push_event};
use super::lock_recover;
use audio_processing::VisualizerFft;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
use playback::{BackendEvent, RunId};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::{debug, warn};

const VISUALIZER_CHANNEL_CAPACITY: usize = 2;
const VISUALIZER_COPY_FRAMES: usize = 4_096;
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
    if !info.is_valid() || info.layout() != gst_audio::AudioLayout::Interleaved {
        return None;
    }
    let channels = info.channels() as usize;
    let frame_size = info.bpf() as usize;
    let frames = (bytes.len() / frame_size).min(max_frames);
    if frames == 0 {
        return None;
    }
    let format = info.format_info();
    let sample_size = if format.is_float() { 8 } else { 4 };
    let mut unpacked = vec![0; frames * channels * sample_size];
    // GStreamer handles integer alignment, signedness and byte order. Its
    // canonical unpacked formats are native-endian S32 and F64.
    format.unpack(
        gst_audio::AudioPackFlags::empty(),
        &mut unpacked,
        &bytes[..frames * frame_size],
    );
    Some(
        unpacked
            .chunks_exact(channels * sample_size)
            .map(|frame| {
                let total: f32 = frame
                    .chunks_exact(sample_size)
                    .map(|sample| {
                        let value = if format.is_float() {
                            f64::from_ne_bytes(sample.try_into().unwrap()) as f32
                        } else {
                            i32::from_ne_bytes(sample.try_into().unwrap()) as f32 / 2_147_483_648.0
                        };
                        if value.is_finite() {
                            value.clamp(-1.0, 1.0)
                        } else {
                            0.0
                        }
                    })
                    .sum();
                total / channels as f32
            })
            .collect(),
    )
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
            debug!(
                run = frame.run.get(),
                sample_rate = frame.sample_rate,
                block_frames = frame.samples.len(),
                "visualizer audio stream started"
            );
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
