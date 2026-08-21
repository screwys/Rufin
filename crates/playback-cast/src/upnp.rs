use std::time::{Duration, Instant};

use playback::{BackendCommand, BackendEvent, BackendState, PreparedNext, PreparedStream, RunId};
use rupnp::ssdp::URN;
use rupnp::{Device, Service};

use crate::relay::{PublishedResource, RelayServer};

const AV_TRANSPORT: URN = URN::service("schemas-upnp-org", "AVTransport", 1);
const RENDERING_CONTROL: URN = URN::service("schemas-upnp-org", "RenderingControl", 1);
const END_POSITION_TOLERANCE_MILLIS: u64 = 2_000;
const SEEK_POSITION_SAMPLES: u8 = 4;
const ACTION_TIMEOUT: Duration = Duration::from_secs(10);

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
                    .map(|track| u64::from(track.duration_seconds) * 1_000)
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
    runtime: tokio::runtime::Runtime,
    device: Device,
    current: Option<QueuedMedia>,
    next_media: Option<QueuedMedia>,
    last_absolute_position_millis: u64,
    pending_seek: Option<PendingSeek>,
    pending_start_position_millis: Option<u64>,
    started: bool,
    renderer_owned: bool,
    seekable: bool,
    startup_output: Option<HeldOutput>,
    observation_unavailable: bool,
}

impl UpnpController {
    pub(crate) fn new(device: Device) -> Result<Self, String> {
        if find_service_any_version(&device, "AVTransport").is_none() {
            return Err(format!(
                "{} does not provide UPnP AVTransport",
                device.friendly_name()
            ));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            runtime,
            device,
            current: None,
            next_media: None,
            last_absolute_position_millis: 0,
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

    pub(crate) fn verify_connection(&self) -> Result<(), String> {
        let result = self.action(
            AV_TRANSPORT,
            "GetTransportInfo",
            "<InstanceID>0</InstanceID>",
        );
        if let Err(error) = &result {
            tracing::debug!(
                description_url = %self.device.url(),
                %error,
                "UPnP renderer connection probe failed"
            );
        }
        result.map(|_| ())
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
                if self.renderer_owned {
                    self.play()?;
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
                    self.set_uri(&uri, &metadata)?;
                    self.play()?;
                    self.renderer_owned = true;
                }
                self.started = true;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Playing,
                }])
            }
            BackendCommand::Pause { run } if self.current_run() == Some(run) => {
                self.pause()?;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Paused,
                }])
            }
            BackendCommand::Stop { run } if self.current_run() == Some(run) => {
                self.stop()?;
                self.finish_current(relay);
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Stopped,
                }])
            }
            BackendCommand::Seek {
                run,
                position_millis,
            } if self.current_run() == Some(run) && self.seekable => {
                let target = self.current_start_millis().saturating_add(position_millis);
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
        let absolute = position
            .get("RelTime")
            .and_then(|value| parse_upnp_time(value))
            .unwrap_or(self.last_absolute_position_millis);
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
                .is_some_and(|pending| pending.reached(absolute));
        let publish_position = !restore_was_pending && self.accept_position_after_seek(absolute);
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
            .current_end_millis()
            .is_some_and(|window_end| absolute >= window_end)
        {
            self.stop()?;
            self.finish_current(relay);
            return Ok(vec![BackendEvent::Ended { run }]);
        }
        if state == BackendState::Stopped && self.started {
            if self.position_is_at_end(absolute, &position) {
                self.finish_current(relay);
                return Ok(vec![BackendEvent::Ended { run }]);
            }
            self.started = false;
        }
        if publish_position {
            self.last_absolute_position_millis = absolute;
        }
        let relative = absolute.saturating_sub(self.current_start_millis());
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
                    millis: relative,
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
        relay.clear();
        self.next_media = None;
        let media = relay.publish(&stream)?;
        self.set_uri(&media.uri, &didl_metadata(&stream, &media))?;
        let absolute_position = media.starts_at_millis.saturating_add(position_millis);
        let hold_output = absolute_position > 0
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
        if let Err(error) = self.play() {
            let _ = self.restore_startup_output();
            return Err(error);
        }
        self.install_current(run, stream.clone(), media.clone());
        self.pending_start_position_millis = None;
        self.seekable = media.seekable
            && self
                .current_actions()
                .is_ok_and(|actions| actions.split(',').any(|action| action.trim() == "Seek"));
        if absolute_position > 0 && media.seekable {
            self.pending_start_position_millis = Some(absolute_position);
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
        self.last_absolute_position_millis = media.starts_at_millis;
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
        let published = match relay.publish(&next.stream) {
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
        self.last_absolute_position_millis = next.published.starts_at_millis;
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
                .current_actions()
                .is_ok_and(|actions| actions.split(',').any(|action| action.trim() == "Seek"));
        let absolute = position
            .get("RelTime")
            .and_then(|value| parse_upnp_time(value))
            .unwrap_or(self.current_start_millis());
        self.last_absolute_position_millis = absolute;
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
                millis: absolute.saturating_sub(self.current_start_millis()),
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
        absolute: u64,
        position: &std::collections::HashMap<String, String>,
    ) -> bool {
        let end = self
            .current_end_millis()
            .or_else(|| {
                self.current_duration_millis()
                    .map(|duration| self.current_start_millis().saturating_add(duration))
            })
            .or_else(|| {
                position
                    .get("TrackDuration")
                    .and_then(|duration| parse_upnp_time(duration))
                    .map(|duration| self.current_start_millis().saturating_add(duration))
            });
        end.is_some_and(|end| {
            absolute.saturating_add(END_POSITION_TOLERANCE_MILLIS) >= end
                || self
                    .last_absolute_position_millis
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

    fn current_start_millis(&self) -> u64 {
        self.current
            .as_ref()
            .map(|media| media.published.starts_at_millis)
            .unwrap_or_default()
    }

    fn current_end_millis(&self) -> Option<u64> {
        self.current
            .as_ref()
            .and_then(|media| media.published.ends_at_millis)
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
        let result = self.seek(millis);
        if paused {
            let _ = self.pause();
        }
        if result.is_ok() {
            tracing::debug!(target_millis = millis, paused, "sent UPnP seek");
            self.pending_seek = Some(PendingSeek {
                origin_millis: self.last_absolute_position_millis,
                target_millis: millis,
                remaining_samples: SEEK_POSITION_SAMPLES,
            });
        }
        result
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
                self.last_absolute_position_millis = observed_millis;
                false
            }
            SeekObservation::Expired => true,
            SeekObservation::Waiting => {
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

    fn current_actions(&self) -> Result<String, String> {
        self.action(
            AV_TRANSPORT,
            "GetCurrentTransportActions",
            "<InstanceID>0</InstanceID>",
        )?
        .remove("Actions")
        .ok_or_else(|| "UPnP renderer did not report its transport actions".to_string())
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
        if self.current.is_some() {
            self.action(AV_TRANSPORT, "Stop", "<InstanceID>0</InstanceID>")?;
        }
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
        service_type: URN,
        action: &str,
        payload: &str,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let service = self.service(&service_type)?;
        let started = Instant::now();
        let result = match self.runtime.block_on(async {
            tokio::time::timeout(
                ACTION_TIMEOUT,
                service.action(self.device.url(), action, payload),
            )
            .await
        }) {
            Ok(result) => result.map_err(|error| format!("UPnP {action} failed: {error}")),
            Err(_) => Err(format!("UPnP {action} timed out")),
        };
        let elapsed = started.elapsed();
        if result.is_err() || elapsed.as_millis() >= 250 {
            tracing::debug!(
                action,
                elapsed_ms = elapsed.as_millis(),
                "completed UPnP action"
            );
        }
        result
    }

    fn service(&self, service_type: &URN) -> Result<&Service, String> {
        let name = if service_type == &AV_TRANSPORT {
            "AVTransport"
        } else if service_type == &RENDERING_CONTROL {
            "RenderingControl"
        } else {
            return Err(format!("unsupported UPnP service {service_type}"));
        };
        find_service_any_version(&self.device, name).ok_or_else(|| {
            format!(
                "{} does not provide UPnP {name}",
                self.device.friendly_name()
            )
        })
    }
}

fn find_service_any_version<'a>(device: &'a Device, name: &str) -> Option<&'a Service> {
    device
        .services_iter()
        .find(|service| service_type_matches(service.service_type(), name))
}

fn service_type_matches(service_type: &URN, name: &str) -> bool {
    service_type
        .to_string()
        .contains(&format!(":service:{name}:"))
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
        .ends_at_millis
        .map(|end| end.saturating_sub(media.starts_at_millis))
        .unwrap_or_else(|| u64::from(track.duration_seconds) * 1_000);
    let mut fields = format!(
        "<dc:title>{}</dc:title><dc:creator>{}</dc:creator><upnp:artist>{}</upnp:artist><upnp:album>{}</upnp:album>",
        xml_escape(&track.title),
        xml_escape(&track.artist),
        xml_escape(&track.artist),
        xml_escape(&track.album),
    );
    if track.track_number > 0 {
        fields.push_str(&format!(
            "<upnp:originalTrackNumber>{}</upnp:originalTrackNumber>",
            track.track_number
        ));
    }
    if let Some(date) = track
        .release_date
        .as_deref()
        .filter(|date| !date.trim().is_empty())
        .map(str::to_string)
        .or_else(|| (track.year > 0).then(|| track.year.to_string()))
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
        xml_escape(track.id.as_str()),
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

    #[test]
    fn transport_urls_are_xml_escaped() {
        assert_eq!(
            xml_escape("http://host/a?x=1&y=2"),
            "http://host/a?x=1&amp;y=2"
        );
    }

    #[test]
    fn renderer_services_match_supported_newer_versions() {
        assert!(service_type_matches(
            &URN::service("schemas-upnp-org", "AVTransport", 3),
            "AVTransport",
        ));
        assert!(!service_type_matches(
            &URN::service("schemas-upnp-org", "RenderingControl", 3),
            "AVTransport",
        ));
    }

    #[test]
    fn connection_probe_rejects_an_unusable_control_service() {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let renderer = thread::spawn(move || {
            let description = server.recv().expect("description request");
            description
                .respond(Response::from_string(
                    r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service></serviceList></device></root>"#,
                ))
                .expect("device description response");
            let probe = server.recv().expect("connection probe");
            probe
                .respond(Response::from_string("renderer unavailable").with_status_code(500))
                .expect("connection failure response");
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let device = runtime
            .block_on(Device::from_url(
                format!("http://{address}/device.xml")
                    .parse()
                    .expect("device URL"),
            ))
            .expect("device description");
        let controller = UpnpController::new(device).expect("controller");

        let error = controller
            .verify_connection()
            .expect_err("unusable control service");

        assert!(error.contains("GetTransportInfo"), "error={error}");
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
            for index in 0..7 {
                let mut request = server.recv().expect("renderer request");
                if index == 0 {
                    let description = format!(
                        r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service></serviceList></device></root>"#
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
                } else if action.contains("SetNextAVTransportURI") {
                    current_uri = xml_element(&body, "NextURI").unwrap_or_default();
                } else if action.contains("#Seek") {
                    seeked = true;
                }
                sent.send((action, body)).expect("record SOAP request");
                let values = if request
                    .headers()
                    .iter()
                    .any(|header| header.value.as_str().contains("GetTransportInfo"))
                {
                    "<CurrentTransportState>PLAYING</CurrentTransportState>".to_string()
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

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let device = runtime
            .block_on(Device::from_url(
                format!("http://{address}/device.xml")
                    .parse()
                    .expect("device URL"),
            ))
            .expect("device description");
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
            library::ResolvedStream::new(Url::from_file_path(path).expect("track URL").to_string())
                .with_window(10_000, 20_000),
        );
        let next = PreparedNext::new(
            RunId::new(2),
            PreparedStream::from(library::ResolvedStream::new(
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
        assert_eq!(actions.len(), 6);
        assert!(actions[0].0.contains("SetAVTransportURI"));
        assert!(actions[0].1.contains("CurrentURI"));
        assert!(actions[1].0.contains("Play"));
        assert!(actions[2].0.contains("SetNextAVTransportURI"));
        assert!(actions[2].1.contains("<NextURI>http://"));
        assert!(actions[3].0.contains("GetTransportInfo"));
        assert!(actions[4].0.contains("GetPositionInfo"));
        assert!(actions[5].0.contains("GetCurrentTransportActions"));
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
            let mut seeked = false;
            for index in 0..18 {
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
                } else if action.contains("#Seek") {
                    seeked = true;
                }
                sent.send((action.clone(), body))
                    .expect("record SOAP request");
                let values = if action.contains("GetTransportInfo") {
                    "<CurrentTransportState>PLAYING</CurrentTransportState>".to_string()
                } else if action.contains("GetVolume") {
                    "<CurrentVolume>40</CurrentVolume>".to_string()
                } else if action.contains("GetMute") {
                    "<CurrentMute>0</CurrentMute>".to_string()
                } else if action.contains("GetPositionInfo") {
                    format!(
                        "<TrackDuration>00:04:19</TrackDuration><TrackURI>{current_uri}</TrackURI><RelTime>{}</RelTime>",
                        if seeked { "00:00:42" } else { "00:00:00" }
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

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let device = runtime
            .block_on(Device::from_url(
                format!("http://{address}/device.xml")
                    .parse()
                    .expect("device URL"),
            ))
            .expect("device description");
        let directory = tempfile::tempdir().expect("track directory");
        let path = directory.path().join("track.mp3");
        File::create(&path)
            .expect("create track")
            .write_all(b"track")
            .expect("write track");
        let stream = PreparedStream::from(library::ResolvedStream::new(
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
        assert_eq!(actions.len(), 17);
        assert!(actions[0].0.contains("SetAVTransportURI"));
        assert!(actions[1].0.contains("GetVolume"));
        assert!(actions[2].0.contains("GetMute"));
        assert!(actions[3].0.contains("SetVolume"));
        assert!(actions[3].1.contains("<DesiredVolume>0</DesiredVolume>"));
        assert!(actions[4].0.contains("SetMute"));
        assert!(actions[4].1.contains("<DesiredMute>1</DesiredMute>"));
        assert!(actions[5].0.contains("#Play"));
        assert!(actions[6].0.contains("GetCurrentTransportActions"));
        let seeks = actions
            .iter()
            .filter(|(action, _)| action.contains("#Seek"))
            .collect::<Vec<_>>();
        assert_eq!(seeks.len(), 1);
        assert!(
            seeks
                .iter()
                .all(|(_, body)| body.contains("<Target>00:00:42</Target>"))
        );
        assert!(actions[13].0.contains("SetVolume"));
        assert!(actions[13].1.contains("<DesiredVolume>40</DesiredVolume>"));
        assert!(actions[14].0.contains("SetMute"));
        assert!(actions[14].1.contains("<DesiredMute>0</DesiredMute>"));
        renderer.join().expect("renderer thread");
        relay.shutdown();
    }

    #[test]
    fn transient_status_failure_does_not_fail_upnp_playback() {
        let server = Server::http("127.0.0.1:0").expect("fake renderer");
        let address = server.server_addr().to_ip().expect("renderer address");
        let renderer = thread::spawn(move || {
            let mut current_uri = String::new();
            for index in 0..7 {
                let mut request = server.recv().expect("renderer request");
                if index == 0 {
                    let description = format!(
                        r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service></serviceList></device></root>"#
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
                }
                if index == 4 {
                    request
                        .respond(Response::from_string("renderer busy").with_status_code(500))
                        .expect("temporary failure response");
                    continue;
                }
                let values = if action.contains("GetTransportInfo") {
                    "<CurrentTransportState>PLAYING</CurrentTransportState>".to_string()
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

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let device = runtime
            .block_on(Device::from_url(
                format!("http://{address}/device.xml")
                    .parse()
                    .expect("device URL"),
            ))
            .expect("device description");
        let directory = tempfile::tempdir().expect("track directory");
        let path = directory.path().join("track.mp3");
        File::create(&path)
            .expect("create track")
            .write_all(b"track")
            .expect("write track");
        let stream = PreparedStream::from(library::ResolvedStream::new(
            Url::from_file_path(path).expect("track URL").to_string(),
        ))
        .with_media(test_track(), Some("audio/mpeg".to_string()));
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
        let stream = PreparedStream::from(library::ResolvedStream::new("file:///track.flac"))
            .with_media(test_track(), Some("audio/flac".to_string()));
        let media = PublishedResource {
            uri: "http://192.0.2.10:4000/media".to_string(),
            content_type: "audio/flac".to_string(),
            content_length: Some(12_345),
            starts_at_millis: 10_000,
            ends_at_millis: Some(70_000),
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

    fn test_track() -> library::Track {
        library::Track::new(library::TrackData {
            id: library::TrackId::new("track-1"),
            album_id: None,
            title: "Track & Title".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            album_artwork: None,
            year: 2026,
            release_date: Some("2026-08-17".to_string()),
            date_added: None,
            last_played: None,
            play_count: None,
            user_rating: Some(8),
            duration_seconds: 300,
            favorite: true,
            disc_number: 1,
            track_number: 2,
            image_ref: None,
            local_artwork: None,
            musicbrainz_recording_id: None,
            musicbrainz_release_track_id: None,
            source_path: None,
            cue: None,
            source_format: Some("flac".to_string()),
            comment: None,
            skip_count: None,
            bpm: None,
            relations: library::TrackRelations::default(),
        })
    }

    fn xml_element(body: &str, name: &str) -> Option<String> {
        let start = format!("<{name}>");
        let end = format!("</{name}>");
        let value = body.split_once(&start)?.1.split_once(&end)?.0;
        Some(value.to_string())
    }
}
