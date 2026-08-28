use std::time::{Duration, Instant};

use playback::{BackendCommand, BackendEvent, BackendState, PreparedNext, PreparedStream, RunId};

use crate::relay::{PublishedResource, RelayRepresentation, RelayServer, source_content_type};
use crate::upnp_transport::UpnpDevice;

const AV_TRANSPORT: &str = "AVTransport";
const CONNECTION_MANAGER: &str = "ConnectionManager";
const RENDERING_CONTROL: &str = "RenderingControl";
const END_POSITION_TOLERANCE_MILLIS: u64 = 2_000;
const SEEK_POSITION_SAMPLES: u8 = 4;
const PLAY_READY_TIMEOUT: Duration = Duration::from_secs(5);
const PLAY_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

struct PendingSeek {
    origin_millis: u64,
    target_millis: u64,
    remaining_samples: u8,
}

#[derive(Debug, Eq, PartialEq)]
enum SeekObservation {
    Waiting,
    Reached,
    Expired,
}

struct SinkProtocols(Option<Vec<String>>);

struct TransportActions(String);

impl SinkProtocols {
    fn known(value: &str) -> Self {
        Self(Some(
            value
                .split(',')
                .filter_map(|entry| {
                    let mut fields = entry.trim().splitn(4, ':');
                    let protocol = fields.next()?.trim().to_ascii_lowercase();
                    let _network = fields.next()?;
                    let content_type = normalize_content_type(fields.next()?);
                    let _additional_info = fields.next()?;
                    matches!(protocol.as_str(), "http-get" | "*").then_some(content_type)
                })
                .collect(),
        ))
    }

    fn representation_for(&self, source_content_type: &str) -> Result<RelayRepresentation, String> {
        let Some(protocols) = &self.0 else {
            return Ok(RelayRepresentation::Source);
        };
        let source_content_type = normalize_content_type(source_content_type);
        if protocols
            .iter()
            .any(|content_type| content_type == "*" || content_type == &source_content_type)
        {
            return Ok(RelayRepresentation::Source);
        }
        if protocols
            .iter()
            .any(|content_type| content_type == "*" || content_type == "audio/mpeg")
        {
            return Ok(RelayRepresentation::Mp3);
        }
        Err(format!(
            "UPnP renderer does not accept {source_content_type} or audio/mpeg"
        ))
    }
}

impl TransportActions {
    fn parse(value: &str) -> Self {
        Self(value.trim().to_ascii_lowercase())
    }

    fn allows(&self, action: &str) -> bool {
        self.0.is_empty()
            || self
                .0
                .split(',')
                .any(|allowed| allowed.trim().eq_ignore_ascii_case(action))
    }
}

fn normalize_content_type(value: &str) -> String {
    match value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/x-flac" => "audio/flac".to_string(),
        "audio/mp3" => "audio/mpeg".to_string(),
        value => value.to_string(),
    }
}

impl PendingSeek {
    fn reached(&self, observed_millis: u64) -> bool {
        if self.origin_millis <= self.target_millis {
            observed_millis >= self.target_millis
        } else {
            observed_millis <= self.target_millis
        }
    }

    fn observe(&mut self, observed_millis: u64) -> SeekObservation {
        if self.reached(observed_millis) {
            return SeekObservation::Reached;
        }
        self.remaining_samples = self.remaining_samples.saturating_sub(1);
        if self.remaining_samples == 0 {
            SeekObservation::Expired
        } else {
            SeekObservation::Waiting
        }
    }
}

#[derive(Clone, Copy)]
struct HeldOutput {
    volume: f64,
    muted: bool,
}

struct QueuedMedia {
    run: RunId,
    published: PublishedResource,
    metadata: String,
    duration_millis: Option<u64>,
}

impl QueuedMedia {
    fn new(run: RunId, stream: PreparedStream, published: PublishedResource) -> Self {
        let duration_millis = stream
            .end_millis()
            .map(|end| end.saturating_sub(stream.start_millis()))
            .or_else(|| {
                stream
                    .track
                    .as_ref()
                    .and_then(|track| u64::try_from(track.duration_millis).ok())
            });
        let metadata = didl_metadata(&stream, &published);
        Self {
            run,
            published,
            metadata,
            duration_millis,
        }
    }
}

pub(crate) struct UpnpController {
    device: UpnpDevice,
    sink_protocols: SinkProtocols,
    current: Option<QueuedMedia>,
    next_media: Option<QueuedMedia>,
    last_position_millis: u64,
    pending_seek: Option<PendingSeek>,
    pending_start_position_millis: Option<u64>,
    started: bool,
    renderer_owned: bool,
    seekable: bool,
    startup_output: Option<HeldOutput>,
    observation_unavailable: bool,
}

impl UpnpController {
    pub(crate) fn new(device: UpnpDevice) -> Result<Self, String> {
        if !device.has_service(AV_TRANSPORT) {
            return Err(format!(
                "{} does not provide UPnP AVTransport",
                device.friendly_name()
            ));
        }
        Ok(Self {
            device,
            sink_protocols: SinkProtocols(None),
            current: None,
            next_media: None,
            last_position_millis: 0,
            pending_seek: None,
            pending_start_position_millis: None,
            started: false,
            renderer_owned: false,
            seekable: false,
            startup_output: None,
            observation_unavailable: false,
        })
    }

    pub(crate) fn initial_events(&self) -> Vec<BackendEvent> {
        self.volume()
            .map(|(volume, muted)| {
                vec![BackendEvent::AudioApplied {
                    volume,
                    muted,
                    output: None,
                }]
            })
            .unwrap_or_default()
    }

    pub(crate) fn verify_connection(&mut self) -> Result<(), String> {
        let result = self.action(
            AV_TRANSPORT,
            "GetTransportInfo",
            "<InstanceID>0</InstanceID>",
        );
        if let Err(error) = &result {
            tracing::debug!(
                description_url = %self.device.url(),
                local_address = ?self.device.local_address(),
                %error,
                "UPnP renderer connection probe failed"
            );
        }
        result?;
        self.sink_protocols = self.read_sink_protocols();
        Ok(())
    }

    pub(crate) fn handle(
        &mut self,
        command: BackendCommand,
        relay: &RelayServer,
    ) -> Result<Vec<BackendEvent>, String> {
        match command {
            BackendCommand::Start {
                run,
                current,
                next,
                start_position_millis,
                ..
            } => self.start(run, current, next, start_position_millis, relay),
            BackendCommand::Play { run } if self.current_run() == Some(run) => {
                let result = if self.renderer_owned {
                    if !self.transport_action_allowed("Play") {
                        return Ok(Vec::new());
                    }
                    self.play()
                } else {
                    let uri = self
                        .current
                        .as_ref()
                        .map(|media| media.published.uri.clone())
                        .ok_or_else(|| "UPnP current media is unavailable".to_string())?;
                    let metadata = self
                        .current
                        .as_ref()
                        .map(|media| media.metadata.clone())
                        .unwrap_or_default();
                    self.set_uri(&uri, &metadata).and_then(|()| {
                        self.renderer_owned = true;
                        self.start_transport()
                    })
                };
                if let Err(error) = result {
                    let _ = self.stop();
                    self.finish_current(relay);
                    return Err(error);
                }
                self.started = true;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Playing,
                }])
            }
            BackendCommand::Pause { run } if self.current_run() == Some(run) => {
                if !self.transport_action_allowed("Pause") {
                    return Ok(Vec::new());
                }
                self.pause()?;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Paused,
                }])
            }
            BackendCommand::Stop { run } if self.current_run() == Some(run) => {
                let result = self.stop();
                self.finish_current(relay);
                result?;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Stopped,
                }])
            }
            BackendCommand::Seek {
                run,
                position_millis,
            } if self.current_run() == Some(run) && self.seekable => {
                let target = position_millis;
                match self.seek_once(target) {
                    Ok(()) => Ok(vec![BackendEvent::Position {
                        run,
                        millis: position_millis,
                    }]),
                    Err(error) => {
                        tracing::debug!(%error, target_millis = target, "UPnP seek was rejected");
                        Ok(Vec::new())
                    }
                }
            }
            BackendCommand::PrepareNext { current_run, next }
                if self.current_run() == Some(current_run) =>
            {
                self.prepare_next(current_run, next, relay)
            }
            BackendCommand::SetOutputVolume { volume, muted, .. } => {
                if let Some(output) = self.startup_output.as_mut() {
                    output.volume = volume;
                    output.muted = muted;
                    self.set_volume(volume, true)?;
                } else {
                    self.set_volume(volume, muted)?;
                }
                Ok(vec![BackendEvent::AudioApplied {
                    volume,
                    muted,
                    output: None,
                }])
            }
            BackendCommand::PrepareNext { .. }
            | BackendCommand::ConfigureAudio(_)
            | BackendCommand::SetPlaybackRate(_)
            | BackendCommand::SetVisualizerEnabled(_)
            | BackendCommand::Play { .. }
            | BackendCommand::Pause { .. }
            | BackendCommand::Stop { .. }
            | BackendCommand::Seek { .. } => Ok(Vec::new()),
        }
    }

    pub(crate) fn poll(&mut self, relay: &RelayServer) -> Result<Vec<BackendEvent>, String> {
        let Some(run) = self.current_run() else {
            return Ok(Vec::new());
        };
        let transport = match self.action(
            AV_TRANSPORT,
            "GetTransportInfo",
            "<InstanceID>0</InstanceID>",
        ) {
            Ok(transport) => transport,
            Err(error) => return Ok(self.observation_failed(error)),
        };
        let transport_state = transport.get("CurrentTransportState").map(String::as_str);
        let position = match self.action(
            AV_TRANSPORT,
            "GetPositionInfo",
            "<InstanceID>0</InstanceID>",
        ) {
            Ok(position) => position,
            Err(error) => return Ok(self.observation_failed(error)),
        };
        if self.observation_unavailable {
            self.observation_unavailable = false;
            tracing::debug!("UPnP renderer status is available again");
        }
        let track_uri = position
            .get("TrackURI")
            .map(String::as_str)
            .unwrap_or_default();
        if self
            .next_media
            .as_ref()
            .is_some_and(|next| !track_uri.is_empty() && track_uri == next.published.uri)
        {
            return self.accept_next_transition(position, transport_state, relay);
        }
        if !track_uri.is_empty()
            && self
                .current
                .as_ref()
                .is_some_and(|current| track_uri != current.published.uri)
        {
            self.started = false;
            self.renderer_owned = false;
            let _ = self.restore_startup_output();
            return Ok(vec![BackendEvent::State {
                run,
                state: BackendState::Stopped,
            }]);
        }
        let state = match transport_state {
            Some("PLAYING") => BackendState::Playing,
            Some("PAUSED_PLAYBACK") | Some("PAUSED_RECORDING") => BackendState::Paused,
            Some("TRANSITIONING") => BackendState::Buffering,
            Some("STOPPED") | Some("NO_MEDIA_PRESENT") => BackendState::Stopped,
            _ => BackendState::Buffering,
        };
        let position_millis = position
            .get("RelTime")
            .and_then(|value| parse_upnp_time(value))
            .map(|renderer_position| self.current_logical_position_millis(renderer_position))
            .unwrap_or(self.last_position_millis);
        let restore_was_pending = self.pending_start_position_millis.is_some();
        let mut seekability_changed = false;
        if matches!(state, BackendState::Playing | BackendState::Paused)
            && let Some(target) = self.pending_start_position_millis.take()
        {
            match self.seek_once(target) {
                Ok(()) => {
                    seekability_changed = !self.seekable;
                    self.seekable = true;
                }
                Err(error) => {
                    tracing::debug!(
                        %error,
                        target_millis = target,
                        "UPnP output could not restore its position after becoming ready"
                    );
                    if self.startup_output.is_some() {
                        let _ = self.stop();
                        let _ = self.restore_startup_output();
                        self.finish_current(relay);
                        return Err(error);
                    }
                }
            }
        }
        let held_seek_was_pending = self.startup_output.is_some() && self.pending_seek.is_some();
        let held_seek_reached = held_seek_was_pending
            && self
                .pending_seek
                .as_ref()
                .is_some_and(|pending| pending.reached(position_millis));
        let publish_position =
            !restore_was_pending && self.accept_position_after_seek(position_millis);
        if held_seek_was_pending && self.pending_seek.is_none() {
            if !held_seek_reached {
                let _ = self.stop();
                let _ = self.restore_startup_output();
                self.finish_current(relay);
                return Err("UPnP output did not confirm its startup position".to_string());
            }
            self.restore_startup_output()?;
        }
        if self
            .current_duration_millis()
            .is_some_and(|duration| position_millis >= duration)
        {
            self.stop()?;
            self.finish_current(relay);
            return Ok(vec![BackendEvent::Ended { run }]);
        }
        if state == BackendState::Stopped && self.started {
            if self.position_is_at_end(position_millis, &position) {
                self.finish_current(relay);
                return Ok(vec![BackendEvent::Ended { run }]);
            }
            self.started = false;
        }
        if publish_position {
            self.last_position_millis = position_millis;
        }
        let mut events = vec![BackendEvent::State { run, state }];
        if seekability_changed {
            events.push(BackendEvent::Seekable {
                run,
                seekable: true,
            });
        }
        if publish_position {
            events.insert(
                0,
                BackendEvent::Position {
                    run,
                    millis: position_millis,
                },
            );
        }
        Ok(events)
    }

    pub(crate) fn shutdown(&mut self) {
        let _ = self.stop();
        let _ = self.restore_startup_output();
        self.current = None;
        self.next_media = None;
        self.pending_seek = None;
        self.pending_start_position_millis = None;
        self.started = false;
        self.renderer_owned = false;
        self.observation_unavailable = false;
    }

    fn observation_failed(&mut self, error: String) -> Vec<BackendEvent> {
        if self.observation_unavailable {
            tracing::debug!(%error, "UPnP renderer status remains unavailable");
        } else {
            self.observation_unavailable = true;
            tracing::warn!(%error, "UPnP renderer status is temporarily unavailable");
        }
        Vec::new()
    }

    fn start(
        &mut self,
        run: RunId,
        stream: PreparedStream,
        next: Option<PreparedNext>,
        position_millis: u64,
        relay: &RelayServer,
    ) -> Result<Vec<BackendEvent>, String> {
        let _ = self.restore_startup_output();
        self.reset_transport()?;
        self.finish_current(relay);
        relay.clear();
        let representation = self
            .sink_protocols
            .representation_for(&source_content_type(&stream))?;
        let media = relay.publish_at(&stream, representation, position_millis)?;
        if let Err(error) = self.set_uri(&media.uri, &didl_metadata(&stream, &media)) {
            relay.remove(&media);
            return Err(error);
        }
        self.install_current(run, stream.clone(), media.clone());
        let hold_output = position_millis > 0
            && media.seekable
            && match self.hold_startup_output() {
                Ok(()) => true,
                Err(error) => {
                    tracing::debug!(
                        %error,
                        "UPnP output could not be silenced for position restoration"
                    );
                    false
                }
            };
        if let Err(error) = self.start_transport() {
            let _ = self.stop();
            let _ = self.restore_startup_output();
            self.finish_current(relay);
            return Err(error);
        }
        self.pending_start_position_millis = None;
        self.seekable = media.seekable
            && self
                .transport_actions()
                .is_ok_and(|actions| actions.allows("Seek"));
        if position_millis > 0 && media.seekable {
            self.pending_start_position_millis = Some(position_millis);
        }
        self.started = true;
        let mut events = vec![
            BackendEvent::Started { run },
            BackendEvent::Seekable {
                run,
                seekable: self.seekable,
            },
            BackendEvent::State {
                run,
                state: if hold_output {
                    BackendState::Buffering
                } else {
                    BackendState::Playing
                },
            },
        ];
        if let Some(millis) = self.current_duration_millis() {
            events.push(BackendEvent::Duration { run, millis });
        }
        events.extend(self.prepare_next(run, next, relay)?);
        Ok(events)
    }

    fn install_current(&mut self, run: RunId, stream: PreparedStream, media: PublishedResource) {
        self.last_position_millis = media.logical_offset_millis;
        self.current = Some(QueuedMedia::new(run, stream, media));
        self.renderer_owned = true;
    }

    fn prepare_next(
        &mut self,
        current_run: RunId,
        next: Option<PreparedNext>,
        relay: &RelayServer,
    ) -> Result<Vec<BackendEvent>, String> {
        let previous = self.next_media.take();
        let had_previous = previous.is_some();
        if let Some(previous) = previous {
            relay.remove(&previous.published);
        }
        let Some(next) = next else {
            if had_previous {
                let _ = self.set_next_uri("", "");
            }
            return Ok(Vec::new());
        };
        let next_run = next.run;
        let representation = match self
            .sink_protocols
            .representation_for(&source_content_type(&next.stream))
        {
            Ok(representation) => representation,
            Err(error) => {
                tracing::debug!(%error, %current_run, %next_run, "UPnP next item has no compatible representation");
                return Ok(Vec::new());
            }
        };
        let published = match relay.publish_as(&next.stream, representation) {
            Ok(published) => published,
            Err(error) => {
                tracing::debug!(%error, %current_run, %next_run, "UPnP next item could not be published");
                return Ok(Vec::new());
            }
        };
        if let Err(error) =
            self.set_next_uri(&published.uri, &didl_metadata(&next.stream, &published))
        {
            relay.remove(&published);
            tracing::debug!(%error, %current_run, %next_run, "UPnP renderer did not accept a next item");
            return Ok(Vec::new());
        }
        self.next_media = Some(QueuedMedia::new(next_run, next.stream, published));
        Ok(Vec::new())
    }

    fn accept_next_transition(
        &mut self,
        position: std::collections::HashMap<String, String>,
        transport_state: Option<&str>,
        relay: &RelayServer,
    ) -> Result<Vec<BackendEvent>, String> {
        let old_run = self
            .current_run()
            .ok_or_else(|| "UPnP current run is unavailable".to_string())?;
        let next = self
            .next_media
            .take()
            .ok_or_else(|| "UPnP next media is unavailable".to_string())?;
        if let Some(current) = self.current.take() {
            relay.remove(&current.published);
        }
        let new_run = next.run;
        self.last_position_millis = next.published.logical_offset_millis;
        self.current = Some(next);
        self.pending_seek = None;
        self.pending_start_position_millis = None;
        self.renderer_owned = true;
        self.started = true;
        self.seekable = self
            .current
            .as_ref()
            .is_some_and(|media| media.published.seekable)
            && self
                .transport_actions()
                .is_ok_and(|actions| actions.allows("Seek"));
        let position_millis = position
            .get("RelTime")
            .and_then(|value| parse_upnp_time(value))
            .map(|renderer_position| self.current_logical_position_millis(renderer_position))
            .unwrap_or(self.current_logical_offset_millis());
        self.last_position_millis = position_millis;
        let state = match transport_state {
            Some("PLAYING") => BackendState::Playing,
            Some("PAUSED_PLAYBACK") | Some("PAUSED_RECORDING") => BackendState::Paused,
            Some("STOPPED") | Some("NO_MEDIA_PRESENT") => BackendState::Stopped,
            _ => BackendState::Buffering,
        };
        let mut events = vec![
            BackendEvent::Transitioned { old_run, new_run },
            BackendEvent::Seekable {
                run: new_run,
                seekable: self.seekable,
            },
            BackendEvent::Position {
                run: new_run,
                millis: position_millis,
            },
            BackendEvent::State {
                run: new_run,
                state,
            },
        ];
        if let Some(millis) = self.current_duration_millis() {
            events.push(BackendEvent::Duration {
                run: new_run,
                millis,
            });
        }
        Ok(events)
    }

    fn position_is_at_end(
        &self,
        position_millis: u64,
        position: &std::collections::HashMap<String, String>,
    ) -> bool {
        let end = self.current_duration_millis().or_else(|| {
            position
                .get("TrackDuration")
                .and_then(|duration| parse_upnp_time(duration))
                .map(|duration| {
                    self.current_logical_offset_millis()
                        .saturating_add(duration)
                })
        });
        end.is_some_and(|end| {
            position_millis.saturating_add(END_POSITION_TOLERANCE_MILLIS) >= end
                || self
                    .last_position_millis
                    .saturating_add(END_POSITION_TOLERANCE_MILLIS)
                    >= end
        })
    }

    fn finish_current(&mut self, relay: &RelayServer) {
        let _ = self.restore_startup_output();
        if let Some(current) = self.current.take() {
            relay.remove(&current.published);
        }
        if let Some(next) = self.next_media.take() {
            relay.remove(&next.published);
        }
        self.pending_seek = None;
        self.pending_start_position_millis = None;
        self.started = false;
        self.renderer_owned = false;
        self.seekable = false;
    }

    fn current_run(&self) -> Option<RunId> {
        self.current.as_ref().map(|media| media.run)
    }

    fn current_logical_offset_millis(&self) -> u64 {
        self.current
            .as_ref()
            .map(|media| media.published.logical_offset_millis)
            .unwrap_or_default()
    }

    fn current_logical_position_millis(&self, renderer_position_millis: u64) -> u64 {
        self.current
            .as_ref()
            .map(|media| {
                media
                    .published
                    .logical_position_millis(renderer_position_millis)
            })
            .unwrap_or(renderer_position_millis)
    }

    fn current_duration_millis(&self) -> Option<u64> {
        self.current
            .as_ref()
            .and_then(|media| media.duration_millis)
    }

    fn seek(&self, millis: u64) -> Result<(), String> {
        self.action(
            AV_TRANSPORT,
            "Seek",
            &format!(
                "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{}</Target>",
                format_upnp_time(millis)
            ),
        )?;
        Ok(())
    }

    fn seek_once(&mut self, millis: u64) -> Result<(), String> {
        let state = self.transport_state()?;
        let paused = state == "PAUSED_PLAYBACK" || state == "PAUSED_RECORDING";
        if paused {
            self.play()?;
        }
        let result = self.seek_logical_position(millis);
        if paused {
            let _ = self.pause();
        }
        if let Ok(target_millis) = &result {
            tracing::debug!(
                requested_millis = millis,
                target_millis,
                paused,
                "sent UPnP seek"
            );
            self.pending_seek = Some(PendingSeek {
                origin_millis: self.last_position_millis,
                target_millis: *target_millis,
                remaining_samples: SEEK_POSITION_SAMPLES,
            });
        }
        result.map(|_| ())
    }

    fn seek_logical_position(&self, millis: u64) -> Result<u64, String> {
        let logical_offset = self.current_logical_offset_millis();
        let renderer_position = quantize_upnp_time(millis.saturating_sub(logical_offset));
        self.seek(renderer_position)?;
        Ok(logical_offset.saturating_add(renderer_position))
    }

    fn accept_position_after_seek(&mut self, observed_millis: u64) -> bool {
        let Some(mut pending) = self.pending_seek.take() else {
            return true;
        };
        tracing::debug!(
            target_millis = pending.target_millis,
            observed_millis,
            difference_millis = observed_millis as i128 - pending.target_millis as i128,
            "observed UPnP position after seek"
        );
        match pending.observe(observed_millis) {
            SeekObservation::Reached => {
                self.last_position_millis = observed_millis;
                false
            }
            SeekObservation::Expired => true,
            SeekObservation::Waiting => {
                if self.startup_output.is_some() {
                    match self.seek_logical_position(pending.target_millis) {
                        Ok(_) => tracing::debug!(
                            target_millis = pending.target_millis,
                            "retried UPnP startup seek"
                        ),
                        Err(error) => tracing::debug!(
                            %error,
                            target_millis = pending.target_millis,
                            "UPnP startup seek retry was rejected"
                        ),
                    }
                }
                self.pending_seek = Some(pending);
                false
            }
        }
    }

    fn transport_state(&self) -> Result<String, String> {
        self.action(
            AV_TRANSPORT,
            "GetTransportInfo",
            "<InstanceID>0</InstanceID>",
        )?
        .remove("CurrentTransportState")
        .ok_or_else(|| "UPnP renderer did not report its transport state".to_string())
    }

    fn transport_actions(&self) -> Result<TransportActions, String> {
        self.action(
            AV_TRANSPORT,
            "GetCurrentTransportActions",
            "<InstanceID>0</InstanceID>",
        )?
        .remove("Actions")
        .map(|actions| TransportActions::parse(&actions))
        .ok_or_else(|| "UPnP renderer did not report its transport actions".to_string())
    }

    fn read_sink_protocols(&self) -> SinkProtocols {
        match self.action(CONNECTION_MANAGER, "GetProtocolInfo", "") {
            Ok(mut values) => values
                .remove("Sink")
                .map_or_else(|| SinkProtocols(None), |sink| SinkProtocols::known(&sink)),
            Err(error) => {
                tracing::debug!(%error, "UPnP renderer sink capabilities are unavailable");
                SinkProtocols(None)
            }
        }
    }

    fn play(&self) -> Result<(), String> {
        self.action(
            AV_TRANSPORT,
            "Play",
            "<InstanceID>0</InstanceID><Speed>1</Speed>",
        )?;
        Ok(())
    }

    fn pause(&self) -> Result<(), String> {
        self.action(AV_TRANSPORT, "Pause", "<InstanceID>0</InstanceID>")?;
        Ok(())
    }

    fn set_uri(&self, uri: &str, metadata: &str) -> Result<(), String> {
        self.action(
            AV_TRANSPORT,
            "SetAVTransportURI",
            &format!(
                "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI><CurrentURIMetaData>{}</CurrentURIMetaData>",
                xml_escape(uri),
                xml_escape(metadata),
            ),
        )?;
        Ok(())
    }

    fn reset_transport(&self) -> Result<(), String> {
        if self.transport_action_allowed("Stop") {
            self.stop_transport()?;
        }
        Ok(())
    }

    fn transport_action_allowed(&self, action: &str) -> bool {
        match self.transport_actions() {
            Ok(actions) => actions.allows(action),
            Err(error) => {
                tracing::debug!(%error, action, "UPnP transport action availability could not be observed");
                true
            }
        }
    }

    fn is_playing(&self) -> bool {
        self.transport_state()
            .is_ok_and(|state| state.eq_ignore_ascii_case("PLAYING"))
    }

    fn start_transport(&self) -> Result<(), String> {
        self.start_transport_until(Instant::now() + PLAY_READY_TIMEOUT)
    }

    fn start_transport_until(&self, deadline: Instant) -> Result<(), String> {
        if self.is_playing() {
            return Ok(());
        }
        if !self.wait_for_play_ready_until(deadline) && !self.is_playing() {
            return Err("UPnP renderer did not become ready to play".to_string());
        }
        if self.is_playing() {
            return Ok(());
        }
        self.play()
    }

    fn wait_for_play_ready_until(&self, deadline: Instant) -> bool {
        loop {
            match self.transport_actions() {
                Ok(actions) if actions.allows("Play") => return true,
                Ok(_) if Instant::now() < deadline => {
                    std::thread::sleep(PLAY_READY_POLL_INTERVAL);
                }
                Ok(_) => {
                    tracing::debug!(
                        "UPnP renderer did not advertise Play before the readiness deadline"
                    );
                    return false;
                }
                Err(error) => {
                    tracing::debug!(%error, "UPnP Play readiness could not be observed");
                    return true;
                }
            }
        }
    }

    fn set_next_uri(&self, uri: &str, metadata: &str) -> Result<(), String> {
        self.action(
            AV_TRANSPORT,
            "SetNextAVTransportURI",
            &format!(
                "<InstanceID>0</InstanceID><NextURI>{}</NextURI><NextURIMetaData>{}</NextURIMetaData>",
                xml_escape(uri),
                xml_escape(metadata),
            ),
        )?;
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        if self.current.is_some() && self.transport_action_allowed("Stop") {
            self.stop_transport()?;
        }
        Ok(())
    }

    fn stop_transport(&self) -> Result<(), String> {
        self.action(AV_TRANSPORT, "Stop", "<InstanceID>0</InstanceID>")?;
        Ok(())
    }

    fn volume(&self) -> Result<(f64, bool), String> {
        let volume = self.action(
            RENDERING_CONTROL,
            "GetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        )?;
        let muted = self.action(
            RENDERING_CONTROL,
            "GetMute",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        )?;
        Ok((
            volume
                .get("CurrentVolume")
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(100.0)
                / 100.0,
            muted
                .get("CurrentMute")
                .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true")),
        ))
    }

    fn set_volume(&self, volume: f64, muted: bool) -> Result<(), String> {
        let desired = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        let applied = if muted { 0 } else { desired };
        self.action(
            RENDERING_CONTROL,
            "SetVolume",
            &format!(
                "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>{applied}</DesiredVolume>"
            ),
        )?;
        self.action(
            RENDERING_CONTROL,
            "SetMute",
            &format!(
                "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredMute>{}</DesiredMute>",
                u8::from(muted)
            ),
        )?;
        Ok(())
    }

    fn hold_startup_output(&mut self) -> Result<(), String> {
        let (volume, muted) = self.volume()?;
        self.startup_output = Some(HeldOutput { volume, muted });
        if let Err(error) = self.set_volume(volume, true) {
            let _ = self.restore_startup_output();
            return Err(error);
        }
        Ok(())
    }

    fn restore_startup_output(&mut self) -> Result<(), String> {
        let Some(output) = self.startup_output.take() else {
            return Ok(());
        };
        self.set_volume(output.volume, output.muted)
    }

    fn action(
        &self,
        service_type: &str,
        action: &str,
        payload: &str,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let started = Instant::now();
        let result = self
            .device
            .action(service_type, action, payload)
            .map_err(|error| format!("UPnP {action} failed: {error}"));
        let elapsed = started.elapsed();
        if result.is_err() || elapsed.as_millis() >= 250 {
            tracing::debug!(
                action,
                local_address = ?self.device.local_address(),
                elapsed_ms = elapsed.as_millis(),
                "completed UPnP action"
            );
        }
        result
    }
}

fn format_upnp_time(millis: u64) -> String {
    let seconds = millis / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn quantize_upnp_time(millis: u64) -> u64 {
    millis / 1_000 * 1_000
}

fn parse_upnp_time(value: &str) -> Option<u64> {
    let mut pieces = value.split(':');
    let hours = pieces.next()?.parse::<u64>().ok()?;
    let minutes = pieces.next()?.parse::<u64>().ok()?;
    let seconds = pieces.next()?.split('.').next()?.parse::<u64>().ok()?;
    Some((hours * 3_600 + minutes * 60 + seconds) * 1_000)
}

fn didl_metadata(stream: &PreparedStream, media: &PublishedResource) -> String {
    let Some(track) = stream.track.as_ref() else {
        return String::new();
    };
    let duration_millis = media
        .resource_duration_millis
        .unwrap_or_else(|| u64::try_from(track.duration_millis).unwrap_or_default());
    let mut fields = format!(
        "<dc:title>{}</dc:title><dc:creator>{}</dc:creator><upnp:artist>{}</upnp:artist><upnp:album>{}</upnp:album>",
        xml_escape(&track.title),
        xml_escape(&track.artist),
        xml_escape(&track.artist),
        xml_escape(&track.album),
    );
    if let Some(track_number) = track.track_number.filter(|number| *number > 0) {
        fields.push_str(&format!(
            "<upnp:originalTrackNumber>{}</upnp:originalTrackNumber>",
            track_number
        ));
    }
    if let Some(date) = track
        .release_date
        .as_deref()
        .filter(|date| !date.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            track
                .year
                .filter(|year| *year > 0)
                .map(|year| year.to_string())
        })
    {
        fields.push_str(&format!("<dc:date>{}</dc:date>", xml_escape(&date)));
    }
    if let Some(artwork_uri) = &media.artwork_uri {
        fields.push_str(&format!(
            "<upnp:albumArtURI>{}</upnp:albumArtURI>",
            xml_escape(artwork_uri)
        ));
    }
    let protocol_info = dlna_protocol_info(media);
    let size = media
        .content_length
        .map(|length| format!(" size=\"{length}\""))
        .unwrap_or_default();
    format!(
        concat!(
            "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" ",
            "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
            "xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">",
            "<item id=\"{}\" parentID=\"0\" restricted=\"1\">{}",
            "<upnp:class>object.item.audioItem.musicTrack</upnp:class>",
            "<res protocolInfo=\"http-get:*:{}:{}\" duration=\"{}\"{}>{}</res>",
            "</item></DIDL-Lite>"
        ),
        xml_escape(&track.track_object_id),
        fields,
        xml_escape(&media.content_type),
        protocol_info,
        format_upnp_time(duration_millis),
        size,
        xml_escape(&media.uri),
    )
}

fn dlna_protocol_info(media: &PublishedResource) -> String {
    let profile = match media
        .content_type
        .split(';')
        .next()
        .unwrap_or(&media.content_type)
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/mpeg" | "audio/mp3" => "DLNA.ORG_PN=MP3;",
        _ => "",
    };
    let (operation, converted) = if media.seekable {
        ("01", "0")
    } else {
        ("00", "1")
    };
    format!(
        "{profile}DLNA.ORG_OP={operation};DLNA.ORG_CI={converted};DLNA.ORG_FLAGS=01500000000000000000000000000000"
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, mpsc};
    use std::thread;

    use tiny_http::{Response, Server};
    use url::Url;

    use super::*;
    use crate::upnp_transport::service_type_matches;

    const TEST_DEVICE_DESCRIPTION: &str = r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service></serviceList></device></root>"#;
    const TEST_CAPABLE_DEVICE_DESCRIPTION: &str = r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service><service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType><serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId><SCPDURL>/connection.xml</SCPDURL><controlURL>/connection</controlURL><eventSubURL>/connection-events</eventSubURL></service></serviceList></device></root>"#;

    fn test_device(address: std::net::SocketAddr) -> UpnpDevice {
        UpnpDevice::from_url(&format!("http://{address}/device.xml"), None, None)
            .expect("device description")
    }

    fn test_stream() -> (tempfile::TempDir, PreparedStream) {
        let directory = tempfile::tempdir().expect("track directory");
        let path = directory.path().join("track.mp3");
        File::create(&path)
            .expect("create track")
            .write_all(b"track")
            .expect("write track");
        let stream = PreparedStream::from(playback::ResolvedStream::new(
            Url::from_file_path(path).expect("track URL").to_string(),
        ))
        .with_media(test_track(), Some("audio/mpeg".to_string()));
        (directory, stream)
    }

    fn test_renderer(
        description: &'static str,
        action_count: usize,
        mut respond: impl FnMut(&str, &str) -> Result<String, String> + Send + 'static,
    ) -> (
        std::net::SocketAddr,
        mpsc::Receiver<(String, String)>,
        thread::JoinHandle<()>,
    ) {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let (sent, received) = mpsc::channel();
        let renderer = thread::spawn(move || {
            server
                .recv()
                .expect("description request")
                .respond(Response::from_string(description))
                .expect("device description response");
            for _ in 0..action_count {
                let mut request = server.recv().expect("renderer request");
                let action = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("SOAPAction"))
                    .map(|header| header.value.as_str().to_string())
                    .expect("SOAP action");
                let mut body = String::new();
                request
                    .as_reader()
                    .read_to_string(&mut body)
                    .expect("SOAP body");
                sent.send((action.clone(), body.clone()))
                    .expect("record SOAP request");
                match respond(&action, &body) {
                    Ok(values) => request
                        .respond(Response::from_string(format!(
                            r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:Response xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">{values}</u:Response></s:Body></s:Envelope>"#,
                        )))
                        .expect("renderer response"),
                    Err(error) => request
                        .respond(Response::from_string(error).with_status_code(500))
                        .expect("renderer failure response"),
                }
            }
        });
        (address, received, renderer)
    }

    #[test]
    fn transport_urls_are_xml_escaped() {
        assert_eq!(
            xml_escape("http://host/a?x=1&y=2"),
            "http://host/a?x=1&amp;y=2"
        );
    }

    #[test]
    fn seek_confirmation_uses_the_exact_upnp_time_that_was_sent() {
        assert_eq!(quantize_upnp_time(23_755), 23_000);
        assert_eq!(format_upnp_time(23_755), "00:00:23");
    }

    #[test]
    fn renderer_services_match_supported_newer_versions() {
        assert!(service_type_matches(
            "urn:schemas-upnp-org:service:AVTransport:3",
            "AVTransport",
        ));
        assert!(!service_type_matches(
            "urn:schemas-upnp-org:service:RenderingControl:3",
            "AVTransport",
        ));
    }

    #[test]
    fn renderer_sink_formats_select_a_compatible_media_representation() {
        let mp3_only = SinkProtocols::known("http-get:*:audio/mpeg:DLNA.ORG_PN=MP3");
        assert_eq!(
            mp3_only.representation_for("audio/flac"),
            Ok(RelayRepresentation::Mp3)
        );

        let flac = SinkProtocols::known("http-get:*:audio/flac:*");
        assert_eq!(
            flac.representation_for("audio/flac"),
            Ok(RelayRepresentation::Source)
        );

        let wildcard = SinkProtocols::known("http-get:*:*:*");
        assert_eq!(
            wildcard.representation_for("audio/ogg"),
            Ok(RelayRepresentation::Source)
        );
        assert_eq!(
            SinkProtocols(None).representation_for("audio/flac"),
            Ok(RelayRepresentation::Source)
        );
        assert!(
            SinkProtocols::known("")
                .representation_for("audio/flac")
                .is_err()
        );

        assert!(TransportActions::parse("PLAY, pause,Seek").allows("Play"));
        assert!(TransportActions::parse("").allows("Stop"));
    }

    #[test]
    fn mp3_only_renderer_receives_a_compatible_uri_and_metadata() {
        let (address, received, renderer) =
            test_renderer(TEST_CAPABLE_DEVICE_DESCRIPTION, 8, |action, _| {
                Ok(if action.contains("GetProtocolInfo") {
                    "<Sink>http-get:*:audio/mpeg:DLNA.ORG_PN=MP3</Sink>"
                } else if action.contains("GetTransportInfo") {
                    "<CurrentTransportState>STOPPED</CurrentTransportState>"
                } else if action.contains("GetCurrentTransportActions") {
                    "<Actions>Play</Actions>"
                } else {
                    ""
                }
                .to_string())
            });
        let device = test_device(address);
        let directory = tempfile::tempdir().expect("track directory");
        let path = directory.path().join("track.flac");
        File::create(&path)
            .expect("create track")
            .write_all(b"flac")
            .expect("write track");
        let stream = PreparedStream::from(playback::ResolvedStream::new(
            Url::from_file_path(path).expect("track URL").to_string(),
        ))
        .with_media(test_track(), Some("audio/flac".to_string()));
        let relay =
            RelayServer::start(address, Arc::new(AtomicBool::new(false)), None).expect("relay");
        let mut controller = UpnpController::new(device).expect("controller");
        controller.verify_connection().expect("connection probe");

        controller
            .start(RunId::new(1), stream, None, 42_000, &relay)
            .expect("start compatible representation");

        let actions = received.try_iter().collect::<Vec<_>>();
        let (_, set_uri) = actions
            .iter()
            .find(|(action, _)| action.contains("SetAVTransportURI"))
            .expect("SetAVTransportURI request");
        assert!(set_uri.contains("/media.mp3"), "body={set_uri}");
        assert!(set_uri.contains("audio/mpeg"), "body={set_uri}");
        assert!(
            set_uri.contains("duration=&quot;00:04:18&quot;"),
            "body={set_uri}"
        );
        assert!(!set_uri.contains("audio/flac"), "body={set_uri}");
        assert!(!actions.iter().any(|(action, _)| action.contains("#Seek")));
        assert_eq!(
            controller
                .current
                .as_ref()
                .map(|current| current.published.logical_offset_millis),
            Some(42_000)
        );
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn connection_probe_rejects_an_unusable_control_service() {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let renderer = thread::spawn(move || {
            let description = server.recv().expect("description request");
            description
                .respond(Response::from_string(TEST_DEVICE_DESCRIPTION))
                .expect("device description response");
            let probe = server.recv().expect("connection probe");
            probe
                .respond(Response::from_string("renderer unavailable").with_status_code(500))
                .expect("connection failure response");
        });
        let device = test_device(address);
        let mut controller = UpnpController::new(device).expect("controller");

        let error = controller
            .verify_connection()
            .expect_err("unusable control service");

        assert!(error.contains("GetTransportInfo"), "error={error}");
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn already_playing_renderer_does_not_receive_another_play_command() {
        let (address, received, renderer) = test_renderer(TEST_DEVICE_DESCRIPTION, 1, |_, _| {
            Ok("<CurrentTransportState>PLAYING</CurrentTransportState>".to_string())
        });
        let device = test_device(address);
        let controller = UpnpController::new(device).expect("controller");

        controller.start_transport().expect("already playing");

        let actions = received.try_iter().collect::<Vec<_>>();
        assert_eq!(actions.len(), 1);
        assert!(actions[0].0.contains("GetTransportInfo"));
        assert!(!actions[0].0.contains("#Play"));
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn unavailable_play_action_is_not_sent_after_the_readiness_deadline() {
        let (address, received, renderer) =
            test_renderer(TEST_DEVICE_DESCRIPTION, 3, |action, _| {
                Ok(if action.contains("GetTransportInfo") {
                    "<CurrentTransportState>STOPPED</CurrentTransportState>"
                } else if action.contains("GetCurrentTransportActions") {
                    "<Actions>Stop</Actions>"
                } else {
                    ""
                }
                .to_string())
            });
        let device = test_device(address);
        let controller = UpnpController::new(device).expect("controller");

        let error = controller
            .start_transport_until(Instant::now())
            .expect_err("unavailable Play action");

        assert!(error.contains("did not become ready"));
        let actions = received.try_iter().collect::<Vec<_>>();
        assert_eq!(actions.len(), 3);
        assert!(!actions.iter().any(|(action, _)| action.contains("#Play")));
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn start_resets_transport_and_waits_until_play_is_available() {
        let mut stopped = false;
        let mut action_polls = 0;
        let (address, received, renderer) =
            test_renderer(TEST_DEVICE_DESCRIPTION, 9, move |action, _| {
                if action.contains("#Stop") {
                    stopped = true;
                }
                if action.contains("SetAVTransportURI") && !stopped {
                    return Err("transport locked".to_string());
                }
                Ok(if action.contains("GetTransportInfo") {
                    if stopped {
                        "<CurrentTransportState>STOPPED</CurrentTransportState>"
                    } else {
                        "<CurrentTransportState>LG_TRANSITIONING</CurrentTransportState>"
                    }
                    .to_string()
                } else if action.contains("GetCurrentTransportActions") {
                    action_polls += 1;
                    if action_polls <= 2 {
                        "<Actions>Stop</Actions>".to_string()
                    } else {
                        "<Actions>Play,Stop,Seek</Actions>".to_string()
                    }
                } else {
                    String::new()
                })
            });
        let device = test_device(address);
        let (_directory, stream) = test_stream();
        let relay =
            RelayServer::start(address, Arc::new(AtomicBool::new(false)), None).expect("relay");
        let mut controller = UpnpController::new(device).expect("controller");

        controller
            .start(RunId::new(1), stream, None, 0, &relay)
            .expect("start after renderer reset");

        let actions = received.try_iter().collect::<Vec<_>>();
        assert!(actions[0].0.contains("GetCurrentTransportActions"));
        assert!(actions[1].0.contains("#Stop"));
        assert!(actions[2].0.contains("SetAVTransportURI"));
        assert!(actions[3].0.contains("GetTransportInfo"));
        assert!(actions[4].0.contains("GetCurrentTransportActions"));
        assert!(actions[5].0.contains("GetCurrentTransportActions"));
        assert!(actions[6].0.contains("GetTransportInfo"));
        assert!(actions[7].0.contains("#Play"));
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn play_failure_stops_the_uri_owned_by_the_controller() {
        let (address, received, renderer) =
            test_renderer(TEST_DEVICE_DESCRIPTION, 9, |action, _| {
                if action.contains("#Play") {
                    return Err("play failed".to_string());
                }
                Ok(if action.contains("GetTransportInfo") {
                    "<CurrentTransportState>STOPPED</CurrentTransportState>"
                } else if action.contains("GetCurrentTransportActions") {
                    "<Actions>Play,Stop,Seek</Actions>"
                } else {
                    ""
                }
                .to_string())
            });
        let device = test_device(address);
        let (_directory, stream) = test_stream();
        let relay =
            RelayServer::start(address, Arc::new(AtomicBool::new(false)), None).expect("relay");
        let mut controller = UpnpController::new(device).expect("controller");

        let error = controller
            .start(RunId::new(1), stream, None, 0, &relay)
            .expect_err("failed Play");

        assert!(error.contains("UPnP Play failed"));
        assert!(controller.current.is_none());
        let actions = received.try_iter().collect::<Vec<_>>();
        assert!(actions[0].0.contains("GetCurrentTransportActions"));
        assert!(actions[1].0.contains("#Stop"));
        assert!(actions[2].0.contains("SetAVTransportURI"));
        assert!(actions[3].0.contains("GetTransportInfo"));
        assert!(actions[4].0.contains("GetCurrentTransportActions"));
        assert!(actions[5].0.contains("GetTransportInfo"));
        assert!(actions[6].0.contains("#Play"));
        assert!(actions[7].0.contains("GetCurrentTransportActions"));
        assert!(actions[8].0.contains("#Stop"));
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn stale_renderer_positions_do_not_cross_a_seek_boundary() {
        let mut forward = PendingSeek {
            origin_millis: 25_000,
            target_millis: 99_000,
            remaining_samples: SEEK_POSITION_SAMPLES,
        };
        assert_eq!(forward.observe(25_000), SeekObservation::Waiting);
        assert_eq!(forward.observe(99_000), SeekObservation::Reached);

        let mut backward = PendingSeek {
            origin_millis: 180_000,
            target_millis: 60_000,
            remaining_samples: SEEK_POSITION_SAMPLES,
        };
        assert_eq!(backward.observe(180_000), SeekObservation::Waiting);
        assert_eq!(backward.observe(59_000), SeekObservation::Reached);
    }

    #[test]
    fn a_renderer_that_never_moves_releases_position_updates() {
        let mut pending = PendingSeek {
            origin_millis: 25_000,
            target_millis: 99_000,
            remaining_samples: 2,
        };
        assert_eq!(pending.observe(25_000), SeekObservation::Waiting);
        assert_eq!(pending.observe(25_000), SeekObservation::Expired);
    }

    #[test]
    fn cue_start_uses_a_bounded_representation_and_prepares_next() {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let (sent, received) = mpsc::channel();
        let renderer = thread::spawn(move || {
            let mut current_uri = String::new();
            let mut seeked = false;
            let mut playing = false;
            for index in 0..12 {
                let mut request = server.recv().expect("renderer request");
                if index == 0 {
                    request
                        .respond(Response::from_string(TEST_DEVICE_DESCRIPTION))
                        .expect("device description response");
                    continue;
                }
                let action = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("SOAPAction"))
                    .map(|header| header.value.as_str().to_string())
                    .expect("SOAP action");
                let mut body = String::new();
                request
                    .as_reader()
                    .read_to_string(&mut body)
                    .expect("SOAP body");
                if action.contains("SetAVTransportURI") {
                    current_uri = xml_element(&body, "CurrentURI").unwrap_or_default();
                } else if action.contains("SetNextAVTransportURI") {
                    current_uri = xml_element(&body, "NextURI").unwrap_or_default();
                } else if action.contains("#Play") {
                    playing = true;
                } else if action.contains("#Seek") {
                    seeked = true;
                }
                sent.send((action, body)).expect("record SOAP request");
                let values = if request
                    .headers()
                    .iter()
                    .any(|header| header.value.as_str().contains("GetTransportInfo"))
                {
                    if playing {
                        "<CurrentTransportState>PLAYING</CurrentTransportState>".to_string()
                    } else {
                        "<CurrentTransportState>STOPPED</CurrentTransportState>".to_string()
                    }
                } else if request
                    .headers()
                    .iter()
                    .any(|header| header.value.as_str().contains("GetCurrentTransportActions"))
                {
                    "<Actions>Play,Pause,Stop,Seek</Actions>".to_string()
                } else if request
                    .headers()
                    .iter()
                    .any(|header| header.value.as_str().contains("GetPositionInfo"))
                {
                    if seeked {
                        format!(
                            "<TrackDuration>00:04:19</TrackDuration><TrackURI>{current_uri}</TrackURI><RelTime>00:00:12</RelTime>"
                        )
                    } else {
                        format!(
                            "<TrackDuration>00:04:19</TrackDuration><TrackURI>{current_uri}</TrackURI><RelTime>00:00:00</RelTime>"
                        )
                    }
                } else {
                    String::new()
                };
                request
                    .respond(Response::from_string(format!(
                        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:Response xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">{values}</u:Response></s:Body></s:Envelope>"#,
                    )))
                    .expect("SOAP response");
            }
        });

        let device = test_device(address);
        let directory = tempfile::tempdir().expect("track directory");
        let path = directory.path().join("track.mp3");
        File::create(&path)
            .expect("create track")
            .write_all(b"track")
            .expect("write track");
        let next_path = directory.path().join("next.mp3");
        File::create(&next_path)
            .expect("create next track")
            .write_all(b"next")
            .expect("write next track");
        let stream = PreparedStream::from(
            playback::ResolvedStream::new(
                Url::from_file_path(path).expect("track URL").to_string(),
            )
            .with_window(10_000, 20_000),
        );
        let next = PreparedNext::new(
            RunId::new(2),
            PreparedStream::from(playback::ResolvedStream::new(
                Url::from_file_path(next_path)
                    .expect("next track URL")
                    .to_string(),
            )),
            playback::NextTransition::Gapless,
        );
        let relay =
            RelayServer::start(address, Arc::new(AtomicBool::new(false)), None).expect("relay");
        let mut controller = UpnpController::new(device).expect("controller");

        controller
            .start(RunId::new(1), stream, Some(next), 2_000, &relay)
            .expect("start playback");
        let transition = controller.poll(&relay).expect("poll transition");

        let actions = received.try_iter().collect::<Vec<_>>();
        assert_eq!(actions.len(), 11);
        assert!(actions[0].0.contains("GetCurrentTransportActions"));
        assert!(actions[1].0.contains("#Stop"));
        assert!(actions[2].0.contains("SetAVTransportURI"));
        assert!(actions[2].1.contains("CurrentURI"));
        assert!(actions[3].0.contains("GetTransportInfo"));
        assert!(actions[4].0.contains("GetCurrentTransportActions"));
        assert!(actions[5].0.contains("GetTransportInfo"));
        assert!(actions[6].0.contains("#Play"));
        assert!(actions[7].0.contains("SetNextAVTransportURI"));
        assert!(actions[7].1.contains("<NextURI>http://"));
        assert!(actions[8].0.contains("GetTransportInfo"));
        assert!(actions[9].0.contains("GetPositionInfo"));
        assert!(actions[10].0.contains("GetCurrentTransportActions"));
        assert!(!actions.iter().any(|(action, _)| action.contains("#Seek")));
        assert!(
            transition.iter().any(|event| {
                matches!(
                    event,
                    BackendEvent::Transitioned { old_run, new_run }
                        if *old_run == RunId::new(1) && *new_run == RunId::new(2)
                )
            }),
            "transition={transition:?} actions={actions:?}"
        );
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn output_switch_stays_silent_until_one_startup_seek_is_confirmed() {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let (sent, received) = mpsc::channel();
        let renderer = thread::spawn(move || {
            let mut current_uri = String::new();
            let mut seek_attempts = 0;
            let mut playing = false;
            for index in 0..24 {
                let mut request = server.recv().expect("renderer request");
                if index == 0 {
                    let description = format!(
                        r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service><service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><serviceId>urn:upnp-org:serviceId:RenderingControl</serviceId><SCPDURL>/rendering.xml</SCPDURL><controlURL>/rendering</controlURL><eventSubURL>/rendering-events</eventSubURL></service></serviceList></device></root>"#
                    );
                    request
                        .respond(Response::from_string(description))
                        .expect("device description response");
                    continue;
                }
                let action = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("SOAPAction"))
                    .map(|header| header.value.as_str().to_string())
                    .expect("SOAP action");
                let mut body = String::new();
                request
                    .as_reader()
                    .read_to_string(&mut body)
                    .expect("SOAP body");
                if action.contains("SetAVTransportURI") {
                    current_uri = xml_element(&body, "CurrentURI").unwrap_or_default();
                } else if action.contains("#Play") {
                    playing = true;
                } else if action.contains("#Seek") {
                    seek_attempts += 1;
                }
                sent.send((action.clone(), body))
                    .expect("record SOAP request");
                let values = if action.contains("GetTransportInfo") {
                    if playing {
                        "<CurrentTransportState>PLAYING</CurrentTransportState>".to_string()
                    } else {
                        "<CurrentTransportState>STOPPED</CurrentTransportState>".to_string()
                    }
                } else if action.contains("GetVolume") {
                    "<CurrentVolume>40</CurrentVolume>".to_string()
                } else if action.contains("GetMute") {
                    "<CurrentMute>0</CurrentMute>".to_string()
                } else if action.contains("GetPositionInfo") {
                    format!(
                        "<TrackDuration>00:04:19</TrackDuration><TrackURI>{current_uri}</TrackURI><RelTime>{}</RelTime>",
                        if seek_attempts >= 2 {
                            "00:00:42"
                        } else {
                            "00:00:00"
                        }
                    )
                } else if action.contains("GetCurrentTransportActions") {
                    "<Actions>Play,Pause,Stop,Seek</Actions>".to_string()
                } else {
                    String::new()
                };
                request
                    .respond(Response::from_string(format!(
                        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:Response xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">{values}</u:Response></s:Body></s:Envelope>"#,
                    )))
                    .expect("SOAP response");
            }
        });

        let device = test_device(address);
        let directory = tempfile::tempdir().expect("track directory");
        let path = directory.path().join("track.mp3");
        File::create(&path)
            .expect("create track")
            .write_all(b"track")
            .expect("write track");
        let stream = PreparedStream::from(playback::ResolvedStream::new(
            Url::from_file_path(path).expect("track URL").to_string(),
        ));
        let mut relay =
            RelayServer::start(address, Arc::new(AtomicBool::new(false)), None).expect("relay");
        let mut controller = UpnpController::new(device).expect("controller");

        let started = controller
            .start(RunId::new(1), stream, None, 42_000, &relay)
            .expect("start playback");
        assert!(started.iter().any(|event| matches!(
            event,
            BackendEvent::State {
                run,
                state: BackendState::Buffering,
            } if *run == RunId::new(1)
        )));
        let first = controller.poll(&relay).expect("first renderer poll");
        assert!(
            !first
                .iter()
                .any(|event| matches!(event, BackendEvent::Position { millis: 0, .. }))
        );
        let restored = controller.poll(&relay).expect("restored renderer poll");
        assert!(
            !restored
                .iter()
                .any(|event| matches!(event, BackendEvent::Position { millis: 0, .. }))
        );
        let confirmed = controller.poll(&relay).expect("confirmed renderer poll");
        assert!(
            !confirmed
                .iter()
                .any(|event| matches!(event, BackendEvent::Position { millis: 0, .. }))
        );

        let actions = received.try_iter().collect::<Vec<_>>();
        assert_eq!(actions.len(), 23);
        assert!(actions[0].0.contains("GetCurrentTransportActions"));
        assert!(actions[1].0.contains("#Stop"));
        assert!(actions[2].0.contains("SetAVTransportURI"));
        assert!(actions[3].0.contains("GetVolume"));
        assert!(actions[4].0.contains("GetMute"));
        assert!(actions[5].0.contains("SetVolume"));
        assert!(actions[5].1.contains("<DesiredVolume>0</DesiredVolume>"));
        assert!(actions[6].0.contains("SetMute"));
        assert!(actions[6].1.contains("<DesiredMute>1</DesiredMute>"));
        assert!(actions[7].0.contains("GetTransportInfo"));
        assert!(actions[8].0.contains("GetCurrentTransportActions"));
        assert!(actions[9].0.contains("GetTransportInfo"));
        assert!(actions[10].0.contains("#Play"));
        assert!(actions[11].0.contains("GetCurrentTransportActions"));
        let seeks = actions
            .iter()
            .filter(|(action, _)| action.contains("#Seek"))
            .collect::<Vec<_>>();
        assert_eq!(seeks.len(), 2);
        assert!(
            seeks
                .iter()
                .all(|(_, body)| body.contains("<Target>00:00:42</Target>"))
        );
        assert!(actions[21].0.contains("SetVolume"));
        assert!(actions[21].1.contains("<DesiredVolume>40</DesiredVolume>"));
        assert!(actions[22].0.contains("SetMute"));
        assert!(actions[22].1.contains("<DesiredMute>0</DesiredMute>"));
        renderer.join().expect("renderer thread");
        relay.shutdown();
    }

    #[test]
    fn transient_status_failure_does_not_fail_upnp_playback() {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let renderer = thread::spawn(move || {
            let mut current_uri = String::new();
            let mut playing = false;
            let mut failed_status = false;
            for index in 0..12 {
                let mut request = server.recv().expect("renderer request");
                if index == 0 {
                    request
                        .respond(Response::from_string(TEST_DEVICE_DESCRIPTION))
                        .expect("device description response");
                    continue;
                }
                let action = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("SOAPAction"))
                    .map(|header| header.value.as_str().to_string())
                    .expect("SOAP action");
                let mut body = String::new();
                request
                    .as_reader()
                    .read_to_string(&mut body)
                    .expect("SOAP body");
                if action.contains("SetAVTransportURI") {
                    current_uri = xml_element(&body, "CurrentURI").unwrap_or_default();
                } else if action.contains("#Play") {
                    playing = true;
                }
                if action.contains("GetTransportInfo") && playing && !failed_status {
                    failed_status = true;
                    request
                        .respond(Response::from_string("renderer busy").with_status_code(500))
                        .expect("temporary failure response");
                    continue;
                }
                let values = if action.contains("GetTransportInfo") {
                    if playing {
                        "<CurrentTransportState>PLAYING</CurrentTransportState>".to_string()
                    } else {
                        "<CurrentTransportState>STOPPED</CurrentTransportState>".to_string()
                    }
                } else if action.contains("GetCurrentTransportActions") {
                    "<Actions>Play,Pause,Stop,Seek</Actions>".to_string()
                } else if action.contains("GetPositionInfo") {
                    format!(
                        "<TrackDuration>00:04:19</TrackDuration><TrackURI>{current_uri}</TrackURI><RelTime>00:00:12</RelTime>"
                    )
                } else {
                    String::new()
                };
                request
                    .respond(Response::from_string(format!(
                        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:Response xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">{values}</u:Response></s:Body></s:Envelope>"#,
                    )))
                    .expect("SOAP response");
            }
        });

        let device = test_device(address);
        let (_directory, stream) = test_stream();
        let relay =
            RelayServer::start(address, Arc::new(AtomicBool::new(false)), None).expect("relay");
        let mut controller = UpnpController::new(device).expect("controller");
        controller
            .start(RunId::new(1), stream, None, 0, &relay)
            .expect("start playback");

        let unavailable = controller.poll(&relay).expect("temporary status failure");
        assert!(unavailable.is_empty());
        let recovered = controller.poll(&relay).expect("status recovery");
        assert!(recovered.iter().any(|event| matches!(
            event,
            BackendEvent::Position { run, millis: 12_000 } if *run == RunId::new(1)
        )));
        assert!(recovered.iter().any(|event| matches!(
            event,
            BackendEvent::State {
                run,
                state: BackendState::Playing,
            } if *run == RunId::new(1)
        )));
        renderer.join().expect("renderer thread");
    }

    #[test]
    fn didl_metadata_carries_standard_music_facts_artwork_and_resource() {
        let stream = PreparedStream::from(playback::ResolvedStream::new("file:///track.flac"))
            .with_media(test_track(), Some("audio/flac".to_string()));
        let media = PublishedResource {
            uri: "http://192.0.2.10:4000/media".to_string(),
            content_type: "audio/flac".to_string(),
            content_length: Some(12_345),
            logical_offset_millis: 0,
            resource_duration_millis: Some(60_000),
            seekable: true,
            artwork_uri: Some("http://192.0.2.10:4000/media/artwork".to_string()),
            relay_token: None,
        };

        let metadata = didl_metadata(&stream, &media);

        assert!(metadata.contains("<dc:title>Track &amp; Title</dc:title>"));
        assert!(metadata.contains("<upnp:artist>Artist</upnp:artist>"));
        assert!(metadata.contains("<upnp:album>Album</upnp:album>"));
        assert!(!metadata.contains("rating"));
        assert!(metadata.contains("<upnp:albumArtURI>http://192.0.2.10:4000/media/artwork"));
        assert!(metadata.contains("duration=\"00:01:00\""));
        assert!(metadata.contains("size=\"12345\""));
        assert!(!metadata.contains("DLNA.ORG_PN"));
        assert!(metadata.contains("DLNA.ORG_OP=01;DLNA.ORG_CI=0"));
    }

    fn test_track() -> playback::PlaybackMedia {
        playback::PlaybackMedia {
            source_id: "source".to_string(),
            track_key: Some(library::TrackKey::from_raw(1)),
            track_object_id: "track-1".to_string(),
            title: "Track & Title".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_display_artist: Some("Album Artist".to_string()),
            album_key: None,
            primary_artist_key: None,
            media_uri: None,
            artwork_binding: None,
            duration_millis: 300_000,
            disc_number: Some(1),
            track_number: Some(2),
            year: Some(2026),
            release_date: Some("2026-08-17".to_string()),
            favorite: Some(true),
            rating: None,
            is_downloaded: false,
            source_format: Some("flac".to_string()),
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            musicbrainz_album_id: None,
            musicbrainz_release_group_id: None,
            primary_artist_musicbrainz_id: None,
            cue_path: None,
            cue_start_millis: None,
            cue_end_millis: None,
            artist_links: Vec::new(),
        }
    }

    fn xml_element(body: &str, name: &str) -> Option<String> {
        let start = format!("<{name}>");
        let end = format!("</{name}>");
        let value = body.split_once(&start)?.1.split_once(&end)?.0;
        Some(value.to_string())
    }
}
