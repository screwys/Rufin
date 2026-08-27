use super::audio::{AudioGraph, SharedLoudnessTags};
use super::engine::{PipelineId, PreparedRun, SharedBackendState, Slot, handle_about_to_finish};
use super::waveform::VisualizerTap;
use super::*;

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
    session: Option<PipelineSession>,
}
#[derive(Debug, PartialEq)]
pub(super) enum AboutToFinishAction {
    Preload(Box<PreparedNext>),
    Ignore,
}
struct PipelineSession {
    id: PipelineId,
    pipeline: gst::Element,
    bus: gst::Bus,
    clock: SourceClock,
    trust_invalid_certificate: Arc<AtomicBool>,
    about_to_finish_id: Option<glib::SignalHandlerId>,
    audio_graph: Option<AudioGraph>,
    loudness_tags: SharedLoudnessTags,
    visualizer_probe: Option<gst::PadProbeId>,
    current_stream: PreparedStream,
    playback_rate: f64,
}
impl PlayerPipeline {
    pub(super) fn new(name: &str, shared: Arc<Mutex<SharedBackendState>>) -> Self {
        Self {
            name: name.to_string(),
            shared,
            session: None,
        }
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
        let session_name = format!("{}-{}", self.name, id.0);
        let mut session = PipelineSession::new(
            &session_name,
            id,
            slot,
            Arc::clone(&self.shared),
            &item.stream,
            playback_rate,
        )?;
        session.configure_audio(settings)?;
        session.set_stream(&item.stream);
        session.set_output_volume(volume, muted);
        if let Err(error) = session.set_state(startup_state) {
            session.stop();
            return Err(error);
        }
        session.set_output_volume(volume, muted);
        self.session = Some(session);
        Ok(())
    }

    pub(super) fn configure_audio(
        &mut self,
        settings: &BackendAudioSettings,
    ) -> Result<(), String> {
        if let Some(session) = self.session.as_mut() {
            session.configure_audio(settings)?;
        }
        Ok(())
    }

    pub(super) fn try_reconfigure_audio(
        &mut self,
        settings: &BackendAudioSettings,
    ) -> Result<bool, String> {
        self.session
            .as_mut()
            .map_or(Ok(false), |session| session.try_reconfigure_audio(settings))
    }

    pub(super) fn set_visualizer_tap(&mut self, tap: Option<VisualizerTap>) {
        if let Some(session) = self.session.as_mut() {
            session.set_visualizer_tap(tap);
        }
    }

    pub(super) fn set_output_volume(&self, volume: f64, muted: bool) {
        if let Some(session) = self.session.as_ref() {
            session.set_output_volume(volume, muted);
        }
    }

    #[cfg(test)]
    pub(super) fn output_volume_state(&self) -> Option<(f64, bool)> {
        self.session.as_ref().map(|session| {
            (
                session.pipeline.property::<f64>("volume"),
                session.pipeline.property::<bool>("mute"),
            )
        })
    }

    #[cfg(test)]
    pub(super) fn has_or_targets_state(&self, state: gst::State) -> bool {
        self.session.as_ref().is_some_and(|session| {
            let (result, current, pending) = session.pipeline.state(gst::ClockTime::ZERO);
            result.is_ok() && (current == state || pending == state)
        })
    }

    pub(super) fn set_state(&self, state: gst::State) -> Result<gst::StateChangeSuccess, String> {
        let Some(session) = self.session.as_ref() else {
            return Err(format!("GStreamer session {} is not active", self.name));
        };
        session.set_state(state)
    }

    pub(super) fn stop(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.stop();
        }
    }

    pub(super) fn seek_millis(&self, millis: u64) -> Result<(), String> {
        let Some(session) = self.session.as_ref() else {
            return Err(format!("GStreamer session {} is not active", self.name));
        };
        session.seek_millis(millis)
    }

    pub(super) fn seek_physical_millis(&self, millis: u64) -> Result<(), String> {
        let Some(session) = self.session.as_ref() else {
            return Err(format!("GStreamer session {} is not active", self.name));
        };
        session.seek_physical_millis(millis)
    }

    pub(super) fn set_playback_rate(
        &mut self,
        rate: f64,
        seek_current_position: bool,
        settings: &BackendAudioSettings,
    ) -> Result<bool, String> {
        let Some(session) = self.session.as_mut() else {
            return Ok(false);
        };
        session.set_playback_rate(rate, seek_current_position, settings)
    }

    pub(super) fn needs_initial_rate_seek(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(PipelineSession::needs_initial_rate_seek)
    }

    pub(super) fn physical_seek_target(&self, logical_millis: u64) -> u64 {
        self.session.as_ref().map_or(logical_millis, |session| {
            session.clock.physical_seek(logical_millis)
        })
    }

    pub(super) fn logical_position(&self, physical_millis: u64) -> u64 {
        self.session.as_ref().map_or(physical_millis, |session| {
            session.clock.logical_position(physical_millis)
        })
    }

    pub(super) fn logical_duration(&self, physical_millis: u64) -> u64 {
        self.session.as_ref().map_or(physical_millis, |session| {
            session.clock.logical_duration(physical_millis)
        })
    }

    pub(super) fn logical_remaining(&self, physical_position: u64, physical_duration: u64) -> u64 {
        self.session.as_ref().map_or_else(
            || physical_duration.saturating_sub(physical_position),
            |session| {
                session
                    .clock
                    .remaining(physical_position, physical_duration)
            },
        )
    }

    pub(super) fn fixed_duration(&self) -> Option<u64> {
        self.session
            .as_ref()
            .and_then(|session| session.clock.fixed_duration())
    }

    pub(super) fn set_source_clock(&mut self, stream: &ResolvedStream) {
        if let Some(session) = self.session.as_mut() {
            session.clock = SourceClock::from_stream(stream);
        }
    }

    pub(super) fn rearm_stream_window(
        &mut self,
        stream: &PreparedStream,
    ) -> Result<gst::Seqnum, String> {
        let Some(session) = self.session.as_mut() else {
            return Err(format!("GStreamer session {} is not active", self.name));
        };
        session.rearm_stream_window(stream)
    }

    pub(super) fn has_session(&self) -> bool {
        self.session.is_some()
    }

    pub(super) fn position(&self) -> Option<gst::ClockTime> {
        self.session.as_ref().and_then(PipelineSession::position)
    }

    pub(super) fn duration(&self) -> Option<gst::ClockTime> {
        self.session.as_ref().and_then(PipelineSession::duration)
    }

    pub(super) fn running_time(&self) -> Option<gst::ClockTime> {
        self.session
            .as_ref()
            .and_then(PipelineSession::running_time)
    }

    pub(super) fn seekable(&self) -> Option<bool> {
        self.session.as_ref().map(PipelineSession::seekable)
    }

    pub(super) fn audio_output_factory(&self) -> Option<String> {
        self.session
            .as_ref()
            .and_then(PipelineSession::audio_output_factory)
    }

    pub(super) fn set_stream(&mut self, stream: &PreparedStream) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else {
            return Err("GStreamer session is not active".to_string());
        };
        session.set_stream(stream);
        Ok(())
    }

    pub(super) fn pop_bus_message(&self) -> Option<(PipelineId, gst::Message)> {
        self.session
            .as_ref()
            .and_then(|session| session.bus.pop().map(|message| (session.id, message)))
    }

    pub(super) fn message_source_is_pipeline(&self, message: &gst::Message) -> bool {
        self.session.as_ref().is_some_and(|session| {
            message
                .src()
                .is_some_and(|source| source == session.pipeline.upcast_ref::<gst::Object>())
        })
    }
}
impl PipelineSession {
    pub(super) fn new(
        name: &str,
        id: PipelineId,
        slot: Slot,
        shared: Arc<Mutex<SharedBackendState>>,
        stream: &PreparedStream,
        playback_rate: f64,
    ) -> Result<Self, String> {
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
        let trust_invalid_certificate =
            Arc::new(AtomicBool::new(stream.trust_invalid_certificate()));
        let certificate_policy = Arc::clone(&trust_invalid_certificate);
        connect_server_certificate_policy(&pipeline, move || {
            certificate_policy.load(Ordering::SeqCst)
        });

        let pipeline_for_signal = pipeline.clone();
        let shared_for_signal = Arc::clone(&shared);
        let loudness_tags = Arc::new(Mutex::new(None));
        let loudness_tags_for_signal = Arc::clone(&loudness_tags);
        let certificate_policy_for_signal = Arc::clone(&trust_invalid_certificate);
        let about_to_finish_id = pipeline.connect("about-to-finish", false, move |_| {
            handle_about_to_finish(
                &pipeline_for_signal,
                &shared_for_signal,
                &loudness_tags_for_signal,
                &certificate_policy_for_signal,
                slot,
                id,
            );
            None
        });

        Ok(Self {
            id,
            pipeline,
            bus,
            clock: SourceClock::from_stream(stream),
            trust_invalid_certificate,
            about_to_finish_id: Some(about_to_finish_id),
            audio_graph: None,
            loudness_tags,
            visualizer_probe: None,
            current_stream: stream.clone(),
            playback_rate: sanitize_playback_rate(playback_rate),
        })
    }

    pub(super) fn configure_audio(
        &mut self,
        settings: &BackendAudioSettings,
    ) -> Result<(), String> {
        if let Some(graph) = self.audio_graph.as_mut()
            && graph.reconfigure(settings, self.playback_rate)?
        {
            return Ok(());
        }
        self.clear_visualizer_tap();
        let graph = AudioGraph::new(settings, self.playback_rate)?;
        graph.apply_loudness(&self.current_stream.loudness);
        *self
            .loudness_tags
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = graph.loudness_tags();
        self.pipeline.set_property("audio-sink", graph.root());
        self.audio_graph = Some(graph);
        Ok(())
    }

    fn try_reconfigure_audio(&mut self, settings: &BackendAudioSettings) -> Result<bool, String> {
        self.audio_graph.as_mut().map_or(Ok(false), |graph| {
            graph.reconfigure(settings, self.playback_rate)
        })
    }

    fn set_stream(&mut self, stream: &PreparedStream) {
        if let Some(graph) = self.audio_graph.as_ref() {
            graph.apply_loudness(&stream.loudness);
        }
        self.current_stream = stream.clone();
        self.trust_invalid_certificate
            .store(stream.trust_invalid_certificate(), Ordering::SeqCst);
        self.pipeline.set_property("uri", stream.uri());
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
        self.pipeline.set_property("volume", volume.clamp(0.0, 1.0));
        self.pipeline.set_property("mute", muted);
    }

    pub(super) fn set_state(&self, state: gst::State) -> Result<gst::StateChangeSuccess, String> {
        self.pipeline.set_state(state).map_err(|error| {
            self.bus
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
        })
    }

    pub(super) fn stop(&mut self) {
        if let Some(handler_id) = self.about_to_finish_id.take() {
            self.pipeline.disconnect(handler_id);
        }
        self.clear_visualizer_tap();
        self.loudness_tags
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let _ = self.pipeline.set_state(gst::State::Null);
    }

    pub(super) fn seek_millis(&self, millis: u64) -> Result<(), String> {
        self.seek_physical_millis(self.clock.physical_seek(millis))
    }

    pub(super) fn seek_physical_millis(&self, millis: u64) -> Result<(), String> {
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        let (seek_flags, stop_type, stop) = self.clock.end_millis().map_or(
            (flags, gst::SeekType::None, gst::ClockTime::NONE),
            |end_millis| {
                (
                    flags | gst::SeekFlags::SEGMENT,
                    gst::SeekType::Set,
                    Some(gst::ClockTime::from_mseconds(end_millis)),
                )
            },
        );
        let result = self.pipeline.seek(
            self.playback_rate,
            seek_flags,
            gst::SeekType::Set,
            gst::ClockTime::from_mseconds(
                self.clock
                    .end_millis()
                    .map_or(millis, |end_millis| millis.min(end_millis)),
            ),
            stop_type,
            stop,
        );
        result.map_err(|error| error.to_string())
    }

    fn set_playback_rate(
        &mut self,
        rate: f64,
        seek_current_position: bool,
        settings: &BackendAudioSettings,
    ) -> Result<bool, String> {
        self.playback_rate = sanitize_playback_rate(rate);
        self.configure_audio(settings)?;
        if !seek_current_position {
            return Ok(false);
        }
        let Some(position) = self.position() else {
            return Ok(false);
        };
        self.seek_physical_millis(position.mseconds())
            .map(|()| true)
    }

    fn needs_initial_rate_seek(&self) -> bool {
        (self.playback_rate - DEFAULT_PLAYBACK_RATE).abs() > f64::EPSILON
    }

    fn rearm_stream_window(&mut self, stream: &PreparedStream) -> Result<gst::Seqnum, String> {
        if let Some(graph) = self.audio_graph.as_ref() {
            graph.apply_loudness(&stream.loudness);
        }
        self.current_stream = stream.clone();
        let clock = SourceClock::from_stream(stream);
        let start_millis = clock.physical_seek(0);
        let end_millis = clock
            .end_millis()
            .ok_or_else(|| "Adjacent stream window has no end boundary".to_string())?;
        // GstBin gives the top-level AsyncDone its own sequence number, so retain the
        // last number allocated before this seek instead of expecting exact equality.
        let confirmation_after = gst::Seqnum::next();
        self.pipeline
            .seek(
                self.playback_rate,
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE | gst::SeekFlags::SEGMENT,
                gst::SeekType::Set,
                gst::ClockTime::from_mseconds(start_millis.min(end_millis)),
                gst::SeekType::Set,
                gst::ClockTime::from_mseconds(end_millis),
            )
            .map(|_| confirmation_after)
            .map_err(|error| error.to_string())
    }

    pub(super) fn position(&self) -> Option<gst::ClockTime> {
        self.pipeline.query_position::<gst::ClockTime>()
    }

    pub(super) fn duration(&self) -> Option<gst::ClockTime> {
        self.pipeline.query_duration::<gst::ClockTime>()
    }

    pub(super) fn running_time(&self) -> Option<gst::ClockTime> {
        self.pipeline.current_running_time()
    }

    pub(super) fn seekable(&self) -> bool {
        let mut query = gst::query::Seeking::new(gst::Format::Time);
        self.pipeline.query(&mut query) && query.result().0
    }

    pub(super) fn audio_output_factory(&self) -> Option<String> {
        self.audio_graph
            .as_ref()
            .and_then(AudioGraph::output_factory)
    }
}
impl Drop for PipelineSession {
    fn drop(&mut self) {
        self.stop();
    }
}
pub(super) fn make_playbin(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make("playbin3")
        .name(name)
        .build()
        .or_else(|_| gst::ElementFactory::make("playbin").name(name).build())
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
