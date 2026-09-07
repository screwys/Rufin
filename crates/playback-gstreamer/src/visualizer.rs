//! Live visualizer sampling and bounded spectrum analysis.

use super::engine::{EventMailbox, PipelineId, SharedBackendState, Slot, push_event};
use super::lock_recover;
use audio_processing::{VisualizerFft, copy_audio_samples};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
use playback::{BackendEvent, RunId};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::warn;

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
