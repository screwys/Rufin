use async_channel::Receiver;

use super::source::DiscoveryUpdate;
use super::{ReleaseUpdate, SourceEvent, VisualizerPublication, WaveformProjection};

pub struct ProductReceivers {
    pub source: Receiver<SourceEvent>,
    pub source_discovery: Receiver<DiscoveryUpdate>,
    pub downloads: Receiver<downloads::DownloadEvent>,
    pub playback: Receiver<playback::PlaybackProjection>,
    pub visualizer: Receiver<VisualizerPublication>,
    pub waveform: Receiver<WaveformProjection>,
    pub lyrics: Receiver<crate::lyrics::LyricsEvent>,
    pub release_updates: Receiver<ReleaseUpdate>,
}
