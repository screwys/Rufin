#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]
mod freedesktop {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use app_identity::{APP_ID, DISPLAY_NAME};
    use glib;
    use mpris_server::{
        LoopStatus, Metadata, PlaybackStatus, Player as MprisPlayer, Time, TrackId as MprisTrackId,
    };
    use playback::{PlaybackView, PositionDiscontinuity, QueueRepeatMode, RunId, TransportStatus};
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
        transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
        player: RefCell<Option<Rc<MprisPlayer>>>,
        desired: RefCell<Option<MprisDesiredState>>,
        applied: RefCell<Option<MprisDesiredState>>,
        pending_seeked: Cell<Option<PositionDiscontinuity>>,
        generation: Cell<u64>,
        running: Cell<bool>,
    }

    impl MediaControls {
        fn new(transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>) -> Self {
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
        pub fn start(
            transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
            open_uri: impl Fn(&str) + 'static,
        ) -> Rc<Self> {
            let mpris = Rc::new(Self::new(transport));
            let setup = Rc::clone(&mpris);
            glib::spawn_future_local(async move {
                setup.install(open_uri).await;
            });
            mpris
        }

        pub fn observe(
            self: &Rc<Self>,
            playback: Option<&PlaybackView>,
            art_url: Option<String>,
            lyrics: Option<String>,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            self.queue(
                mpris_desired_state(playback, art_url, lyrics),
                discontinuity,
            );
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

        async fn install(self: Rc<Self>, open_uri: impl Fn(&str) + 'static) {
            let player = match MprisPlayer::builder(APP_ID)
                .identity(DISPLAY_NAME)
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
            player.connect_open_uri(move |_, uri| open_uri(uri));
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
        lyrics: Option<String>,
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
            has_current && (playback.queue.can_next || playback.controls.auto_dj_enabled)
        });
        MprisDesiredState {
            run: playback
                .and_then(|playback| playback.transport.current.as_ref())
                .and_then(|media| media.id.run),
            track_id: playback
                .and_then(|playback| playback.transport.current.as_ref())
                .map_or(MprisTrackId::NO_TRACK, |media| mpris_track_id(&media.id)),
            playback_status: playback.map_or(PlaybackStatus::Stopped, |playback| {
                mpris_playback_status(playback.transport.effective_state())
            }),
            loop_status: mpris_loop_status(playback.map_or(QueueRepeatMode::Off, |playback| {
                playback.controls.repeat_mode
            })),
            shuffle: playback.is_some_and(|playback| playback.controls.shuffle_enabled),
            metadata: mpris_metadata(playback, art_url, lyrics),
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

    fn mpris_metadata(
        playback: Option<&PlaybackView>,
        art_url: Option<String>,
        lyrics: Option<String>,
    ) -> Metadata {
        let Some((transport, entry)) = playback
            .and_then(|playback| Some((&playback.transport, playback.transport.current.as_ref()?)))
        else {
            return Metadata::builder().trackid(MprisTrackId::NO_TRACK).build();
        };
        let mut builder = Metadata::builder()
            .trackid(mpris_track_id(&entry.id))
            .title(entry.row.track.metadata.title.clone())
            .artist([entry.row.track.metadata.artist.clone()])
            .album(entry.row.track.metadata.album.clone())
            // The length playback found, which a catalog without durations lacks.
            .length(Time::from_micros(
                (transport.duration_millis as i64).saturating_mul(1_000),
            ));
        if let Some(art_url) = art_url {
            builder = builder.art_url(art_url);
        }
        if let Some(lyrics) = lyrics {
            builder = builder.lyrics(lyrics);
        }
        builder.build()
    }

    fn mpris_track_id(media: &playback::CurrentMediaId) -> MprisTrackId {
        let encoded = blake3::hash(media.media_uri.as_bytes()).to_hex();
        MprisTrackId::try_from(format!(
            "/io/github/screwys/Rufin/track/{}/{encoded}",
            media.run.map_or(0, playback::RunId::get)
        ))
        .unwrap_or(MprisTrackId::NO_TRACK)
    }

    #[cfg(test)]
    fn mpris_set_position_matches(
        playback: Option<&PlaybackView>,
        track_id: &MprisTrackId,
    ) -> bool {
        playback
            .and_then(|playback| playback.transport.current.as_ref())
            .is_some_and(|entry| &mpris_track_id(&entry.id) == track_id)
    }

    fn mpris_time(position_millis: u64) -> Time {
        Time::from_millis(position_millis.min(i64::MAX as u64) as i64)
    }

    fn mpris_loop_status(repeat_mode: QueueRepeatMode) -> LoopStatus {
        match repeat_mode {
            QueueRepeatMode::Off => LoopStatus::None,
            QueueRepeatMode::One => LoopStatus::Track,
            QueueRepeatMode::All => LoopStatus::Playlist,
        }
    }

    fn repeat_mode_from_mpris(status: LoopStatus) -> QueueRepeatMode {
        match status {
            LoopStatus::None => QueueRepeatMode::Off,
            LoopStatus::Track => QueueRepeatMode::One,
            LoopStatus::Playlist => QueueRepeatMode::All,
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
            QueueRepeatMode, TransportStatus, mpris_desired_state, mpris_loop_status,
            mpris_metadata, mpris_set_position_matches, mpris_time, mpris_track_id,
            repeat_mode_from_mpris,
        };
        use playback::{
            ControlsView, CurrentMedia, CurrentMediaId, PlaybackView, QueueProvenance, QueueRow,
            QueueSummaryView, RunId, TrackReference, TransportView,
        };
        use std::sync::Arc;

        fn test_view(
            run: u64,
            album: &str,
            state: TransportStatus,
            position_millis: u64,
        ) -> PlaybackView {
            let media_uri =
                library::source_entity_uri(&library::SourceId::new("source"), "track", "track");
            let current = CurrentMedia {
                stream: None,
                id: CurrentMediaId {
                    run: Some(RunId::new(run)),
                    media_uri,
                },
                row: Arc::new(QueueRow {
                    occurrence_id: None,
                    position: 0,
                    provenance: QueueProvenance::Manual,
                    track: {
                        let reference = TrackReference {
                            media_uri: library::source_entity_uri(
                                &library::SourceId::new("source"),
                                "track",
                                "track",
                            ),
                            artwork_binding: None,
                            metadata: library::TrackMetadata {
                                audio_properties: Default::default(),
                                title: "Track".to_string(),
                                artist: "Artist".to_string(),
                                album: album.to_string(),
                                album_display_artist: Some("Album Artist".to_string()),
                                duration_millis: 42_500,
                                disc_number: 1,
                                track_number: 1,
                                year: Some(2026),
                                release_date: None,
                                source_format: None,
                                musicbrainz_recording_id: Some("recording-id".to_string()),
                                musicbrainz_release_track_id: Some("track-id".to_string()),
                                musicbrainz_album_id: Some("album-id".to_string()),
                                musicbrainz_release_group_id: Some("release-group-id".to_string()),
                                primary_artist_musicbrainz_id: Some("artist-id".to_string()),
                                ..Default::default()
                            },
                        };
                        library::TrackView {
                            track_key: library::TrackKey::from_raw(1),
                            source_key: None,
                            source_id: None,
                            object_id: None,
                            media_uri: reference.media_uri,
                            metadata: reference.metadata,
                            artwork_binding: reference.artwork_binding,
                            state: Default::default(),
                            access: Default::default(),
                            links: Default::default(),
                        }
                    },
                }),
            };
            PlaybackView {
                queue_loading: false,
                queue_window: Vec::new(),
                queue: QueueSummaryView {
                    revision: run,
                    total: 1,
                    current_index: Some(0),
                    next_index: None,
                    can_next: false,
                },
                transport: TransportView {
                    current: Some(current),
                    state,
                    desired_playing: matches!(
                        state,
                        TransportStatus::Resolving
                            | TransportStatus::Buffering
                            | TransportStatus::Playing
                    ),
                    position_millis,
                    position_observed_at_millis: 0,
                    playback_rate: 1.0,
                    duration_millis: 42_500,
                    can_seek: true,
                    buffering_percent: None,
                    error: None,
                },
                controls: ControlsView {
                    repeat_mode: QueueRepeatMode::Off,
                    shuffle_enabled: false,
                    auto_dj_enabled: false,
                    volume: 1.0,
                    muted: false,
                    audio_output: None,
                    playback_output: playback::PlaybackOutput::Local,
                    sleep_timer: None,
                },
            }
        }

        #[test]
        fn mpris_run_paths_distinguish_duplicate_tracks_and_guard_set_position() {
            let playback = test_view(1, "Album", TransportStatus::Playing, 1_000);
            let first = playback
                .transport
                .current
                .as_ref()
                .expect("current media")
                .id
                .clone();
            let second = CurrentMediaId {
                run: Some(RunId::new(2)),
                media_uri: first.media_uri.clone(),
            };
            let first_id = mpris_track_id(&first);
            let second_id = mpris_track_id(&second);
            assert_ne!(first_id, second_id);
            let private = CurrentMediaId {
                run: first.run,
                media_uri: "https://user:secret@example.test/track?token=secret".into(),
            };
            let encoded = private
                .media_uri
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert!(!mpris_track_id(&private).to_string().contains(&encoded));

            assert!(mpris_set_position_matches(Some(&playback), &first_id));
            assert!(!mpris_set_position_matches(Some(&playback), &second_id));
        }

        #[test]
        fn mpris_exact_repeat_mapping_round_trips() {
            for repeat in [
                QueueRepeatMode::Off,
                QueueRepeatMode::One,
                QueueRepeatMode::All,
            ] {
                assert_eq!(repeat_mode_from_mpris(mpris_loop_status(repeat)), repeat);
            }
        }

        #[test]
        fn mpris_capabilities_follow_queue_summary() {
            let mut playback = test_view(1, "Album", TransportStatus::Playing, 0);
            let exhausted = mpris_desired_state(Some(&playback), None, None);
            assert!(!exhausted.can_go_next);
            assert!(exhausted.can_go_previous);

            playback.queue.next_index = Some(1);
            playback.queue.can_next = true;
            let with_next = mpris_desired_state(Some(&playback), None, None);
            assert!(with_next.can_go_next);

            playback.queue.next_index = None;
            assert!(mpris_desired_state(Some(&playback), None, None).can_go_next);
        }

        #[test]
        fn mpris_metadata_updates_when_cached_art_arrives() {
            let playback = test_view(1, "Album", TransportStatus::Paused, 0);
            assert_ne!(
                mpris_metadata(Some(&playback), None, None),
                mpris_metadata(
                    Some(&playback),
                    Some("file:///tmp/cover.png".to_string()),
                    None
                )
            );
        }

        #[test]
        fn mpris_position_update_preserves_static_desired_state() {
            let playback = test_view(1, "Album", TransportStatus::Playing, 1_000);
            let mut desired =
                mpris_desired_state(Some(&playback), Some("file:///cover.png".to_string()), None);
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

    use playback::{PlaybackView, PositionDiscontinuity, QueueRepeatMode, TransportStatus};
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
        pub fn start(
            transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
            _open_uri: impl Fn(&str) + 'static,
        ) -> Rc<Self> {
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
            _lyrics: Option<String>,
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

    fn apply_media_event(
        transport: &rufin_core::playback::PlaybackOwner,
        event: WindowsMediaEvent,
    ) {
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
        repeat: QueueRepeatMode,
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
        let media = playback.transport.current.as_ref()?;
        let has_active_run = media.id.run.is_some()
            && !matches!(
                playback.transport.state,
                TransportStatus::Stopped | TransportStatus::Failed
            );
        let can_next = playback.queue.can_next || playback.controls.auto_dj_enabled;
        Some(WindowsMediaState {
            title: media.row.track.metadata.title.clone(),
            artist: media.row.track.metadata.artist.clone(),
            album: media.row.track.metadata.album.clone(),
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
        SetRepeat(QueueRepeatMode),
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

    const fn repeat_mode_to_windows(repeat: QueueRepeatMode) -> MediaPlaybackAutoRepeatMode {
        match repeat {
            QueueRepeatMode::Off => MediaPlaybackAutoRepeatMode::None,
            QueueRepeatMode::One => MediaPlaybackAutoRepeatMode::Track,
            QueueRepeatMode::All => MediaPlaybackAutoRepeatMode::List,
        }
    }

    const fn repeat_mode_from_windows(
        repeat: MediaPlaybackAutoRepeatMode,
    ) -> Option<QueueRepeatMode> {
        match repeat {
            MediaPlaybackAutoRepeatMode::None => Some(QueueRepeatMode::Off),
            MediaPlaybackAutoRepeatMode::Track => Some(QueueRepeatMode::One),
            MediaPlaybackAutoRepeatMode::List => Some(QueueRepeatMode::All),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            QueueRepeatMode, repeat_mode_from_windows, repeat_mode_to_windows, windows_time,
        };

        #[test]
        fn repeat_mapping_round_trips() {
            for repeat in [
                QueueRepeatMode::Off,
                QueueRepeatMode::One,
                QueueRepeatMode::All,
            ] {
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
// objc2 generates every MediaPlayer method as `unsafe fn`.
#[allow(unsafe_code)]
mod macos {
    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;

    use block2::RcBlock;
    use gio::prelude::FileExt;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, Message};
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSMutableDictionary, NSNumber, NSSize, NSString};
    use objc2_media_player::{
        MPChangePlaybackPositionCommandEvent, MPChangeRepeatModeCommandEvent,
        MPChangeShuffleModeCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
        MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
        MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoMediaType,
        MPNowPlayingInfoPropertyDefaultPlaybackRate, MPNowPlayingInfoPropertyElapsedPlaybackTime,
        MPNowPlayingInfoPropertyMediaType, MPNowPlayingInfoPropertyPlaybackRate,
        MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent,
        MPRemoteCommandHandlerStatus, MPRepeatType, MPShuffleType,
    };
    use playback::{PlaybackView, PositionDiscontinuity, QueueRepeatMode};
    use rufin_core::playback::PlaybackOwner;

    pub struct MediaControls {
        center: Retained<MPNowPlayingInfoCenter>,
        state: RefCell<MediaState>,
        _commands: Vec<CommandHandler>,
    }

    struct MediaState {
        title: String,
        artist: String,
        album: String,
        duration_millis: u64,
        position_millis: u64,
        playback: MPNowPlayingPlaybackState,
        artwork: Option<Retained<MPMediaItemArtwork>>,
        current: bool,
    }

    impl MediaControls {
        pub fn start(
            transport: Arc<PlaybackOwner>,
            _open_uri: impl Fn(&str) + 'static,
        ) -> Rc<Self> {
            Rc::new(Self {
                // SAFETY: the shared center is always available.
                center: unsafe { MPNowPlayingInfoCenter::defaultCenter() },
                state: RefCell::new(MediaState {
                    title: String::new(),
                    artist: String::new(),
                    album: String::new(),
                    duration_millis: 0,
                    position_millis: 0,
                    playback: MPNowPlayingPlaybackState::Unknown,
                    artwork: None,
                    current: false,
                }),
                _commands: install_commands(transport),
            })
        }

        pub fn observe(
            self: &Rc<Self>,
            playback: Option<&PlaybackView>,
            art_url: Option<String>,
            _lyrics: Option<String>,
            _discontinuity: Option<PositionDiscontinuity>,
        ) {
            let Some(playback) = playback else {
                self.clear();
                return;
            };
            let Some(media) = playback.transport.current.as_ref() else {
                self.clear();
                return;
            };
            update_commands(playback);
            let artwork = art_url.and_then(load_artwork);
            {
                let mut state = self.state.borrow_mut();
                state.title.clone_from(&media.row.track.metadata.title);
                state.artist.clone_from(&media.row.track.metadata.artist);
                state.album.clone_from(&media.row.track.metadata.album);
                state.duration_millis = playback.transport.duration_millis;
                state.position_millis = playback.transport.position_millis;
                state.playback = playback_state(playback);
                state.artwork = artwork;
                state.current = true;
            }
            self.publish();
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
            {
                let mut state = self.state.borrow_mut();
                if !state.current || state.position_millis == position_millis {
                    return;
                }
                state.position_millis = position_millis;
            }
            self.publish();
        }

        fn publish(&self) {
            let state = self.state.borrow();
            let rate = if state.playback == MPNowPlayingPlaybackState::Playing {
                1.0
            } else {
                0.0
            };
            let info = NSMutableDictionary::<NSString, AnyObject>::new();
            let text = |key: &NSString, value: &str| info.insert(key, &NSString::from_str(value));
            let number = |key: &NSString, value: f64| info.insert(key, &NSNumber::new_f64(value));
            // SAFETY: the keys are MediaPlayer's constants and each value has the type its
            // key documents.
            unsafe {
                text(MPMediaItemPropertyTitle, &state.title);
                text(MPMediaItemPropertyArtist, &state.artist);
                text(MPMediaItemPropertyAlbumTitle, &state.album);
                number(
                    MPMediaItemPropertyPlaybackDuration,
                    state.duration_millis as f64 / 1_000.0,
                );
                number(
                    MPNowPlayingInfoPropertyElapsedPlaybackTime,
                    state.position_millis as f64 / 1_000.0,
                );
                number(MPNowPlayingInfoPropertyPlaybackRate, rate);
                number(MPNowPlayingInfoPropertyDefaultPlaybackRate, 1.0);
                info.insert(
                    MPNowPlayingInfoPropertyMediaType,
                    &NSNumber::new_usize(MPNowPlayingInfoMediaType::Audio.0),
                );
                if let Some(artwork) = &state.artwork {
                    info.insert(MPMediaItemPropertyArtwork, artwork);
                }
                self.center.setNowPlayingInfo(Some(&info));
                self.center.setPlaybackState(state.playback);
            }
        }

        fn clear(&self) {
            self.state.borrow_mut().current = false;
            disable_commands();
            // SAFETY: both setters take plain values.
            unsafe {
                self.center
                    .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
                self.center.setNowPlayingInfo(None);
            }
        }
    }

    fn playback_state(playback: &PlaybackView) -> MPNowPlayingPlaybackState {
        match playback.transport.effective_state() {
            playback::TransportStatus::Playing | playback::TransportStatus::Buffering => {
                MPNowPlayingPlaybackState::Playing
            }
            playback::TransportStatus::Resolving | playback::TransportStatus::Paused => {
                MPNowPlayingPlaybackState::Paused
            }
            playback::TransportStatus::Stopped | playback::TransportStatus::Failed => {
                MPNowPlayingPlaybackState::Stopped
            }
        }
    }

    fn load_artwork(uri: String) -> Option<Retained<MPMediaItemArtwork>> {
        let path = gio::File::for_uri(&uri).path()?;
        let image =
            NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path.to_str()?))?;
        let size = image.size();
        let request = RcBlock::new(move |_: NSSize| NonNull::from(&*image));
        // SAFETY: the artwork copies the block, which owns the image it hands out.
        Some(unsafe {
            MPMediaItemArtwork::initWithBoundsSize_requestHandler(
                MPMediaItemArtwork::alloc(),
                size,
                &request,
            )
        })
    }

    fn update_commands(playback: &PlaybackView) {
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
        let can_go_next =
            has_current && (playback.queue.can_next || playback.controls.auto_dj_enabled);

        // SAFETY: the shared command center is always available and its setters take plain
        // values.
        unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            commands.playCommand().setEnabled(has_current);
            commands.pauseCommand().setEnabled(has_active_run);
            commands.stopCommand().setEnabled(has_active_run);
            commands.togglePlayPauseCommand().setEnabled(has_current);
            commands.nextTrackCommand().setEnabled(can_go_next);
            commands.previousTrackCommand().setEnabled(has_current);
            commands
                .changePlaybackPositionCommand()
                .setEnabled(has_current && playback.transport.can_seek);

            let repeat = commands.changeRepeatModeCommand();
            repeat.setEnabled(has_current);
            repeat.setCurrentRepeatType(repeat_type(playback.controls.repeat_mode));
            let shuffle = commands.changeShuffleModeCommand();
            shuffle.setEnabled(has_current);
            shuffle.setCurrentShuffleType(if playback.controls.shuffle_enabled {
                MPShuffleType::Items
            } else {
                MPShuffleType::Off
            });
        }
    }

    fn disable_commands() {
        // SAFETY: as in `update_commands`.
        unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            commands.playCommand().setEnabled(false);
            commands.pauseCommand().setEnabled(false);
            commands.stopCommand().setEnabled(false);
            commands.togglePlayPauseCommand().setEnabled(false);
            commands.nextTrackCommand().setEnabled(false);
            commands.previousTrackCommand().setEnabled(false);
            commands.changePlaybackPositionCommand().setEnabled(false);
            commands.changeRepeatModeCommand().setEnabled(false);
            commands.changeShuffleModeCommand().setEnabled(false);
        }
    }

    const fn repeat_type(repeat: QueueRepeatMode) -> MPRepeatType {
        match repeat {
            QueueRepeatMode::Off => MPRepeatType::Off,
            QueueRepeatMode::One => MPRepeatType::One,
            QueueRepeatMode::All => MPRepeatType::All,
        }
    }

    const fn repeat_mode(repeat: MPRepeatType) -> Option<QueueRepeatMode> {
        match repeat {
            MPRepeatType::Off => Some(QueueRepeatMode::Off),
            MPRepeatType::One => Some(QueueRepeatMode::One),
            MPRepeatType::All => Some(QueueRepeatMode::All),
            _ => None,
        }
    }

    /// A handler registered on a remote command, removed again on drop.
    struct CommandHandler {
        command: Retained<MPRemoteCommand>,
        target: Retained<AnyObject>,
    }

    impl Drop for CommandHandler {
        fn drop(&mut self) {
            // SAFETY: `target` is what `addTargetWithHandler` returned for this command.
            unsafe { self.command.removeTarget(Some(&self.target)) };
        }
    }

    fn handle(
        command: &MPRemoteCommand,
        handler: impl Fn(&MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus + Send + Sync + 'static,
    ) -> CommandHandler {
        let block = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
            // SAFETY: MediaPlayer passes an event that is valid for the call.
            handler(unsafe { event.as_ref() })
        });
        // SAFETY: the command copies the block, which owns the handler, so a delivery that
        // is already running keeps it alive after the target is removed.
        let target = unsafe { command.addTargetWithHandler(&block) };
        CommandHandler {
            command: command.retain(),
            target,
        }
    }

    fn position_time(event: &MPRemoteCommandEvent) -> Option<f64> {
        let event = event.downcast_ref::<MPChangePlaybackPositionCommandEvent>()?;
        // SAFETY: a plain property read.
        Some(unsafe { event.positionTime() })
    }

    fn repeat_event(event: &MPRemoteCommandEvent) -> Option<QueueRepeatMode> {
        let event = event.downcast_ref::<MPChangeRepeatModeCommandEvent>()?;
        // SAFETY: a plain property read.
        repeat_mode(unsafe { event.repeatType() })
    }

    fn shuffle_event(event: &MPRemoteCommandEvent) -> Option<MPShuffleType> {
        let event = event.downcast_ref::<MPChangeShuffleModeCommandEvent>()?;
        // SAFETY: a plain property read.
        Some(unsafe { event.shuffleType() })
    }

    fn install_commands(transport: Arc<PlaybackOwner>) -> Vec<CommandHandler> {
        let transport_command = |command: &MPRemoteCommand, action: fn(&PlaybackOwner)| {
            let transport = transport.clone();
            handle(command, move |_| {
                action(&transport);
                MPRemoteCommandHandlerStatus::Success
            })
        };
        // SAFETY: the shared command center is always available.
        unsafe {
            let commands = MPRemoteCommandCenter::sharedCommandCenter();
            vec![
                transport_command(&commands.playCommand(), PlaybackOwner::play),
                transport_command(&commands.pauseCommand(), PlaybackOwner::pause),
                transport_command(&commands.stopCommand(), PlaybackOwner::stop),
                transport_command(
                    &commands.togglePlayPauseCommand(),
                    PlaybackOwner::play_pause,
                ),
                transport_command(&commands.nextTrackCommand(), PlaybackOwner::next),
                transport_command(&commands.previousTrackCommand(), PlaybackOwner::previous),
                handle(&commands.changePlaybackPositionCommand(), {
                    let transport = transport.clone();
                    move |event| {
                        if let Some(position) = position_time(event)
                            && position.is_finite()
                            && position >= 0.0
                        {
                            transport.seek_millis((position * 1_000.0).min(u64::MAX as f64) as u64);
                        }
                        MPRemoteCommandHandlerStatus::Success
                    }
                }),
                handle(&commands.changeRepeatModeCommand(), {
                    let transport = transport.clone();
                    move |event| {
                        let Some(repeat) = repeat_event(event) else {
                            return MPRemoteCommandHandlerStatus::CommandFailed;
                        };
                        transport.set_repeat(repeat);
                        MPRemoteCommandHandlerStatus::Success
                    }
                }),
                handle(&commands.changeShuffleModeCommand(), {
                    let transport = transport.clone();
                    move |event| {
                        let Some(shuffle) = shuffle_event(event) else {
                            return MPRemoteCommandHandlerStatus::CommandFailed;
                        };
                        transport.set_shuffle(shuffle != MPShuffleType::Off);
                        MPRemoteCommandHandlerStatus::Success
                    }
                }),
            ]
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{QueueRepeatMode, repeat_mode, repeat_type};

        #[test]
        fn repeat_mapping_round_trips() {
            for repeat in [
                QueueRepeatMode::Off,
                QueueRepeatMode::One,
                QueueRepeatMode::All,
            ] {
                assert_eq!(repeat_mode(repeat_type(repeat)), Some(repeat));
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

    use playback::{PlaybackView, PositionDiscontinuity};

    pub struct MediaControls;

    impl MediaControls {
        pub fn start(
            _transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
            _open_uri: impl Fn(&str) + 'static,
        ) -> Rc<Self> {
            Rc::new(Self)
        }

        pub fn observe(
            self: &Rc<Self>,
            _playback: Option<&PlaybackView>,
            _art_url: Option<String>,
            _lyrics: Option<String>,
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
