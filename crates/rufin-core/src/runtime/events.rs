use async_channel::Receiver;

use super::{ReleaseUpdate, SourceEvent, VisualizerPublication, WaveformProjection};

pub struct ProductReceivers {
    pub source: Receiver<SourceEvent>,
    pub downloads: Receiver<crate::downloads::DownloadEvent>,
    pub visualizer: Receiver<VisualizerPublication>,
    pub waveform: Receiver<WaveformProjection>,
    pub lyrics: Receiver<crate::lyrics::LyricsEvent>,
    pub release_updates: Receiver<ReleaseUpdate>,
}
