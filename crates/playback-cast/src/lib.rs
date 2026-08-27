mod chromecast;
mod discovery;
mod relay;
mod upnp;
mod upnp_transport;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chromecast::GoogleCastController;
use discovery::{DiscoveredTarget, discover_google_cast, discover_upnp};
use playback::{
    BackendCommand, BackendError, BackendEvent, BackendFailure, PlaybackBackend, RemoteOutput,
    RemoteOutputProtocol,
};
use relay::{ArtworkResolver, RelayServer, available_networks, local_address_for, network_address};
use upnp::UpnpController;
use upnp_transport::UpnpDevice;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const GOOGLE_CAST_STATUS_INTERVAL: Duration = Duration::from_millis(500);
const UPNP_STATUS_INTERVAL: Duration = Duration::from_secs(1);

pub struct CastManager {
    targets: Mutex<HashMap<String, DiscoveredTarget>>,
    proxy_media: Arc<AtomicBool>,
    network_interface: Mutex<Option<String>>,
    artwork_resolver: ArtworkResolver,
}

impl Default for CastManager {
    fn default() -> Self {
        Self::new(false, None, |_| None)
    }
}

impl CastManager {
    pub fn new(
        proxy_media: bool,
        network_interface: Option<String>,
        artwork_resolver: impl Fn(&playback::PreparedStream) -> Option<std::path::PathBuf>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            targets: Mutex::new(HashMap::new()),
            proxy_media: Arc::new(AtomicBool::new(proxy_media)),
            network_interface: Mutex::new(network_interface),
            artwork_resolver: Arc::new(artwork_resolver),
        }
    }

    pub fn set_proxy_media(&self, enabled: bool) {
        self.proxy_media.store(enabled, Ordering::Release);
    }

    pub fn available_networks(&self) -> Result<Vec<playback::CastNetwork>, String> {
        available_networks()
    }

    pub fn set_network_interface(&self, network_interface: Option<String>) {
        *self
            .network_interface
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = network_interface;
    }

    pub fn discover(&self) -> Result<Vec<RemoteOutput>, String> {
        let started = Instant::now();
        let network_interface = self
            .network_interface
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let local_address = network_interface
            .as_deref()
            .map(network_address)
            .transpose()?
            .flatten();
        if network_interface.is_some() && local_address.is_none() {
            tracing::warn!(
                network_interface = ?network_interface,
                "selected casting network is unavailable; using automatic discovery"
            );
        }
        tracing::debug!(
            network_interface = ?network_interface,
            ?local_address,
            "discovering cast outputs"
        );
        let upnp = thread::spawn(move || discover_upnp(DISCOVERY_TIMEOUT, local_address));
        let google_cast = thread::spawn(|| discover_google_cast(DISCOVERY_TIMEOUT));
        let upnp = upnp
            .join()
            .map_err(|_| "UPnP discovery stopped unexpectedly".to_string())?;
        let google_cast = google_cast
            .join()
            .map_err(|_| "Google Cast discovery stopped unexpectedly".to_string())?;
        let mut discovered = Vec::new();
        let mut errors = Vec::new();
        match upnp {
            Ok(targets) => discovered.extend(targets),
            Err(error) => errors.push(error),
        }
        match google_cast {
            Ok(targets) => discovered.extend(targets),
            Err(error) => errors.push(error),
        }
        if discovered.is_empty() && errors.len() == 2 {
            return Err(errors.join("; "));
        }
        for error in errors {
            tracing::debug!(%error, "network output discovery was partially unavailable");
        }
        discovered.sort_by(|left, right| {
            left.output()
                .name
                .to_lowercase()
                .cmp(&right.output().name.to_lowercase())
                .then_with(|| left.output().protocol.cmp(&right.output().protocol))
        });
        let outputs = discovered
            .iter()
            .map(|target| target.output().clone())
            .collect::<Vec<_>>();
        let upnp = outputs
            .iter()
            .filter(|output| output.protocol == RemoteOutputProtocol::Upnp)
            .count();
        let google_cast = outputs.len().saturating_sub(upnp);
        tracing::debug!(
            upnp,
            google_cast,
            elapsed_ms = started.elapsed().as_millis(),
            "completed network output discovery"
        );
        *self
            .targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = discovered
            .into_iter()
            .map(|target| (target.output().id.clone(), target))
            .collect();
        Ok(outputs)
    }

    pub fn connect(&self, output: &RemoteOutput) -> Result<CastPlaybackBackend, String> {
        let target = self
            .targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&output.id)
            .cloned()
            .ok_or_else(|| format!("{} is no longer available", output.name))?;
        let network_interface = self
            .network_interface
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        CastPlaybackBackend::new(
            target,
            Arc::clone(&self.proxy_media),
            network_interface,
            Arc::clone(&self.artwork_resolver),
        )
    }
}

pub struct CastPlaybackBackend {
    commands: SyncSender<WorkerCommand>,
    events: Arc<Mutex<Vec<BackendEvent>>>,
    thread: Option<JoinHandle<()>>,
}

impl CastPlaybackBackend {
    fn new(
        target: DiscoveredTarget,
        proxy_media: Arc<AtomicBool>,
        network_interface: Option<String>,
        artwork_resolver: ArtworkResolver,
    ) -> Result<Self, String> {
        let selected_local_address = network_interface
            .as_deref()
            .map(|network_interface| local_address_for(target.address(), Some(network_interface)))
            .transpose()?;
        tracing::debug!(
            network_interface = ?network_interface,
            local_address = ?selected_local_address,
            renderer_address = %target.address(),
            protocol = ?target.output().protocol,
            "connecting cast output"
        );
        let relay =
            RelayServer::start(target.address(), proxy_media, network_interface.as_deref())?
                .with_artwork_resolver(artwork_resolver);
        let controller = match target {
            DiscoveredTarget::Upnp { device, .. } => {
                let device = if device.local_address() == selected_local_address {
                    *device
                } else {
                    UpnpDevice::from_url(device.url().as_str(), selected_local_address)?
                };
                let mut controller = UpnpController::new(device)?;
                controller.verify_connection()?;
                Controller::Upnp(Box::new(controller))
            }
            DiscoveredTarget::GoogleCast { address, .. } => {
                Controller::GoogleCast(Box::new(GoogleCastController::new(address)?))
            }
        };
        let (commands, receiver) = sync_channel(64);
        let events = Arc::new(Mutex::new(Vec::new()));
        let worker_events = Arc::clone(&events);
        let thread = thread::Builder::new()
            .name("rufin-cast-output".to_string())
            .spawn(move || run_controller(controller, relay, receiver, worker_events))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            commands,
            events,
            thread: Some(thread),
        })
    }
}

impl PlaybackBackend for CastPlaybackBackend {
    fn send(&mut self, command: BackendCommand) -> Result<(), BackendError> {
        self.commands
            .send(WorkerCommand::Backend(Box::new(command)))
            .map_err(|_| BackendError::ChannelClosed)
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        std::mem::take(
            &mut *self
                .events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn shutdown(&mut self) -> Result<(), BackendError> {
        let _ = self.commands.send(WorkerCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| {
                BackendError::Backend("cast output stopped unexpectedly".to_string())
            })?;
        }
        Ok(())
    }
}

impl Drop for CastPlaybackBackend {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

enum WorkerCommand {
    Backend(Box<BackendCommand>),
    Shutdown,
}

enum Controller {
    Upnp(Box<UpnpController>),
    GoogleCast(Box<GoogleCastController>),
}

impl Controller {
    fn poll_interval(&self) -> Duration {
        match self {
            Self::Upnp(_) => UPNP_STATUS_INTERVAL,
            Self::GoogleCast(_) => GOOGLE_CAST_STATUS_INTERVAL,
        }
    }

    fn initial_events(&self) -> Vec<BackendEvent> {
        match self {
            Self::Upnp(controller) => controller.initial_events(),
            Self::GoogleCast(controller) => controller.initial_events(),
        }
    }

    fn handle(
        &mut self,
        command: BackendCommand,
        relay: &RelayServer,
    ) -> Result<Vec<BackendEvent>, String> {
        match self {
            Self::Upnp(controller) => controller.handle(command, relay),
            Self::GoogleCast(controller) => controller.handle(command, relay),
        }
    }

    fn poll(&mut self, relay: &RelayServer) -> Result<Vec<BackendEvent>, String> {
        match self {
            Self::Upnp(controller) => controller.poll(relay),
            Self::GoogleCast(controller) => controller.poll(relay),
        }
    }

    fn shutdown(&mut self) {
        match self {
            Self::Upnp(controller) => controller.shutdown(),
            Self::GoogleCast(controller) => controller.shutdown(),
        }
    }
}

fn run_controller(
    mut controller: Controller,
    mut relay: RelayServer,
    commands: Receiver<WorkerCommand>,
    events: Arc<Mutex<Vec<BackendEvent>>>,
) {
    publish(&events, controller.initial_events());
    let mut active_run = None;
    let mut last_poll = Instant::now();
    loop {
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(WorkerCommand::Backend(command)) => {
                let run = command.run();
                if matches!(*command, BackendCommand::Start { .. }) {
                    active_run = run;
                } else if matches!(*command, BackendCommand::Stop { .. }) {
                    active_run = None;
                }
                match controller.handle(*command, &relay) {
                    Ok(update) => publish(&events, update),
                    Err(error) => publish_error(&events, active_run.or(run), error),
                }
            }
            Ok(WorkerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if last_poll.elapsed() >= controller.poll_interval() {
            match controller.poll(&relay) {
                Ok(update) => publish(&events, update),
                Err(error) => publish_error(&events, active_run, error),
            }
            last_poll = Instant::now();
        }
    }
    controller.shutdown();
    relay.shutdown();
}

fn publish(events: &Mutex<Vec<BackendEvent>>, mut update: Vec<BackendEvent>) {
    events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .append(&mut update);
}

fn publish_error(events: &Mutex<Vec<BackendEvent>>, run: Option<playback::RunId>, error: String) {
    if let Some(run) = run {
        publish(
            events,
            vec![BackendEvent::Error {
                run,
                error: BackendFailure::new(error),
            }],
        );
    } else {
        tracing::warn!(%error, "network output command failed");
    }
}
