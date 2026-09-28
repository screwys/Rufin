use super::audio::{AudioGraph, SharedQueuedLoudness};
use super::engine::{
    GaplessPlayback, PipelineId, PreparedRun, SharedBackendState, Slot, handle_about_to_finish,
};
use super::visualizer::VisualizerTap;
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SourceClock {
    origin_millis: u64,
    end_millis: Option<u64>,
}

impl SourceClock {
    pub(super) fn from_stream(stream: &ResolvedStream) -> Self {
        Self {
            origin_millis: stream.start_millis(),
            end_millis: stream.end_millis(),
        }
    }

    pub(super) fn physical_seek(self, logical_millis: u64) -> u64 {
        let physical = self.origin_millis.saturating_add(logical_millis);
        self.end_millis
            .map_or(physical, |end_millis| physical.min(end_millis))
    }

    pub(super) fn logical_position(self, physical_millis: u64) -> u64 {
        let logical = physical_millis.saturating_sub(self.origin_millis);
        self.fixed_duration()
            .map_or(logical, |duration| logical.min(duration))
    }

    pub(super) fn logical_duration(self, physical_duration_millis: u64) -> u64 {
        self.fixed_duration().unwrap_or(physical_duration_millis)
    }

    pub(super) fn remaining(
        self,
        physical_position_millis: u64,
        physical_duration_millis: u64,
    ) -> u64 {
        self.logical_duration(physical_duration_millis)
            .saturating_sub(self.logical_position(physical_position_millis))
    }

    pub(super) fn end_millis(self) -> Option<u64> {
        self.end_millis
    }

    pub(super) fn fixed_duration(self) -> Option<u64> {
        self.end_millis
            .map(|end_millis| end_millis.saturating_sub(self.origin_millis))
    }
}
pub(super) struct PlayerPipeline {
    name: String,
    shared: Arc<Mutex<SharedBackendState>>,
    native: Option<(gst::Element, gst::Bus)>,
    id: Option<PipelineId>,
    trust_invalid_certificate: Arc<AtomicBool>,
    module_decoder: Arc<AtomicBool>,
    about_to_finish_id: Option<glib::SignalHandlerId>,
    audio_graph: Option<AudioGraph>,
    visualizer_probe: Option<gst::PadProbeId>,
    current_stream: Option<PreparedStream>,
    queued_loudness: SharedQueuedLoudness,
    gapless: Arc<Mutex<GaplessPlayback>>,
    playback_rate: f64,
    segment: Arc<Mutex<SegmentPlayback>>,
    requested_state: Cell<gst::State>,
    buffering_percent: Cell<Option<u8>>,
    live: Cell<bool>,
}

#[derive(Debug, PartialEq)]
pub(super) enum AboutToFinishAction {
    Preload(Box<PreparedNext>),
    Ignore,
}

#[derive(Default)]
struct SegmentPlayback {
    seek: Option<gst::Seqnum>,
    done: bool,
    starts_stream: bool,
}

impl PlayerPipeline {
    pub(super) fn new(name: &str, shared: Arc<Mutex<SharedBackendState>>) -> Self {
        Self {
            name: name.to_string(),
            shared,
            native: None,
            id: None,
            trust_invalid_certificate: Arc::new(AtomicBool::new(false)),
            module_decoder: Arc::new(AtomicBool::new(false)),
            about_to_finish_id: None,
            audio_graph: None,
            visualizer_probe: None,
            current_stream: None,
            queued_loudness: Arc::new(Mutex::new(TrackLoudness::default())),
            gapless: Arc::new(Mutex::new(GaplessPlayback::default())),
            playback_rate: DEFAULT_PLAYBACK_RATE,
            segment: Arc::new(Mutex::new(SegmentPlayback::default())),
            requested_state: Cell::new(gst::State::Null),
            buffering_percent: Cell::new(None),
            live: Cell::new(false),
        }
    }

    fn native(&self) -> Result<&(gst::Element, gst::Bus), String> {
        self.native
            .as_ref()
            .ok_or_else(|| format!("GStreamer player {} is not active", self.name))
    }

    fn clock(&self) -> SourceClock {
        self.current_stream
            .as_ref()
            .map(|stream| SourceClock::from_stream(stream))
            .unwrap_or_default()
    }

    pub(super) fn play_item(
        &mut self,
        id: PipelineId,
        slot: Slot,
        item: &PreparedRun,
        settings: &BackendAudioSettings,
        volume: f64,
        muted: bool,
        playback_rate: f64,
        startup_state: gst::State,
    ) -> Result<(), String> {
        if self
            .audio_graph
            .as_ref()
            .is_some_and(|graph| !graph.uses_output(settings.audio_output.as_deref()))
        {
            // Keep the working output until its replacement has opened.
            let mut replacement = Self::new(&self.name, Arc::clone(&self.shared));
            replacement.play_item(
                id,
                slot,
                item,
                settings,
                volume,
                muted,
                playback_rate,
                startup_state,
            )?;
            *self = replacement;
            return Ok(());
        }
        self.stop();
        self.initialize()?;
        self.id = Some(id);
        self.current_stream = Some(item.stream.clone());
        self.playback_rate = sanitize_playback_rate(playback_rate);
        self.connect_about_to_finish(id, slot, Arc::clone(&self.shared));
        let result = (|| {
            self.configure_audio(settings)?;
            self.set_stream(&item.stream)?;
            self.set_output_volume(volume, muted);
            self.set_state(startup_state)?;
            self.set_output_volume(volume, muted);
            Ok(())
        })();
        if result.is_err() {
            self.stop();
        }
        result
    }

    pub(super) fn gapless(&self) -> Option<&Arc<Mutex<GaplessPlayback>>> {
        self.native.as_ref().map(|_| &self.gapless)
    }

    pub(super) fn has_pending_gapless(&self) -> bool {
        self.gapless
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pending
            .is_some()
    }

    #[cfg(test)]
    pub(super) fn audio_output(&self) -> Option<gst::Element> {
        self.audio_graph
            .as_ref()
            .map(|graph| graph.output().clone())
    }

    #[cfg(test)]
    pub(super) fn output_volume_state(&self) -> Option<(f64, bool)> {
        let (pipeline, _) = self.native.as_ref()?;
        Some((
            pipeline.property::<f64>("volume"),
            pipeline.property::<bool>("mute"),
        ))
    }

    #[cfg(test)]
    pub(super) fn has_or_targets_state(&self, state: gst::State) -> bool {
        self.native.as_ref().is_some_and(|(pipeline, _)| {
            let (result, current, pending) = pipeline.state(gst::ClockTime::ZERO);
            result.is_ok() && (current == state || pending == state)
        })
    }

    pub(super) fn buffering_percent(&self) -> Option<u8> {
        self.buffering_percent.get()
    }

    pub(super) fn is_playing(&self) -> bool {
        self.native
            .as_ref()
            .is_some_and(|(pipeline, _)| pipeline.current_state() == gst::State::Playing)
    }

    pub(super) fn set_buffering(
        &self,
        percent: u8,
        mode: gst::BufferingMode,
    ) -> Result<(), String> {
        let was_buffering = self.is_buffering();
        self.buffering_percent.set(Some(percent));
        if mode == gst::BufferingMode::Live {
            self.live.set(true);
        }
        if was_buffering != self.is_buffering() && self.requested_state.get() == gst::State::Playing
        {
            self.apply_requested_state()?;
        }
        Ok(())
    }

    pub(super) fn owns_audio_output(&self, name: &str) -> bool {
        let prefix = format!("{}-audio-output", self.name);
        name == prefix || name.starts_with(&format!("{prefix}-"))
    }

    pub(super) fn segment_done(&self, seqnum: gst::Seqnum) {
        let mut segment = self.segment.lock().unwrap_or_else(|p| p.into_inner());
        // Some parsers use a fresh sequence number for SEGMENT_DONE.
        if segment.seek.is_some_and(|seek| seqnum >= seek) {
            segment.done = true;
        }
    }

    pub(super) fn take_segment_done(&self) -> bool {
        std::mem::take(&mut self.segment.lock().unwrap_or_else(|p| p.into_inner()).done)
    }

    pub(super) fn continue_segment(&self, next: Option<&PreparedStream>) -> Result<(), String> {
        let (start, end, flags) = if let Some(next) = next {
            *self
                .queued_loudness
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = next.loudness.clone();
            let flags = if next.end_millis().is_some() {
                gst::SeekFlags::ACCURATE | gst::SeekFlags::SEGMENT
            } else {
                gst::SeekFlags::ACCURATE
            };
            (next.start_millis(), next.end_millis(), flags)
        } else {
            // An empty non-segment seek drains the tail and delivers EOS.
            let end = self
                .clock()
                .end_millis()
                .ok_or("No bounded segment to finish")?;
            (end, Some(end), gst::SeekFlags::ACCURATE)
        };
        self.seek_segment(start, end, flags, next.is_some())
    }

    pub(super) fn physical_seek_target(&self, millis: u64) -> u64 {
        self.clock().physical_seek(millis)
    }
    pub(super) fn logical_position(&self, millis: u64) -> u64 {
        self.clock().logical_position(millis)
    }
    pub(super) fn logical_duration(&self, millis: u64) -> u64 {
        self.clock().logical_duration(millis)
    }
    pub(super) fn logical_remaining(&self, position: u64, duration: u64) -> u64 {
        self.clock().remaining(position, duration)
    }
    pub(super) fn fixed_duration(&self) -> Option<u64> {
        self.clock().fixed_duration()
    }

    pub(super) fn activate_stream(&mut self, stream: &PreparedStream) {
        self.current_stream = Some(stream.clone());
        *self
            .queued_loudness
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = stream.loudness.clone();
    }

    pub(super) fn has_session(&self) -> bool {
        self.id.is_some()
    }

    pub(super) fn allows_preloading(&self) -> bool {
        !self.module_decoder.load(Ordering::Relaxed)
    }

    pub(super) fn pop_bus_message(&self) -> Option<(PipelineId, gst::Message)> {
        let id = self.id?;
        let (_, bus) = self.native.as_ref()?;
        let mut message = bus.pop()?;
        // Use the latest consecutive refill update to avoid an unnecessary pause.
        if message.type_() == gst::MessageType::Buffering {
            while bus.peek().is_some_and(|next| {
                next.type_() == gst::MessageType::Buffering && next.src() == message.src()
            }) {
                message = bus.pop()?;
            }
        }
        Some((id, message))
    }

    pub(super) fn message_source_is_pipeline(&self, message: &gst::Message) -> bool {
        self.native.as_ref().is_some_and(|(pipeline, _)| {
            message
                .src()
                .is_some_and(|source| source == pipeline.upcast_ref::<gst::Object>())
        })
    }

    fn initialize(&mut self) -> Result<(), String> {
        if self.native.is_some() {
            return Ok(());
        }
        let name = &self.name;
        let pipeline = make_playbin(name)?;
        let bus = pipeline
            .bus()
            .ok_or_else(|| "GStreamer playbin did not expose a bus".to_string())?;
        let fakesink = gst::ElementFactory::make("fakesink")
            .name(format!("{name}-video-sink"))
            .build()
            .map_err(|error| error.to_string())?;
        configure_playbin_for_audio(&pipeline);
        pipeline.set_property("video-sink", &fakesink);
        let certificate_policy = Arc::clone(&self.trust_invalid_certificate);
        configure_sources(&pipeline, move || certificate_policy.load(Ordering::SeqCst));

        let module_for_setup = Arc::clone(&self.module_decoder);
        pipeline.connect("element-setup", false, move |values| {
            let element = values[1].get::<gst::Element>().expect("playbin element-setup element");
            if let Some(factory) = element.factory()
                && factory.klass().split('/').any(|class| class == "Decoder")
                && factory.static_pad_templates().iter().any(|pad| {
                    pad.direction() == gst::PadDirection::Sink
                        && pad.caps().iter().any(|caps| caps.name() == "audio/x-mod")
                })
            {
                info!(decoder = %factory.name(), "isolating tracker decoder from seeks and preloading");
                module_for_setup.store(true, Ordering::Relaxed);
            }
            None
        });

        self.native = Some((pipeline, bus));
        Ok(())
    }

    fn connect_about_to_finish(
        &mut self,
        id: PipelineId,
        slot: Slot,
        shared: Arc<Mutex<SharedBackendState>>,
    ) {
        let element = &self.native.as_ref().expect("initialized playbin").0;
        let pipeline = element.downgrade();
        let queued_loudness = Arc::clone(&self.queued_loudness);
        let gapless = Arc::clone(&self.gapless);
        let certificate_policy = Arc::clone(&self.trust_invalid_certificate);
        let module_for_signal = Arc::clone(&self.module_decoder);
        self.about_to_finish_id = Some(element.connect("about-to-finish", false, move |_| {
            if module_for_signal.load(Ordering::Relaxed) {
                return None;
            }
            let pipeline = pipeline.upgrade()?;
            handle_about_to_finish(
                &pipeline,
                &shared,
                &gapless,
                &queued_loudness,
                &certificate_policy,
                slot,
                id,
            );
            None
        }));
    }

    pub(super) fn configure_audio(
        &mut self,
        settings: &BackendAudioSettings,
    ) -> Result<(), String> {
        if self.current_stream.is_none() {
            return Ok(());
        }
        if let Some(graph) = self.audio_graph.as_mut()
            && graph.reconfigure(settings, self.playback_rate)?
        {
            return Ok(());
        }
        self.clear_visualizer_tap();
        let graph = AudioGraph::new(
            settings,
            self.playback_rate,
            self.current_stream
                .as_ref()
                .map(|stream| stream.loudness.clone())
                .unwrap_or_default(),
            Arc::clone(&self.queued_loudness),
        )?;
        graph
            .output()
            .set_property("name", format!("{}-audio-output", self.native()?.0.name()));
        let segment = Arc::clone(&self.segment);
        let shared = Arc::clone(&self.shared);
        let gapless = Arc::clone(&self.gapless);
        graph
            .root()
            .static_pad("sink")
            .expect("audio graph input")
            .add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
                let Some(event) = info.event_mut() else {
                    return gst::PadProbeReturn::Ok;
                };
                if matches!(event.view(), gst::EventView::StreamStart(_)) {
                    let shared = lock_recover(&shared);
                    let gapless = lock_recover(&gapless);
                    if gapless
                        .pending
                        .as_ref()
                        .is_some_and(|pending| shared.next.as_ref() != Some(pending))
                    {
                        // Let the current song finish, then end output before a
                        // removed preload can send any audio to the sink.
                        *event = gst::event::Eos::new();
                    }
                }
                if matches!(event.view(), gst::EventView::Segment(_)) {
                    let starts_stream = {
                        let mut segment = segment.lock().unwrap_or_else(|p| p.into_inner());
                        segment.seek == Some(event.seqnum())
                            && std::mem::take(&mut segment.starts_stream)
                    };
                    if starts_stream {
                        // A non-flushing segment seek keeps the decoder and sink. Mark
                        // the logical track boundary for gain, tags, and playback events.
                        let mut tags = Vec::new();
                        while let Some(tag) = pad.sticky_event::<gst::event::Tag>(tags.len() as u32)
                        {
                            tags.push(tag);
                        }
                        pad.send_event(
                            gst::event::StreamStart::builder(&format!(
                                "rufin-segment-{:?}",
                                event.seqnum()
                            ))
                            .group_id(gst::GroupId::next())
                            .build(),
                        );
                        for tag in tags {
                            pad.send_event(tag);
                        }
                    }
                }
                gst::PadProbeReturn::Ok
            });
        // Keep normalization out of the sink: playbin searches it for a volume
        // control and would otherwise let track gain overwrite the user's volume.
        self.native()?.0.set_property("audio-filter", graph.root());
        self.native()?.0.set_property("audio-sink", graph.output());
        self.audio_graph = Some(graph);
        Ok(())
    }

    pub(super) fn try_reconfigure_audio(
        &mut self,
        settings: &BackendAudioSettings,
    ) -> Result<bool, String> {
        self.audio_graph.as_mut().map_or(Ok(false), |graph| {
            graph.reconfigure(settings, self.playback_rate)
        })
    }

    pub(super) fn set_stream(&mut self, stream: &PreparedStream) -> Result<(), String> {
        *self
            .queued_loudness
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = stream.loudness.clone();
        self.trust_invalid_certificate
            .store(stream.trust_invalid_certificate(), Ordering::SeqCst);
        self.native()?.0.set_property("uri", stream.uri());
        Ok(())
    }

    pub(super) fn set_visualizer_tap(&mut self, tap: Option<VisualizerTap>) {
        self.clear_visualizer_tap();
        if let (Some(tap), Some(pad)) = (
            tap,
            self.audio_graph
                .as_ref()
                .and_then(AudioGraph::visualizer_pad),
        ) {
            self.visualizer_probe = tap.install(pad);
        }
    }

    pub(super) fn clear_visualizer_tap(&mut self) {
        if let (Some(pad), Some(probe)) = (
            self.audio_graph
                .as_ref()
                .and_then(AudioGraph::visualizer_pad),
            self.visualizer_probe.take(),
        ) {
            pad.remove_probe(probe);
        } else {
            self.visualizer_probe = None;
        }
    }

    pub(super) fn set_output_volume(&self, volume: f64, muted: bool) {
        if let Some((pipeline, _)) = self.native.as_ref() {
            pipeline.set_property("volume", volume.clamp(0.0, 1.0));
            pipeline.set_property("mute", muted);
        }
    }

    pub(super) fn set_state(&self, state: gst::State) -> Result<gst::StateChangeSuccess, String> {
        self.requested_state.set(state);
        self.apply_requested_state()
    }

    pub(super) fn is_buffering(&self) -> bool {
        !self.live.get()
            && self
                .buffering_percent
                .get()
                .is_some_and(|percent| percent < 100)
    }

    fn apply_requested_state(&self) -> Result<gst::StateChangeSuccess, String> {
        let requested = self.requested_state.get();
        let waiting = requested == gst::State::Playing && self.is_buffering();
        let state = if waiting {
            gst::State::Paused
        } else {
            requested
        };
        let (pipeline, bus) = self.native()?;
        let result = pipeline.set_state(state).map_err(|error| {
            bus
                .pop_filtered(&[gst::MessageType::Error])
                .and_then(|message| {
                    let output = self.audio_output_factory();
                    gstreamer_error_details(
                        &message,
                        &format!("state change to {state:?}"),
                        output.as_deref(),
                    )
                })
                .unwrap_or_else(|| {
                    let output = self
                        .audio_output_factory()
                        .unwrap_or_else(|| "unconfigured".to_string());
                    format!(
                        "GStreamer state change to {state:?} failed; audio_sink={output}; error={error}"
                    )
                })
        })?;
        if result == gst::StateChangeSuccess::NoPreroll {
            self.live.set(true);
            if waiting {
                return self.apply_requested_state();
            }
        }
        // A prepared handoff must wait for Playing, even if pausing to refill
        // completed synchronously.
        if waiting && !self.live.get() {
            Ok(gst::StateChangeSuccess::Async)
        } else {
            Ok(result)
        }
    }

    pub(super) fn stop(&mut self) {
        if let Some(handler_id) = self.about_to_finish_id.take() {
            self.native
                .as_ref()
                .expect("connected playbin")
                .0
                .disconnect(handler_id);
        }
        self.clear_visualizer_tap();
        if let Some((pipeline, _)) = self.native.as_ref() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.id = None;
        self.current_stream = None;
        *lock_recover(&self.queued_loudness) = TrackLoudness::default();
        *lock_recover(&self.gapless) = GaplessPlayback::default();
        *lock_recover(&self.segment) = SegmentPlayback::default();
        self.module_decoder.store(false, Ordering::Relaxed);
        self.buffering_percent.set(None);
        self.requested_state.set(gst::State::Null);
        self.live.set(false);
        if let Some(graph) = self.audio_graph.as_mut() {
            graph.clear_stream();
        }
    }

    pub(super) fn seek_millis(&self, millis: u64) -> Result<(), String> {
        self.seek_physical_millis(self.clock().physical_seek(millis))
    }

    pub(super) fn seek_physical_millis(&self, millis: u64) -> Result<(), String> {
        if self.module_decoder.load(Ordering::Relaxed) {
            return Err("Tracker decoder does not support safe seeking".to_string());
        }
        let mut flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        if self.clock().end_millis().is_some() {
            flags |= gst::SeekFlags::SEGMENT;
        }
        self.seek_segment(millis, self.clock().end_millis(), flags, false)
    }

    fn seek_segment(
        &self,
        millis: u64,
        end: Option<u64>,
        flags: gst::SeekFlags,
        starts_stream: bool,
    ) -> Result<(), String> {
        let start = gst::ClockTime::try_from(Duration::from_millis(
            end.map_or(millis, |end| millis.min(end)),
        ))
        .map_err(|_| "Seek position exceeds the GStreamer clock range".to_string())?;
        let end = end
            .map(|millis| gst::ClockTime::try_from(Duration::from_millis(millis)))
            .transpose()
            .map_err(|_| "Segment end exceeds the GStreamer clock range".to_string())?;
        let event = gst::event::Seek::new(
            self.playback_rate,
            flags,
            gst::SeekType::Set,
            start,
            if end.is_some() {
                gst::SeekType::Set
            } else {
                gst::SeekType::None
            },
            end,
        );
        *self.segment.lock().unwrap_or_else(|p| p.into_inner()) = SegmentPlayback {
            seek: Some(event.seqnum()),
            done: false,
            starts_stream,
        };
        self.native()?
            .0
            .send_event(event)
            .then_some(())
            .ok_or_else(|| "GStreamer segment seek failed".to_string())
    }

    pub(super) fn set_playback_rate(
        &mut self,
        rate: f64,
        seek_current_position: bool,
        settings: &BackendAudioSettings,
    ) -> Result<bool, String> {
        let previous_rate = self.playback_rate;
        self.playback_rate = sanitize_playback_rate(rate);
        let result = (|| {
            self.configure_audio(settings)?;
            let position = seek_current_position.then(|| self.position()).flatten();
            match position {
                Some(position) => self
                    .seek_physical_millis(position.mseconds())
                    .map(|()| true),
                None => Ok(false),
            }
        })();
        if result.is_err() {
            self.playback_rate = previous_rate;
            let _ = self.configure_audio(settings);
        }
        result
    }

    pub(super) fn needs_initial_rate_seek(&self) -> bool {
        (self.playback_rate - DEFAULT_PLAYBACK_RATE).abs() > f64::EPSILON
    }

    pub(super) fn position(&self) -> Option<gst::ClockTime> {
        self.id?;
        let (pipeline, _) = self.native.as_ref()?;
        if self.module_decoder.load(Ordering::Relaxed) {
            return (pipeline.current_state() == gst::State::Playing)
                .then(|| pipeline.current_running_time())
                .flatten();
        }
        pipeline.query_position::<gst::ClockTime>()
    }

    pub(super) fn duration(&self) -> Option<gst::ClockTime> {
        self.id?;
        let (pipeline, _) = self.native.as_ref()?;
        if self.module_decoder.load(Ordering::Relaxed) {
            return None;
        }
        pipeline.query_duration::<gst::ClockTime>()
    }

    pub(super) fn seekable(&self) -> Option<bool> {
        let (pipeline, _) = self.native.as_ref()?;
        if self.module_decoder.load(Ordering::Relaxed) {
            return Some(false);
        }
        let mut query = gst::query::Seeking::new(gst::Format::Time);
        Some(pipeline.query(&mut query) && query.result().0)
    }

    pub(super) fn audio_output_factory(&self) -> Option<String> {
        self.audio_graph
            .as_ref()
            .and_then(AudioGraph::output_factory)
    }
}
impl Drop for PlayerPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
pub(super) fn make_playbin(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make("playbin3")
        .name(name)
        .build()
        .map_err(|error| error.to_string())
}
pub(super) fn configure_playbin_for_audio(pipeline: &gst::Element) {
    let current = pipeline.property_value("flags");
    let Some(flags_class) = glib::FlagsClass::with_type(current.type_()) else {
        return;
    };
    let Some(flags) = flags_class
        .builder()
        .set_by_nick("audio")
        .set_by_nick("soft-volume")
        .set_by_nick("buffering")
        .build()
    else {
        return;
    };
    pipeline.set_property_from_value("flags", &flags);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_decoder_rejects_seeking_without_format_metadata_or_extension() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stream");
        let mut module = vec![0_u8; 1_084 + 1_024 + 64];
        module[42..44].copy_from_slice(&32_u16.to_be_bytes());
        module[45] = 64;
        module[48..50].copy_from_slice(&32_u16.to_be_bytes());
        module[950] = 1;
        module[1_080..1_084].copy_from_slice(b"M.K.");
        module[1_084..1_088].copy_from_slice(&[1, 172, 16, 0]);
        module[2_108..2_140].fill(96);
        module[2_140..].fill(160);
        std::fs::write(&path, module).unwrap();
        let uri = glib::filename_to_uri(&path, None).unwrap();
        let shared = Arc::new(Mutex::new(SharedBackendState::new()));
        let mut player = PlayerPipeline::new("tracker-decoder-check", shared);
        let item = PreparedRun {
            run: RunId::new(1),
            stream: ResolvedStream::new(uri.as_str()).into(),
        };
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        player
            .play_item(
                PipelineId(1),
                Slot::Primary,
                &item,
                &settings,
                1.0,
                false,
                1.0,
                gst::State::Paused,
            )
            .unwrap();
        let state = player
            .native
            .as_ref()
            .unwrap()
            .0
            .state(gst::ClockTime::from_seconds(30));
        if state.0 != Ok(gst::StateChangeSuccess::Success) {
            let (pipeline, bus) = player.native.as_ref().unwrap();
            for message in bus.iter() {
                eprintln!("{message:?}");
            }
            eprintln!(
                "{}",
                pipeline
                    .downcast_ref::<gst::Bin>()
                    .unwrap()
                    .debug_to_dot_data(gst::DebugGraphDetails::ALL)
            );
        }
        assert_eq!(
            state,
            (
                Ok(gst::StateChangeSuccess::Success),
                gst::State::Paused,
                gst::State::VoidPending
            ),
            "tracker pipeline did not finish starting"
        );
        assert!(!player.allows_preloading());
        assert_eq!(player.seekable(), Some(false));
        assert_eq!(player.duration(), None);
        assert!(player.seek_millis(1_000).is_err());
        player.stop();
    }

    #[test]
    fn repeated_tracks_release_streams_and_reuse_the_player() {
        ensure_gstreamer_initialized().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("track.wav");
        let producer =
            gst::parse::launch("audiotestsrc num-buffers=100 ! wavenc ! filesink name=output")
                .unwrap();
        producer
            .downcast_ref::<gst::Bin>()
            .unwrap()
            .by_name("output")
            .unwrap()
            .set_property("location", &path);
        producer.set_state(gst::State::Playing).unwrap();
        let done = producer
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(10),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .unwrap();
        producer.set_state(gst::State::Null).unwrap();
        assert!(matches!(done.view(), gst::MessageView::Eos(_)));
        drop(done);
        drop(producer);
        let uri = glib::filename_to_uri(&path, None).unwrap();
        let shared = Arc::new(Mutex::new(SharedBackendState::new()));
        let mut player = PlayerPipeline::new("retention-check", shared);
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let mut retired: Vec<(String, glib::WeakRef<gst::Object>)> = Vec::new();
        for index in 1..=100 {
            let resource = Arc::new(());
            let resource_ref = Arc::downgrade(&resource);
            let item = PreparedRun {
                run: RunId::new(index),
                stream: ResolvedStream::new(uri.as_str())
                    .with_resource(resource)
                    .into(),
            };
            player
                .play_item(
                    PipelineId(index),
                    Slot::Primary,
                    &item,
                    &settings,
                    1.0,
                    false,
                    1.0,
                    gst::State::Playing,
                )
                .unwrap();
            drop(item);
            assert!(resource_ref.upgrade().is_some());
            let (pipeline, bus) = player.native.as_ref().unwrap();
            pipeline.state(gst::ClockTime::from_seconds(5)).0.unwrap();
            if let Some((_, previous)) = retired.first() {
                assert_eq!(previous.upgrade().as_ref(), Some(pipeline.upcast_ref()));
            }
            retired.clear();
            retired.push((
                pipeline.name().to_string(),
                pipeline.upcast_ref::<gst::Object>().downgrade(),
            ));
            retired.push((
                bus.name().to_string(),
                bus.upcast_ref::<gst::Object>().downgrade(),
            ));
            let bin = pipeline.clone().downcast::<gst::Bin>().unwrap();
            for element in bin.iterate_recurse().into_iter().map(Result::unwrap) {
                retired.push((
                    element.name().to_string(),
                    element.upcast_ref::<gst::Object>().downgrade(),
                ));
                for pad in element.pads() {
                    retired.push((
                        pad.path_string().to_string(),
                        pad.upcast_ref::<gst::Object>().downgrade(),
                    ));
                }
            }
            player.stop();
            assert!(!player.has_session());
            assert_eq!(bin.current_state(), gst::State::Null);
            assert!(resource_ref.upgrade().is_none());
        }
        drop(player);
        let remaining: Vec<_> = retired
            .iter()
            .filter_map(|(name, weak)| weak.upgrade().map(|element| (name, element.ref_count())))
            .collect();
        assert!(
            remaining.is_empty(),
            "after player shutdown: retained {remaining:?}"
        );
    }
}
