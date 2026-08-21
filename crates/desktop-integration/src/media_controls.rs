#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
mod freedesktop {
    use std::cell::{Cell, RefCell};
    use std::fmt::Write as _;
    use std::rc::Rc;

    use app_identity::APP_ID;
    use glib;
    use mpris_server::{
        LoopStatus, Metadata, PlaybackStatus, Player as MprisPlayer, Time, TrackId as MprisTrackId,
    };
    use playback::{
        PlaybackView, PositionDiscontinuity, RepeatMode, RunId, TransportHandle, TransportStatus,
    };
    use tracing::warn;

    #[derive(Clone, Debug, PartialEq)]
    struct MprisDesiredState {
        run: Option<RunId>,
        track_id: MprisTrackId,
        playback_status: PlaybackStatus,
        loop_status: LoopStatus,
        shuffle: bool,
        metadata: Metadata,
        volume: f64,
        can_play: bool,
        can_pause: bool,
        can_seek: bool,
        can_go_next: bool,
        can_go_previous: bool,
        position: Option<Time>,
    }

    impl MprisDesiredState {
        fn replace_position(&mut self, position: Option<Time>) -> bool {
            if self.position == position {
                return false;
            }
            self.position = position;
            true
        }
    }

    pub struct MediaControls {
        transport: TransportHandle,
        player: RefCell<Option<Rc<MprisPlayer>>>,
        desired: RefCell<Option<MprisDesiredState>>,
        applied: RefCell<Option<MprisDesiredState>>,
        pending_seeked: Cell<Option<PositionDiscontinuity>>,
        generation: Cell<u64>,
        running: Cell<bool>,
    }

    impl MediaControls {
        fn new(transport: TransportHandle) -> Self {
            Self {
                transport,
                player: RefCell::new(None),
                desired: RefCell::new(None),
                applied: RefCell::new(None),
                pending_seeked: Cell::new(None),
                generation: Cell::new(0),
                running: Cell::new(false),
            }
        }

        fn install_player(self: &Rc<Self>, player: Rc<MprisPlayer>) {
            *self.player.borrow_mut() = Some(player);
            self.applied.borrow_mut().take();
            self.pending_seeked.set(None);
            self.start_drain();
        }

        fn queue(
            self: &Rc<Self>,
            desired: MprisDesiredState,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            if self
                .pending_seeked
                .get()
                .is_some_and(|pending| Some(pending.run) != desired.run)
            {
                self.pending_seeked.set(None);
            }
            if let Some(discontinuity) = discontinuity
                && desired.run == Some(discontinuity.run)
            {
                self.pending_seeked.set(Some(discontinuity));
            }
            *self.desired.borrow_mut() = Some(desired);
            self.generation.set(self.generation.get().saturating_add(1));
            self.start_drain();
        }

        fn start_drain(self: &Rc<Self>) {
            if self.player.borrow().is_none() || self.running.replace(true) {
                return;
            }
            let adapter = Rc::clone(self);
            glib::spawn_future_local(async move {
                adapter.drain().await;
                adapter.running.set(false);
            });
        }

        async fn drain(&self) {
            loop {
                let Some(player) = self.player.borrow().as_ref().cloned() else {
                    return;
                };
                let Some(desired) = self.desired.borrow().clone() else {
                    return;
                };
                let applied = self.applied.borrow().clone();
                let generation = self.generation.get();
                let discontinuity = self.pending_seeked.take();

                apply_mpris_desired(&player, applied.as_ref(), &desired).await;
                let mut applied = desired.clone();
                if let Some(discontinuity) = discontinuity
                    && self
                        .desired
                        .borrow()
                        .as_ref()
                        .is_some_and(|current| current.run == Some(discontinuity.run))
                {
                    let position = mpris_time(discontinuity.position_millis);
                    player.set_position(position);
                    let _sent = player.seeked(position).await;
                    applied.position = Some(position);
                }
                *self.applied.borrow_mut() = Some(applied);

                if self.generation.get() == generation && self.pending_seeked.get().is_none() {
                    return;
                }
            }
        }
    }

    async fn apply_mpris_desired(
        player: &MprisPlayer,
        applied: Option<&MprisDesiredState>,
        desired: &MprisDesiredState,
    ) {
        if applied.is_none_or(|applied| applied.playback_status != desired.playback_status) {
            let _updated = player.set_playback_status(desired.playback_status).await;
        }
        if applied.is_none_or(|applied| applied.loop_status != desired.loop_status) {
            let _updated = player.set_loop_status(desired.loop_status).await;
        }
        if applied.is_none_or(|applied| applied.shuffle != desired.shuffle) {
            let _updated = player.set_shuffle(desired.shuffle).await;
        }
        if applied.is_none_or(|applied| applied.metadata != desired.metadata) {
            let _updated = player.set_metadata(desired.metadata.clone()).await;
        }
        if applied.is_none_or(|applied| applied.volume != desired.volume) {
            let _updated = player.set_volume(desired.volume).await;
        }
        if applied.is_none_or(|applied| applied.can_play != desired.can_play) {
            let _updated = player.set_can_play(desired.can_play).await;
        }
        if applied.is_none_or(|applied| applied.can_pause != desired.can_pause) {
            let _updated = player.set_can_pause(desired.can_pause).await;
        }
        if applied.is_none_or(|applied| applied.can_seek != desired.can_seek) {
            let _updated = player.set_can_seek(desired.can_seek).await;
        }
        if applied.is_none_or(|applied| applied.can_go_next != desired.can_go_next) {
            let _updated = player.set_can_go_next(desired.can_go_next).await;
        }
        if applied.is_none_or(|applied| applied.can_go_previous != desired.can_go_previous) {
            let _updated = player.set_can_go_previous(desired.can_go_previous).await;
        }
        if applied.is_none_or(|applied| applied.position != desired.position)
            && let Some(position) = desired.position
        {
            player.set_position(position);
        }
    }

    impl MediaControls {
        pub fn start(transport: TransportHandle) -> Rc<Self> {
            let mpris = Rc::new(Self::new(transport));
            let setup = Rc::clone(&mpris);
            glib::spawn_future_local(async move {
                setup.install().await;
            });
            mpris
        }

        pub fn observe(
            self: &Rc<Self>,
            playback: Option<&PlaybackView>,
            art_url: Option<String>,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            self.queue(mpris_desired_state(playback, art_url), discontinuity);
        }

        pub fn observe_position(
            self: &Rc<Self>,
            position_millis: Option<u64>,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            let position = position_millis.map(mpris_time);
            let (position_changed, desired_run) = {
                let mut desired = self.desired.borrow_mut();
                let Some(desired) = desired.as_mut() else {
                    return;
                };
                (desired.replace_position(position), desired.run)
            };

            if self
                .pending_seeked
                .get()
                .is_some_and(|pending| Some(pending.run) != desired_run)
            {
                self.pending_seeked.set(None);
            }
            let matching_discontinuity =
                discontinuity.filter(|discontinuity| desired_run == Some(discontinuity.run));
            if let Some(discontinuity) = matching_discontinuity {
                self.pending_seeked.set(Some(discontinuity));
            }

            if !position_changed && matching_discontinuity.is_none() {
                return;
            }

            if self.running.get() || self.pending_seeked.get().is_some() {
                self.generation.set(self.generation.get().saturating_add(1));
                self.start_drain();
                return;
            }

            let Some(player) = self.player.borrow().as_ref().cloned() else {
                return;
            };
            if let Some(position) = position {
                player.set_position(position);
            }
            if let Some(applied) = self.applied.borrow_mut().as_mut() {
                applied.position = position;
            }
        }

        async fn install(self: Rc<Self>) {
            let player = match MprisPlayer::builder(APP_ID)
                .identity("Rufin")
                .desktop_entry(APP_ID)
                .supported_uri_schemes(["http", "https", "file"])
                .supported_mime_types(["audio/mpeg", "audio/flac", "audio/ogg", "audio/x-wav"])
                .can_play(true)
                .can_pause(true)
                .can_go_next(true)
                .can_go_previous(true)
                .can_seek(true)
                .can_control(true)
                .build()
                .await
            {
                Ok(player) => Rc::new(player),
                Err(error) => {
                    warn!(%error, "failed to start MPRIS server");
                    return;
                }
            };

            let transport = self.transport.clone();
            player.connect_play_pause(move |_| transport.play_pause());
            let transport = self.transport.clone();
            player.connect_play(move |_| transport.play());
            let transport = self.transport.clone();
            player.connect_pause(move |_| transport.pause());
            let transport = self.transport.clone();
            player.connect_stop(move |_| transport.stop());
            let transport = self.transport.clone();
            player.connect_next(move |_| transport.next());
            let transport = self.transport.clone();
            player.connect_previous(move |_| transport.previous());
            let transport = self.transport.clone();
            let seek_mpris = Rc::clone(&self);
            player.connect_seek(move |_, offset| {
                let current = seek_mpris
                    .desired
                    .borrow()
                    .as_ref()
                    .and_then(|desired| desired.position)
                    .map_or(0, |position| (position.as_micros() / 1_000).max(0) as u64);
                let offset_millis = offset.as_micros() / 1_000;
                let target = if offset_millis.is_negative() {
                    current.saturating_sub(offset_millis.unsigned_abs())
                } else {
                    current.saturating_add(offset_millis as u64)
                };
                transport.seek_millis(target);
            });
            let position_mpris = Rc::clone(&self);
            let transport = self.transport.clone();
            player.connect_set_position(move |_, track_id, position| {
                let current_matches = position_mpris
                    .desired
                    .borrow()
                    .as_ref()
                    .is_some_and(|desired| &desired.track_id == track_id);
                if current_matches {
                    transport.seek_millis((position.as_micros() / 1_000).max(0) as u64);
                }
            });
            let transport = self.transport.clone();
            player.connect_set_volume(move |_, volume| {
                let volume = if volume.is_finite() {
                    volume.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                transport.set_volume(volume);
                transport.persist_volume(volume);
            });
            let transport = self.transport.clone();
            player.connect_set_shuffle(move |_, enabled| transport.set_shuffle(enabled));
            let transport = self.transport.clone();
            player.connect_set_loop_status(move |_, status| {
                transport.set_repeat(repeat_mode_from_mpris(status));
            });

            self.install_player(Rc::clone(&player));
            glib::spawn_future_local(async move {
                player.run().await;
            });
        }
    }

    fn mpris_desired_state(
        playback: Option<&PlaybackView>,
        art_url: Option<String>,
    ) -> MprisDesiredState {
        let has_current = playback.is_some_and(|playback| playback.transport.current.is_some());
        let has_active_run = playback.is_some_and(|playback| {
            playback
                .transport
                .current
                .as_ref()
                .is_some_and(|media| media.id.run.is_some())
                && !matches!(
                    playback.transport.state,
                    TransportStatus::Stopped | TransportStatus::Failed
                )
        });
        let can_go_next = playback.is_some_and(|playback| {
            has_current
                && (playback.queue.next_occurrence.is_some() || playback.controls.auto_dj_enabled)
        });
        MprisDesiredState {
            run: playback
                .and_then(|playback| playback.transport.current.as_ref())
                .and_then(|media| media.id.run),
            track_id: playback
                .and_then(|playback| playback.transport.current.as_ref())
                .map_or(MprisTrackId::NO_TRACK, |media| {
                    mpris_track_id(media.id.occurrence.as_str())
                }),
            playback_status: playback.map_or(PlaybackStatus::Stopped, |playback| {
                mpris_playback_status(playback.transport.effective_state())
            }),
            loop_status: mpris_loop_status(
                playback.map_or(RepeatMode::Off, |playback| playback.controls.repeat_mode),
            ),
            shuffle: playback.is_some_and(|playback| playback.controls.shuffle_enabled),
            metadata: mpris_metadata(playback, art_url),
            volume: playback.map_or(1.0, |playback| playback.controls.volume.clamp(0.0, 1.0)),
            can_play: has_current,
            can_pause: has_active_run,
            can_seek: has_current && playback.is_some_and(|playback| playback.transport.can_seek),
            can_go_next,
            can_go_previous: has_current,
            position: playback
                .filter(|playback| playback.transport.current.is_some())
                .map(|playback| mpris_time(playback.transport.position_millis)),
        }
    }

    fn mpris_metadata(playback: Option<&PlaybackView>, art_url: Option<String>) -> Metadata {
        let Some(entry) = playback.and_then(|playback| playback.transport.current.as_ref()) else {
            return Metadata::builder().trackid(MprisTrackId::NO_TRACK).build();
        };
        let mut builder = Metadata::builder()
            .trackid(mpris_track_id(entry.id.occurrence.as_str()))
            .title(entry.track.title.clone())
            .artist([entry.track.artist.clone()])
            .album(entry.track.album.clone())
            .length(Time::from_secs(i64::from(entry.track.duration_seconds)));
        if let Some(art_url) = art_url {
            builder = builder.art_url(art_url);
        }
        builder.build()
    }

    fn mpris_track_id(occurrence: &str) -> MprisTrackId {
        let mut encoded = String::with_capacity(occurrence.len() * 2);
        for byte in occurrence.as_bytes() {
            let _written = write!(&mut encoded, "{byte:02x}");
        }
        MprisTrackId::try_from(format!("/io/github/screwys/Rufin/track/{encoded}"))
            .unwrap_or(MprisTrackId::NO_TRACK)
    }

    #[cfg(test)]
    fn mpris_set_position_matches(
        playback: Option<&PlaybackView>,
        track_id: &MprisTrackId,
    ) -> bool {
        playback
            .and_then(|playback| playback.transport.current.as_ref())
            .is_some_and(|entry| &mpris_track_id(entry.id.occurrence.as_str()) == track_id)
    }

    fn mpris_time(position_millis: u64) -> Time {
        Time::from_millis(position_millis.min(i64::MAX as u64) as i64)
    }

    fn mpris_loop_status(repeat_mode: RepeatMode) -> LoopStatus {
        match repeat_mode {
            RepeatMode::Off => LoopStatus::None,
            RepeatMode::One => LoopStatus::Track,
            RepeatMode::All => LoopStatus::Playlist,
        }
    }

    fn repeat_mode_from_mpris(status: LoopStatus) -> RepeatMode {
        match status {
            LoopStatus::None => RepeatMode::Off,
            LoopStatus::Track => RepeatMode::One,
            LoopStatus::Playlist => RepeatMode::All,
        }
    }

    fn mpris_playback_status(state: TransportStatus) -> PlaybackStatus {
        match state {
            TransportStatus::Resolving | TransportStatus::Buffering | TransportStatus::Playing => {
                PlaybackStatus::Playing
            }
            TransportStatus::Paused => PlaybackStatus::Paused,
            TransportStatus::Stopped | TransportStatus::Failed => PlaybackStatus::Stopped,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            RepeatMode, TransportStatus, mpris_desired_state, mpris_loop_status, mpris_metadata,
            mpris_set_position_matches, mpris_time, mpris_track_id, repeat_mode_from_mpris,
        };
        use crate::discord::tests::test_view;
        use playback::OccurrenceId;
        use std::sync::Arc;

        #[test]
        fn mpris_occurrence_paths_distinguish_duplicate_tracks_and_guard_set_position() {
            let first = OccurrenceId::new("occurrence:first");
            let second = OccurrenceId::new("occurrence:second");
            let first_id = mpris_track_id(first.as_str());
            let second_id = mpris_track_id(second.as_str());
            assert_ne!(first_id, second_id);

            let mut playback = test_view(1, "Album", TransportStatus::Playing, 1_000);
            Arc::make_mut(playback.transport.current.as_mut().expect("current media"))
                .id
                .occurrence = first;
            assert!(mpris_set_position_matches(Some(&playback), &first_id));
            assert!(!mpris_set_position_matches(Some(&playback), &second_id));
        }

        #[test]
        fn mpris_exact_repeat_mapping_round_trips() {
            for repeat in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
                assert_eq!(repeat_mode_from_mpris(mpris_loop_status(repeat)), repeat);
            }
        }

        #[test]
        fn mpris_capabilities_follow_queue_summary() {
            let mut playback = test_view(1, "Album", TransportStatus::Playing, 0);
            let exhausted = mpris_desired_state(Some(&playback), None);
            assert!(!exhausted.can_go_next);
            assert!(exhausted.can_go_previous);

            playback.queue.next_occurrence = Some(OccurrenceId::new("occurrence:next"));
            let with_next = mpris_desired_state(Some(&playback), None);
            assert!(with_next.can_go_next);
        }

        #[test]
        fn mpris_metadata_updates_when_cached_art_arrives() {
            let playback = test_view(1, "Album", TransportStatus::Paused, 0);
            assert_ne!(
                mpris_metadata(Some(&playback), None),
                mpris_metadata(Some(&playback), Some("file:///tmp/cover.png".to_string()))
            );
        }

        #[test]
        fn mpris_position_update_preserves_static_desired_state() {
            let playback = test_view(1, "Album", TransportStatus::Playing, 1_000);
            let mut desired =
                mpris_desired_state(Some(&playback), Some("file:///cover.png".to_string()));
            let expected = super::MprisDesiredState {
                position: Some(mpris_time(1_500)),
                ..desired.clone()
            };

            assert!(desired.replace_position(Some(mpris_time(1_500))));
            assert_eq!(desired, expected);
            assert!(!desired.replace_position(Some(mpris_time(1_500))));
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::rc::Rc;
    use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use playback::{
        PlaybackView, PositionDiscontinuity, RepeatMode, TransportHandle, TransportStatus,
    };
    use tracing::warn;
    use windows::Foundation::{TimeSpan, TypedEventHandler, Uri};
    use windows::Media::Playback::MediaPlayer;
    use windows::Media::{
        AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode, MediaPlaybackStatus,
        MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
        ShuffleEnabledChangeRequestedEventArgs, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    };
    use windows::Storage::Streams::RandomAccessStreamReference;
    use windows::System::{DispatcherQueue, DispatcherQueueController, DispatcherQueueHandler};
    use windows::core::HSTRING;

    pub struct MediaControls {
        manager: Option<WindowsMediaManager>,
    }

    impl MediaControls {
        pub fn start(transport: TransportHandle) -> Rc<Self> {
            let manager = match WindowsMediaManager::new() {
                Ok(manager) => Some(manager),
                Err(error) => {
                    warn!(%error, "failed to start Windows media controls");
                    None
                }
            };
            if let Some(events) = manager
                .as_ref()
                .and_then(WindowsMediaManager::take_event_rx)
            {
                glib::timeout_add_local(Duration::from_millis(100), move || {
                    loop {
                        match events.try_recv() {
                            Ok(event) => apply_media_event(&transport, event),
                            Err(TryRecvError::Empty) => return glib::ControlFlow::Continue,
                            Err(TryRecvError::Disconnected) => return glib::ControlFlow::Break,
                        }
                    }
                });
            }
            Rc::new(Self { manager })
        }

        pub fn observe(
            self: &Rc<Self>,
            playback: Option<&PlaybackView>,
            art_url: Option<String>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
            let Some(manager) = &self.manager else {
                return;
            };
            manager.update(windows_media_state(playback, art_url));
        }

        pub fn observe_position(
            self: &Rc<Self>,
            position_millis: Option<u64>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
            let Some(manager) = &self.manager else {
                return;
            };
            if let Some(position_millis) = position_millis {
                manager.set_position(position_millis);
            } else {
                manager.update(None);
            }
        }
    }

    fn apply_media_event(transport: &TransportHandle, event: WindowsMediaEvent) {
        match event {
            WindowsMediaEvent::Play => transport.play(),
            WindowsMediaEvent::Pause => transport.pause(),
            WindowsMediaEvent::Next => transport.next(),
            WindowsMediaEvent::Previous => transport.previous(),
            WindowsMediaEvent::Stop => transport.stop(),
            WindowsMediaEvent::SetPosition(position_millis) => {
                transport.seek_millis(position_millis);
            }
            WindowsMediaEvent::SetRepeat(repeat) => transport.set_repeat(repeat),
            WindowsMediaEvent::SetShuffle(enabled) => transport.set_shuffle(enabled),
        }
    }

    #[derive(Clone, Copy)]
    enum WindowsPlaybackState {
        Changing,
        Playing,
        Paused,
        Stopped,
    }

    struct WindowsMediaState {
        title: String,
        artist: String,
        album: String,
        duration_millis: u64,
        position_millis: u64,
        art_url: Option<String>,
        playback: WindowsPlaybackState,
        repeat: RepeatMode,
        shuffle: bool,
        can_play: bool,
        can_pause: bool,
        can_stop: bool,
        can_next: bool,
        can_previous: bool,
    }

    fn windows_media_state(
        playback: Option<&PlaybackView>,
        art_url: Option<String>,
    ) -> Option<WindowsMediaState> {
        let playback = playback?;
        let media = playback.transport.current.as_deref()?;
        let has_active_run = media.id.run.is_some()
            && !matches!(
                playback.transport.state,
                TransportStatus::Stopped | TransportStatus::Failed
            );
        let can_next =
            playback.queue.next_occurrence.is_some() || playback.controls.auto_dj_enabled;
        Some(WindowsMediaState {
            title: media.track.title.clone(),
            artist: media.track.artist.clone(),
            album: media.track.album.clone(),
            duration_millis: playback.transport.duration_millis,
            position_millis: playback.transport.position_millis,
            art_url,
            playback: match playback.transport.effective_state() {
                TransportStatus::Resolving | TransportStatus::Buffering => {
                    WindowsPlaybackState::Changing
                }
                TransportStatus::Playing => WindowsPlaybackState::Playing,
                TransportStatus::Paused => WindowsPlaybackState::Paused,
                TransportStatus::Stopped | TransportStatus::Failed => WindowsPlaybackState::Stopped,
            },
            repeat: playback.controls.repeat_mode,
            shuffle: playback.controls.shuffle_enabled,
            can_play: true,
            can_pause: has_active_run,
            can_stop: has_active_run,
            can_next,
            can_previous: true,
        })
    }

    enum WindowsMediaEvent {
        Play,
        Pause,
        Next,
        Previous,
        Stop,
        SetPosition(u64),
        SetRepeat(RepeatMode),
        SetShuffle(bool),
    }

    enum WindowsMediaCommand {
        Update(Option<WindowsMediaState>),
        SetPosition(u64),
    }

    struct WindowsMediaManager {
        controller: DispatcherQueueController,
        queue: DispatcherQueue,
        context: Arc<Mutex<Option<WindowsMediaContext>>>,
        event_rx: Mutex<Option<Receiver<WindowsMediaEvent>>>,
    }

    struct WindowsMediaContext {
        _player: MediaPlayer,
        controls: SystemMediaTransportControls,
        duration_millis: u64,
    }

    impl WindowsMediaManager {
        fn new() -> Result<Self, String> {
            let (event_tx, event_rx) = mpsc::channel();
            let controller = DispatcherQueueController::CreateOnDedicatedThread()
                .map_err(|error| format!("failed to create the Windows media thread: {error}"))?;
            let queue = controller
                .DispatcherQueue()
                .map_err(|error| format!("failed to access the Windows media thread: {error}"))?;
            let context = Arc::new(Mutex::new(None));
            let context_for_startup = Arc::clone(&context);
            let (startup_tx, startup_rx) = mpsc::channel();
            let queued = queue
                .TryEnqueue(&DispatcherQueueHandler::new(move || {
                    let result = create_windows_context(event_tx.clone()).and_then(|created| {
                        let mut context = context_for_startup.lock().map_err(|_| {
                            "failed to lock Windows media controls during startup".to_string()
                        })?;
                        *context = Some(created);
                        Ok(())
                    });
                    let _sent = startup_tx.send(result);
                    Ok(())
                }))
                .map_err(|error| format!("failed to start Windows media controls: {error}"))?;
            if !queued {
                return Err("Windows rejected media-control startup".to_string());
            }
            startup_rx.recv().map_err(|error| {
                format!("Windows media controls stopped during startup: {error}")
            })??;
            Ok(Self {
                controller,
                queue,
                context,
                event_rx: std::sync::Mutex::new(Some(event_rx)),
            })
        }

        fn take_event_rx(&self) -> Option<Receiver<WindowsMediaEvent>> {
            self.event_rx.lock().ok()?.take()
        }

        fn update(&self, state: Option<WindowsMediaState>) {
            self.send(WindowsMediaCommand::Update(state));
        }

        fn set_position(&self, position_millis: u64) {
            self.send(WindowsMediaCommand::SetPosition(position_millis));
        }

        fn send(&self, command: WindowsMediaCommand) {
            let context = Arc::clone(&self.context);
            match self.queue.TryEnqueue(&DispatcherQueueHandler::new(move || {
                let Ok(mut context) = context.lock() else {
                    return Ok(());
                };
                let Some(context) = context.as_mut() else {
                    return Ok(());
                };
                match &command {
                    WindowsMediaCommand::Update(state) => {
                        context.duration_millis =
                            state.as_ref().map_or(0, |state| state.duration_millis);
                        apply_windows_state(&context.controls, state.as_ref());
                    }
                    WindowsMediaCommand::SetPosition(position_millis) => {
                        update_windows_timeline(
                            &context.controls,
                            context.duration_millis,
                            *position_millis,
                        );
                    }
                }
                Ok(())
            })) {
                Ok(true) => {}
                Ok(false) => warn!("Windows rejected a media-control update"),
                Err(error) => warn!(%error, "failed to update Windows media controls"),
            }
        }

        fn shut_down(&mut self) {
            let context = Arc::clone(&self.context);
            if let Err(error) = self.queue.TryEnqueue(&DispatcherQueueHandler::new(move || {
                if let Ok(mut context) = context.lock() {
                    context.take();
                }
                Ok(())
            })) {
                warn!(%error, "failed to clear Windows media controls");
            }
            match self.controller.ShutdownQueueAsync() {
                Ok(shutdown) => {
                    if let Err(error) = shutdown.join() {
                        warn!(%error, "failed to stop the Windows media thread");
                    }
                }
                Err(error) => warn!(%error, "failed to stop the Windows media thread"),
            }
        }
    }

    impl Drop for WindowsMediaManager {
        fn drop(&mut self) {
            self.shut_down();
        }
    }

    fn create_windows_context(
        event_tx: Sender<WindowsMediaEvent>,
    ) -> Result<WindowsMediaContext, String> {
        let player = MediaPlayer::new()
            .map_err(|error| format!("failed to create the Windows media player: {error}"))?;
        player
            .CommandManager()
            .and_then(|manager| manager.SetIsEnabled(false))
            .map_err(|error| format!("failed to disable Windows playback commands: {error}"))?;
        let controls = player
            .SystemMediaTransportControls()
            .map_err(|error| format!("failed to get Windows media controls: {error}"))?;
        controls
            .SetIsEnabled(true)
            .map_err(|error| format!("failed to enable Windows media controls: {error}"))?;
        install_windows_handlers(&controls, event_tx)?;
        Ok(WindowsMediaContext {
            _player: player,
            controls,
            duration_millis: 0,
        })
    }

    fn install_windows_handlers(
        controls: &SystemMediaTransportControls,
        event_tx: Sender<WindowsMediaEvent>,
    ) -> Result<(), String> {
        let button_tx = event_tx.clone();
        controls
            .ButtonPressed(&TypedEventHandler::<
                SystemMediaTransportControls,
                SystemMediaTransportControlsButtonPressedEventArgs,
            >::new(move |_, args| {
                let event = match args.ok()?.Button()? {
                    SystemMediaTransportControlsButton::Play => WindowsMediaEvent::Play,
                    SystemMediaTransportControlsButton::Pause => WindowsMediaEvent::Pause,
                    SystemMediaTransportControlsButton::Next => WindowsMediaEvent::Next,
                    SystemMediaTransportControlsButton::Previous => WindowsMediaEvent::Previous,
                    SystemMediaTransportControlsButton::Stop => WindowsMediaEvent::Stop,
                    _ => return Ok(()),
                };
                let _sent = button_tx.send(event);
                Ok(())
            }))
            .map_err(|error| format!("failed to listen for Windows media buttons: {error}"))?;

        let position_tx = event_tx.clone();
        controls
            .PlaybackPositionChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                PlaybackPositionChangeRequestedEventArgs,
            >::new(move |_, args| {
                let duration = args.ok()?.RequestedPlaybackPosition()?.Duration;
                let position_millis = u64::try_from(duration.max(0) / 10_000).unwrap_or(0);
                let _sent = position_tx.send(WindowsMediaEvent::SetPosition(position_millis));
                Ok(())
            }))
            .map_err(|error| format!("failed to listen for Windows media seeking: {error}"))?;

        let shuffle_tx = event_tx.clone();
        controls
            .ShuffleEnabledChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                ShuffleEnabledChangeRequestedEventArgs,
            >::new(move |_, args| {
                let _sent = shuffle_tx.send(WindowsMediaEvent::SetShuffle(
                    args.ok()?.RequestedShuffleEnabled()?,
                ));
                Ok(())
            }))
            .map_err(|error| format!("failed to listen for Windows shuffle changes: {error}"))?;

        controls
            .AutoRepeatModeChangeRequested(&TypedEventHandler::<
                SystemMediaTransportControls,
                AutoRepeatModeChangeRequestedEventArgs,
            >::new(move |_, args| {
                if let Some(repeat) =
                    repeat_mode_from_windows(args.ok()?.RequestedAutoRepeatMode()?)
                {
                    let _sent = event_tx.send(WindowsMediaEvent::SetRepeat(repeat));
                }
                Ok(())
            }))
            .map_err(|error| format!("failed to listen for Windows repeat changes: {error}"))?;
        Ok(())
    }

    fn apply_windows_state(
        controls: &SystemMediaTransportControls,
        state: Option<&WindowsMediaState>,
    ) {
        let Some(state) = state else {
            let _updated = controls.SetIsPlayEnabled(false);
            let _updated = controls.SetIsPauseEnabled(false);
            let _updated = controls.SetIsStopEnabled(false);
            let _updated = controls.SetIsNextEnabled(false);
            let _updated = controls.SetIsPreviousEnabled(false);
            let _updated = controls.SetPlaybackStatus(MediaPlaybackStatus::Stopped);
            if let Ok(display) = controls.DisplayUpdater() {
                let _cleared = display.ClearAll();
                let _updated = display.Update();
            }
            return;
        };

        let _updated = controls.SetIsPlayEnabled(state.can_play);
        let _updated = controls.SetIsPauseEnabled(state.can_pause);
        let _updated = controls.SetIsStopEnabled(state.can_stop);
        let _updated = controls.SetIsNextEnabled(state.can_next);
        let _updated = controls.SetIsPreviousEnabled(state.can_previous);
        let _updated = controls.SetAutoRepeatMode(repeat_mode_to_windows(state.repeat));
        let _updated = controls.SetShuffleEnabled(state.shuffle);
        let _updated = controls.SetPlaybackRate(1.0);
        let _updated = controls.SetPlaybackStatus(match state.playback {
            WindowsPlaybackState::Changing => MediaPlaybackStatus::Changing,
            WindowsPlaybackState::Playing => MediaPlaybackStatus::Playing,
            WindowsPlaybackState::Paused => MediaPlaybackStatus::Paused,
            WindowsPlaybackState::Stopped => MediaPlaybackStatus::Stopped,
        });
        update_windows_timeline(controls, state.duration_millis, state.position_millis);

        if let Ok(display) = controls.DisplayUpdater() {
            let _cleared = display.ClearAll();
            let _updated = display.SetType(MediaPlaybackType::Music);
            if let Ok(properties) = display.MusicProperties() {
                let _updated = properties.SetTitle(&HSTRING::from(&state.title));
                let _updated = properties.SetArtist(&HSTRING::from(&state.artist));
                let _updated = properties.SetAlbumTitle(&HSTRING::from(&state.album));
            }
            if let Some(art_url) = state.art_url.as_ref()
                && let Ok(uri) = Uri::CreateUri(&HSTRING::from(art_url))
                && let Ok(stream) = RandomAccessStreamReference::CreateFromUri(&uri)
            {
                let _updated = display.SetThumbnail(&stream);
            }
            let _updated = display.Update();
        }
    }

    fn update_windows_timeline(
        controls: &SystemMediaTransportControls,
        duration_millis: u64,
        position_millis: u64,
    ) {
        let Ok(timeline) = SystemMediaTransportControlsTimelineProperties::new() else {
            return;
        };
        let duration = windows_time(duration_millis);
        let position = windows_time(position_millis.min(duration_millis));
        let _updated = timeline.SetStartTime(TimeSpan { Duration: 0 });
        let _updated = timeline.SetMinSeekTime(TimeSpan { Duration: 0 });
        let _updated = timeline.SetPosition(TimeSpan { Duration: position });
        let _updated = timeline.SetMaxSeekTime(TimeSpan { Duration: duration });
        let _updated = timeline.SetEndTime(TimeSpan { Duration: duration });
        let _updated = controls.UpdateTimelineProperties(&timeline);
    }

    fn windows_time(milliseconds: u64) -> i64 {
        milliseconds.min(i64::MAX as u64 / 10_000) as i64 * 10_000
    }

    const fn repeat_mode_to_windows(repeat: RepeatMode) -> MediaPlaybackAutoRepeatMode {
        match repeat {
            RepeatMode::Off => MediaPlaybackAutoRepeatMode::None,
            RepeatMode::One => MediaPlaybackAutoRepeatMode::Track,
            RepeatMode::All => MediaPlaybackAutoRepeatMode::List,
        }
    }

    const fn repeat_mode_from_windows(repeat: MediaPlaybackAutoRepeatMode) -> Option<RepeatMode> {
        match repeat {
            MediaPlaybackAutoRepeatMode::None => Some(RepeatMode::Off),
            MediaPlaybackAutoRepeatMode::Track => Some(RepeatMode::One),
            MediaPlaybackAutoRepeatMode::List => Some(RepeatMode::All),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{RepeatMode, repeat_mode_from_windows, repeat_mode_to_windows, windows_time};

        #[test]
        fn repeat_mapping_round_trips() {
            for repeat in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
                assert_eq!(
                    repeat_mode_from_windows(repeat_mode_to_windows(repeat)),
                    Some(repeat)
                );
            }
        }

        #[test]
        fn timeline_conversion_is_bounded() {
            assert_eq!(windows_time(1_500), 15_000_000);
            assert_eq!(windows_time(u64::MAX), i64::MAX / 10_000 * 10_000);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gio::prelude::FileExt;
    use mediaplayer::{
        Artwork, CommandToken, HandlerStatus, NowPlayingInfo, NowPlayingInfoCenter,
        NowPlayingMediaType, PlaybackState, RemoteCommandCenter, RepeatType, ShuffleType,
    };
    use playback::{PlaybackView, PositionDiscontinuity, RepeatMode, TransportHandle};

    pub struct MediaControls {
        center: NowPlayingInfoCenter,
        state: RefCell<MediaState>,
        _commands: Vec<CommandToken>,
    }

    #[derive(Default)]
    struct MediaState {
        title: String,
        artist: String,
        album: String,
        duration_millis: u64,
        position_millis: u64,
        playback: PlaybackState,
        artwork: Option<Artwork>,
        current: bool,
    }

    impl MediaControls {
        pub fn start(transport: TransportHandle) -> Rc<Self> {
            Rc::new(Self {
                center: NowPlayingInfoCenter::default_center(),
                state: RefCell::new(MediaState::default()),
                _commands: install_commands(transport),
            })
        }

        pub fn observe(
            self: &Rc<Self>,
            playback: Option<&PlaybackView>,
            art_url: Option<String>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
            let Some(playback) = playback else {
                self.clear();
                return;
            };
            let Some(media) = playback.transport.current.as_deref() else {
                self.clear();
                return;
            };
            update_commands(playback);
            let mut state = self.state.borrow_mut();
            state.title.clone_from(&media.track.title);
            state.artist.clone_from(&media.track.artist);
            state.album.clone_from(&media.track.album);
            state.duration_millis = playback.transport.duration_millis;
            state.position_millis = playback.transport.position_millis;
            state.playback = playback_state(playback);
            state.artwork = art_url.and_then(load_artwork);
            state.current = true;
            self.apply(&state);
        }

        pub fn observe_position(
            self: &Rc<Self>,
            position_millis: Option<u64>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
            let Some(position_millis) = position_millis else {
                self.clear();
                return;
            };
            let mut state = self.state.borrow_mut();
            if !state.current || state.position_millis == position_millis {
                return;
            }
            state.position_millis = position_millis;
            self.apply(&state);
        }

        fn apply(&self, state: &MediaState) {
            let rate = if state.playback == PlaybackState::Playing {
                1.0
            } else {
                0.0
            };
            let info = NowPlayingInfo::new()
                .title(&state.title)
                .artist(&state.artist)
                .album_title(&state.album)
                .playback_duration(state.duration_millis as f64 / 1_000.0)
                .elapsed_playback_time(state.position_millis as f64 / 1_000.0)
                .playback_rate(rate)
                .default_playback_rate(1.0)
                .media_type(NowPlayingMediaType::Audio);
            self.center
                .set_now_playing_info_with_artwork(&info, state.artwork.as_ref());
            self.center.set_playback_state(state.playback);
        }

        fn clear(&self) {
            self.state.borrow_mut().current = false;
            disable_commands();
            self.center.set_playback_state(PlaybackState::Stopped);
            self.center.clear();
        }
    }

    fn playback_state(playback: &PlaybackView) -> PlaybackState {
        match playback.transport.effective_state() {
            playback::TransportStatus::Playing | playback::TransportStatus::Buffering => {
                PlaybackState::Playing
            }
            playback::TransportStatus::Resolving | playback::TransportStatus::Paused => {
                PlaybackState::Paused
            }
            playback::TransportStatus::Stopped | playback::TransportStatus::Failed => {
                PlaybackState::Stopped
            }
        }
    }

    fn load_artwork(uri: String) -> Option<Artwork> {
        let path = gio::File::for_uri(&uri).path()?;
        Artwork::from_path(path.to_str()?).ok()
    }

    fn update_commands(playback: &PlaybackView) {
        let commands = RemoteCommandCenter::shared();
        let has_current = playback.transport.current.is_some();
        let has_active_run = playback
            .transport
            .current
            .as_ref()
            .is_some_and(|media| media.id.run.is_some())
            && !matches!(
                playback.transport.state,
                playback::TransportStatus::Stopped | playback::TransportStatus::Failed
            );
        let can_go_next = has_current
            && (playback.queue.next_occurrence.is_some() || playback.controls.auto_dj_enabled);

        commands.play_command().set_enabled(has_current);
        commands.pause_command().set_enabled(has_active_run);
        commands.stop_command().set_enabled(has_active_run);
        commands
            .toggle_play_pause_command()
            .set_enabled(has_current);
        commands.next_track_command().set_enabled(can_go_next);
        commands.previous_track_command().set_enabled(has_current);
        commands
            .change_playback_position_command()
            .set_enabled(has_current && playback.transport.can_seek);

        let repeat = commands.change_repeat_mode_command();
        repeat.set_enabled(has_current);
        repeat.set_current_repeat_type(repeat_type(playback.controls.repeat_mode));
        let shuffle = commands.change_shuffle_mode_command();
        shuffle.set_enabled(has_current);
        shuffle.set_current_shuffle_type(if playback.controls.shuffle_enabled {
            ShuffleType::Items
        } else {
            ShuffleType::Off
        });
    }

    fn disable_commands() {
        let commands = RemoteCommandCenter::shared();
        commands.play_command().set_enabled(false);
        commands.pause_command().set_enabled(false);
        commands.stop_command().set_enabled(false);
        commands.toggle_play_pause_command().set_enabled(false);
        commands.next_track_command().set_enabled(false);
        commands.previous_track_command().set_enabled(false);
        commands
            .change_playback_position_command()
            .set_enabled(false);
        commands.change_repeat_mode_command().set_enabled(false);
        commands.change_shuffle_mode_command().set_enabled(false);
    }

    const fn repeat_type(repeat: RepeatMode) -> RepeatType {
        match repeat {
            RepeatMode::Off => RepeatType::Off,
            RepeatMode::One => RepeatType::One,
            RepeatMode::All => RepeatType::All,
        }
    }

    const fn repeat_mode(repeat: RepeatType) -> RepeatMode {
        match repeat {
            RepeatType::Off => RepeatMode::Off,
            RepeatType::One => RepeatMode::One,
            RepeatType::All => RepeatMode::All,
        }
    }

    fn install_commands(transport: TransportHandle) -> Vec<CommandToken> {
        let commands = RemoteCommandCenter::shared();
        vec![
            commands.on_play(command(&transport, TransportCommand::Play)),
            commands.on_pause(command(&transport, TransportCommand::Pause)),
            commands.on_stop(command(&transport, TransportCommand::Stop)),
            commands.on_toggle_play_pause(command(&transport, TransportCommand::Toggle)),
            commands.on_next_track(command(&transport, TransportCommand::Next)),
            commands.on_previous_track(command(&transport, TransportCommand::Previous)),
            commands.on_change_playback_position({
                let transport = transport.clone();
                move |event| {
                    if let Some(position) = event.position
                        && position.is_finite()
                        && position >= 0.0
                    {
                        transport.seek_millis((position * 1_000.0).min(u64::MAX as f64) as u64);
                    }
                    HandlerStatus::Success
                }
            }),
            commands.on_change_repeat_mode({
                let transport = transport.clone();
                move |event| {
                    let Some(repeat) = event.repeat_type else {
                        return HandlerStatus::CommandFailed;
                    };
                    transport.set_repeat(repeat_mode(repeat));
                    HandlerStatus::Success
                }
            }),
            commands.on_change_shuffle_mode({
                let transport = transport.clone();
                move |event| {
                    let Some(shuffle) = event.shuffle_type else {
                        return HandlerStatus::CommandFailed;
                    };
                    transport.set_shuffle(shuffle != ShuffleType::Off);
                    HandlerStatus::Success
                }
            }),
        ]
    }

    #[derive(Clone, Copy)]
    enum TransportCommand {
        Play,
        Pause,
        Stop,
        Toggle,
        Next,
        Previous,
    }

    fn command(
        transport: &TransportHandle,
        command: TransportCommand,
    ) -> impl FnMut(mediaplayer::CommandEvent) -> HandlerStatus + Send + 'static {
        let transport = transport.clone();
        move |_| {
            match command {
                TransportCommand::Play => transport.play(),
                TransportCommand::Pause => transport.pause(),
                TransportCommand::Stop => transport.stop(),
                TransportCommand::Toggle => transport.play_pause(),
                TransportCommand::Next => transport.next(),
                TransportCommand::Previous => transport.previous(),
            }
            HandlerStatus::Success
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{RepeatMode, repeat_mode, repeat_type};

        #[test]
        fn repeat_mapping_round_trips() {
            for repeat in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
                assert_eq!(repeat_mode(repeat_type(repeat)), repeat);
            }
        }
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
)))]
mod unsupported {
    use std::rc::Rc;

    use playback::{PlaybackView, PositionDiscontinuity, TransportHandle};

    pub struct MediaControls;

    impl MediaControls {
        pub fn start(_transport: TransportHandle) -> Rc<Self> {
            Rc::new(Self)
        }

        pub fn observe(
            self: &Rc<Self>,
            _playback: Option<&PlaybackView>,
            _art_url: Option<String>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
        }

        pub fn observe_position(
            self: &Rc<Self>,
            _position_millis: Option<u64>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
        }
    }
}

#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
pub use freedesktop::MediaControls;
#[cfg(target_os = "macos")]
pub use macos::MediaControls;
#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    all(unix, not(any(target_os = "android", target_vendor = "apple")))
)))]
pub use unsupported::MediaControls;
#[cfg(target_os = "windows")]
pub use windows::MediaControls;
