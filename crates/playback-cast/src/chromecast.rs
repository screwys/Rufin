use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use fcast_sender_sdk::context::CastContext;
use fcast_sender_sdk::device::{
    CastingDevice, DeviceConnectionState, DeviceEventHandler, DeviceInfo, LoadRequest, MediaTrack,
    MediaTrackType, Metadata, PlaybackState as FutoPlaybackState, QueueState, ReceiverError,
    Source, TrackList,
};
use playback::{BackendCommand, BackendEvent, BackendState, PreparedStream, RunId};

use crate::relay::{PublishedResource, RelayServer};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const STATUS_INTERVAL_MILLIS: u64 = 500;

struct CastMedia {
    run: RunId,
    published: PublishedResource,
}

enum SdkEvent {
    Connected,
    Disconnected,
    Volume(f64),
    Position(f64),
    Duration(f64),
    State(FutoPlaybackState),
    Stopped,
    Error(String),
}

struct FutoEventHandler {
    events: Sender<SdkEvent>,
}

impl FutoEventHandler {
    fn publish(&self, event: SdkEvent) {
        let _ = self.events.send(event);
    }
}

impl DeviceEventHandler for FutoEventHandler {
    fn connection_state_changed(&self, state: DeviceConnectionState) {
        match state {
            DeviceConnectionState::Connected { .. } => self.publish(SdkEvent::Connected),
            DeviceConnectionState::Disconnected => self.publish(SdkEvent::Disconnected),
            DeviceConnectionState::Connecting | DeviceConnectionState::Reconnecting => {}
        }
    }

    fn volume_changed(&self, volume: f64) {
        self.publish(SdkEvent::Volume(volume));
    }

    fn time_changed(&self, time: f64) {
        self.publish(SdkEvent::Position(time));
    }

    fn playback_state_changed(&self, state: FutoPlaybackState) {
        self.publish(SdkEvent::State(state));
    }

    fn duration_changed(&self, duration: f64) {
        self.publish(SdkEvent::Duration(duration));
    }

    fn speed_changed(&self, _speed: f64) {}

    fn source_changed(&self, _source: Source) {}

    fn playback_stopped(&self) {
        self.publish(SdkEvent::Stopped);
    }

    fn playback_error(&self, message: String) {
        self.publish(SdkEvent::Error(message));
    }

    fn tracks_available(&self, _tracks: Vec<MediaTrack>) {}

    fn track_selected(&self, _id: Option<u32>, _typ: MediaTrackType) {}

    fn tracks_changed(&self, _tracks: TrackList) {}

    fn queue_changed(&self, _queue: QueueState) {}

    fn command_error(&self, error: ReceiverError) {
        self.publish(SdkEvent::Error(format!(
            "Google Cast receiver rejected a command: {error:?}"
        )));
    }
}

#[derive(Debug, Eq, PartialEq)]
enum CastStateOutcome {
    Ignore,
    State(BackendState),
    Ended,
}

pub(crate) struct GoogleCastController {
    _context: CastContext,
    device: Arc<dyn CastingDevice>,
    events: Receiver<SdkEvent>,
    pending_events: VecDeque<SdkEvent>,
    current: Option<CastMedia>,
    playback_observed: bool,
    output_volume: f64,
    muted: bool,
}

impl GoogleCastController {
    pub(crate) fn new(address: SocketAddr) -> Result<Self, String> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let context = CastContext::new().map_err(|error| error.to_string())?;
        let info = DeviceInfo::chromecast(
            address.to_string(),
            vec![address.into()],
            address.port(),
            HashMap::new(),
        );
        let device = context.create_device_from_info(info);
        let (event_sender, events) = channel();
        device
            .connect(
                None,
                Arc::new(FutoEventHandler {
                    events: event_sender,
                }),
                0,
            )
            .map_err(|error| error.to_string())?;

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut pending_events = VecDeque::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Google Cast receiver connection timed out".to_string());
            }
            match events.recv_timeout(remaining) {
                Ok(SdkEvent::Connected) => break,
                Ok(SdkEvent::Disconnected) => {
                    return Err("Google Cast receiver disconnected while connecting".to_string());
                }
                Ok(SdkEvent::Error(error)) => return Err(error),
                Ok(event) => pending_events.push_back(event),
                Err(_) => return Err("Google Cast receiver connection timed out".to_string()),
            }
        }

        Ok(Self {
            _context: context,
            device,
            events,
            pending_events,
            current: None,
            playback_observed: false,
            output_volume: 1.0,
            muted: false,
        })
    }

    pub(crate) fn initial_events(&self) -> Vec<BackendEvent> {
        Vec::new()
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
                start_position_millis,
                ..
            } => self.start(run, &current, start_position_millis, relay),
            BackendCommand::Play { run } if self.current_run() == Some(run) => {
                self.device.resume_playback().map_err(string_error)?;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Playing,
                }])
            }
            BackendCommand::Pause { run } if self.current_run() == Some(run) => {
                self.device.pause_playback().map_err(string_error)?;
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Paused,
                }])
            }
            BackendCommand::Stop { run } if self.current_run() == Some(run) => {
                self.device.stop_playback().map_err(string_error)?;
                self.clear_session();
                relay.clear();
                Ok(vec![BackendEvent::State {
                    run,
                    state: BackendState::Stopped,
                }])
            }
            BackendCommand::Seek {
                run,
                position_millis,
            } if self.current_run() == Some(run)
                && self
                    .current
                    .as_ref()
                    .is_some_and(|current| current.published.seekable) =>
            {
                self.device
                    .seek(position_millis as f64 / 1_000.0)
                    .map_err(string_error)?;
                Ok(vec![BackendEvent::Position {
                    run,
                    millis: position_millis,
                }])
            }
            BackendCommand::SetOutputVolume { volume, muted, .. } => {
                self.output_volume = volume.clamp(0.0, 1.0);
                self.muted = muted;
                self.device
                    .change_volume(if muted { 0.0 } else { self.output_volume })
                    .map_err(string_error)?;
                Ok(vec![BackendEvent::AudioApplied {
                    volume: self.output_volume,
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
        let mut events = Vec::new();
        while let Some(event) = self.next_sdk_event() {
            self.accept_sdk_event(event, relay, &mut events)?;
        }
        Ok(events)
    }

    pub(crate) fn shutdown(&mut self) {
        let _ = self.device.disconnect();
        self.clear_session();
    }

    fn start(
        &mut self,
        run: RunId,
        stream: &PreparedStream,
        position_millis: u64,
        relay: &RelayServer,
    ) -> Result<Vec<BackendEvent>, String> {
        self.clear_session();
        relay.clear();
        let published = relay.publish(stream)?;
        let duration_millis = stream_duration_millis(stream);
        let title = stream
            .track
            .as_ref()
            .map(|track| track.title.trim())
            .filter(|title| !title.is_empty())
            .map(str::to_string);
        let metadata = (title.is_some() || published.artwork_uri.is_some()).then(|| Metadata {
            title,
            thumbnail_url: published.artwork_uri.clone(),
        });
        let request = LoadRequest::Url {
            content_type: published.content_type.clone(),
            url: published.uri.clone(),
            resume_position: published
                .seekable
                .then_some(position_millis as f64 / 1_000.0),
            speed: None,
            volume: None,
            metadata,
            request_headers: None,
        };
        self.current = Some(CastMedia {
            run,
            published: published.clone(),
        });
        self.playback_observed = false;
        if let Err(error) = self.device.load(request, Some(STATUS_INTERVAL_MILLIS)) {
            self.clear_session();
            relay.remove(&published);
            return Err(error.to_string());
        }

        let mut events = vec![
            BackendEvent::Started { run },
            BackendEvent::Seekable {
                run,
                seekable: published.seekable,
            },
            BackendEvent::State {
                run,
                state: BackendState::Buffering,
            },
        ];
        if let Some(millis) = duration_millis {
            events.push(BackendEvent::Duration { run, millis });
        }
        Ok(events)
    }

    fn next_sdk_event(&mut self) -> Option<SdkEvent> {
        self.pending_events
            .pop_front()
            .or_else(|| match self.events.try_recv() {
                Ok(event) => Some(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
            })
    }

    fn accept_sdk_event(
        &mut self,
        event: SdkEvent,
        relay: &RelayServer,
        update: &mut Vec<BackendEvent>,
    ) -> Result<(), String> {
        match event {
            SdkEvent::Connected => {}
            SdkEvent::Disconnected => {
                if self.current.is_some() {
                    self.clear_session();
                    relay.clear();
                    return Err("Google Cast receiver disconnected".to_string());
                }
            }
            SdkEvent::Volume(volume) => {
                if self.muted && volume <= f64::EPSILON {
                    update.push(BackendEvent::AudioApplied {
                        volume: self.output_volume,
                        muted: true,
                        output: None,
                    });
                } else {
                    self.output_volume = volume.clamp(0.0, 1.0);
                    self.muted = false;
                    update.push(BackendEvent::AudioApplied {
                        volume: self.output_volume,
                        muted: false,
                        output: None,
                    });
                }
            }
            SdkEvent::Position(seconds) => {
                let Some(run) = self.current_run() else {
                    return Ok(());
                };
                let renderer_millis = (seconds.max(0.0) * 1_000.0).round() as u64;
                if self
                    .current
                    .as_ref()
                    .and_then(|current| current.published.resource_duration_millis)
                    .is_some_and(|duration| renderer_millis >= duration)
                {
                    let _ = self.device.stop_playback();
                    self.finish_current(relay);
                    update.push(BackendEvent::Ended { run });
                } else {
                    let logical_position = self
                        .current
                        .as_ref()
                        .map(|current| current.published.logical_position_millis(renderer_millis))
                        .unwrap_or(renderer_millis);
                    update.push(BackendEvent::Position {
                        run,
                        millis: logical_position,
                    });
                }
            }
            SdkEvent::Duration(seconds) => {
                if let Some(run) = self.current_run()
                    && seconds.is_finite()
                    && seconds > 0.0
                {
                    update.push(BackendEvent::Duration {
                        run,
                        millis: (seconds * 1_000.0).round() as u64,
                    });
                }
            }
            SdkEvent::State(state) => {
                let Some(run) = self.current_run() else {
                    return Ok(());
                };
                match cast_state_outcome(state, self.playback_observed)? {
                    CastStateOutcome::Ignore => {}
                    CastStateOutcome::State(state) => {
                        if matches!(state, BackendState::Playing | BackendState::Paused) {
                            self.playback_observed = true;
                        }
                        update.push(BackendEvent::State { run, state });
                    }
                    CastStateOutcome::Ended => {
                        self.finish_current(relay);
                        update.push(BackendEvent::Ended { run });
                    }
                }
            }
            SdkEvent::Stopped => {
                if let Some(run) = self.current_run() {
                    self.finish_current(relay);
                    update.push(BackendEvent::State {
                        run,
                        state: BackendState::Stopped,
                    });
                }
            }
            SdkEvent::Error(error) => {
                if self.current.is_some() {
                    self.finish_current(relay);
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn finish_current(&mut self, relay: &RelayServer) {
        if let Some(current) = self.current.take() {
            relay.remove(&current.published);
        }
        self.playback_observed = false;
    }

    fn clear_session(&mut self) {
        self.current = None;
        self.playback_observed = false;
    }

    fn current_run(&self) -> Option<RunId> {
        self.current.as_ref().map(|current| current.run)
    }
}

fn stream_duration_millis(stream: &PreparedStream) -> Option<u64> {
    stream
        .end_millis()
        .map(|end| end.saturating_sub(stream.start_millis()))
        .or_else(|| {
            stream
                .track
                .as_ref()
                .and_then(|track| u64::try_from(track.duration_millis).ok())
        })
}

fn cast_state_outcome(
    state: FutoPlaybackState,
    playback_observed: bool,
) -> Result<CastStateOutcome, String> {
    match state {
        FutoPlaybackState::Idle => Ok(CastStateOutcome::Ignore),
        FutoPlaybackState::Buffering => Ok(CastStateOutcome::State(BackendState::Buffering)),
        FutoPlaybackState::Playing => Ok(CastStateOutcome::State(BackendState::Playing)),
        FutoPlaybackState::Paused => Ok(CastStateOutcome::State(BackendState::Paused)),
        FutoPlaybackState::Ended if playback_observed => Ok(CastStateOutcome::Ended),
        FutoPlaybackState::Ended => {
            Err("Google Cast receiver finished before playback started".to_string())
        }
    }
}

fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_observed_finished_playback_advances_the_queue() {
        assert_eq!(
            cast_state_outcome(FutoPlaybackState::Idle, false),
            Ok(CastStateOutcome::Ignore)
        );
        assert!(cast_state_outcome(FutoPlaybackState::Ended, false).is_err());
        assert_eq!(
            cast_state_outcome(FutoPlaybackState::Ended, true),
            Ok(CastStateOutcome::Ended)
        );
    }
}
