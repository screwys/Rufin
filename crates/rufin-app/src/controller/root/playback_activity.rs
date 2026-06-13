use super::*;

#[derive(Clone, Debug)]
pub(in crate::controller) struct PlaybackActivityEntry {
    server_id: ServerId,
    track_id: TrackId,
    entry_id: QueueEntryId,
    duration_seconds: u32,
    threshold_seconds: u32,
    position_seconds: u32,
    play_recorded: bool,
    skip_recorded: bool,
    session_key: String,
}

#[derive(Clone, Debug, Default)]
pub(in crate::controller) struct PlaybackActivityState {
    current: Option<PlaybackActivityEntry>,
}

impl AppController {
    pub(in crate::controller) fn start_playback_activity(
        &self,
        server_id: &ServerId,
        entry: &QueueEntry,
        position_seconds: u32,
    ) {
        let session_key = format!(
            "{}:{}:{}",
            server_id.as_str(),
            entry.id.as_str(),
            unique_millis().unwrap_or(0)
        );
        let activity = PlaybackActivityEntry {
            server_id: server_id.clone(),
            track_id: entry.track_id.clone(),
            entry_id: entry.id.clone(),
            duration_seconds: entry.duration_seconds,
            threshold_seconds: play_threshold_seconds(entry.duration_seconds),
            position_seconds,
            play_recorded: false,
            skip_recorded: false,
            session_key,
        };
        if let Ok(mut state) = self.playback_activity.lock() {
            state.current = Some(activity);
        }
        self.record_playback_activity_progress(position_seconds);
    }

    pub(in crate::controller) fn record_playback_activity_progress(&self, seconds: u32) {
        let play = self.play_activity_at(seconds);
        if let Some(activity) = play {
            self.record_local_play(activity);
        }
    }

    pub(in crate::controller) fn record_playback_activity(&self) {
        let play = {
            let mut state = match self.playback_activity.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            let Some(activity) = state.current.as_mut() else {
                return;
            };
            activity.position_seconds = activity.duration_seconds;
            if activity.play_recorded || activity.duration_seconds < activity.threshold_seconds {
                None
            } else {
                activity.play_recorded = true;
                Some(activity.clone())
            }
        };
        if let Some(activity) = play {
            self.record_local_play(activity);
        }
    }

    pub(in crate::controller) fn record_current_skip_if_needed(&self) {
        let skip = {
            let mut state = match self.playback_activity.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            let Some(activity) = state.current.as_mut() else {
                return;
            };
            if activity.play_recorded || activity.skip_recorded {
                return;
            }
            let remaining = activity
                .duration_seconds
                .saturating_sub(activity.position_seconds);
            if activity.position_seconds >= activity.threshold_seconds || remaining <= 5 {
                return;
            }
            activity.skip_recorded = true;
            Some(activity.clone())
        };
        if let Some(activity) = skip {
            match self.store.with_store(|store| {
                store.increment_track_skip_count(&activity.server_id, &activity.track_id)
            }) {
                Ok(()) => self.emit_activity_delta(activity.track_id),
                Err(error) => {
                    warn!(
                        %error,
                        server_id = %activity.server_id,
                        track_id = %activity.track_id,
                        "failed to update local skip count"
                    );
                }
            }
        }
    }

    pub(in crate::controller) fn clear_playback_activity(&self) {
        if let Ok(mut state) = self.playback_activity.lock() {
            state.current = None;
        }
    }

    fn play_activity_at(&self, seconds: u32) -> Option<PlaybackActivityEntry> {
        let mut state = self.playback_activity.lock().ok()?;
        let activity = state.current.as_mut()?;
        activity.position_seconds = seconds.min(activity.duration_seconds);
        if activity.play_recorded || activity.position_seconds < activity.threshold_seconds {
            return None;
        }
        activity.play_recorded = true;
        Some(activity.clone())
    }

    fn record_local_play(&self, activity: PlaybackActivityEntry) {
        let result = self.store.with_store(|store| {
            let Some(saved) = store.active_server()? else {
                return Ok(false);
            };
            if saved.server.id != activity.server_id || saved.server.provider != LOCAL_PROVIDER_ID {
                return Ok(false);
            }
            store.record_local_track_played(
                &activity.server_id,
                &activity.track_id,
                &activity.session_key,
            )
        });
        match result {
            Ok(true) => self.emit_activity_delta(activity.track_id),
            Ok(false) => {}
            Err(error) => {
                warn!(
                    %error,
                    server_id = %activity.server_id,
                    track_id = %activity.track_id,
                    entry_id = activity.entry_id.as_str(),
                    "failed to update local play count"
                );
            }
        }
    }

    fn emit_activity_delta(&self, track_id: TrackId) {
        let mut delta = LibraryDelta::default();
        delta.tracks.stats.push(track_id);
        let _sent = self
            .events
            .send(ControllerEvent::LibraryDelta(Box::new(delta)));
    }
}

fn play_threshold_seconds(duration_seconds: u32) -> u32 {
    if duration_seconds <= 10 {
        return duration_seconds;
    }
    let half = duration_seconds / 2;
    if duration_seconds < 60 {
        half.max(5)
    } else {
        half.clamp(30, 240)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PlaybackEvents {
        events: Vec<PlaybackEvent>,
    }

    impl PlaybackBackend for PlaybackEvents {
        fn send(&mut self, _command: PlaybackCommand) -> Result<(), rufin_playback::PlaybackError> {
            Ok(())
        }

        fn drain_events(&mut self) -> Vec<PlaybackEvent> {
            std::mem::take(&mut self.events)
        }
    }

    #[test]
    fn playback_track_short() {
        assert_eq!(play_threshold_seconds(8), 8);
        assert_eq!(play_threshold_seconds(40), 20);
        assert_eq!(play_threshold_seconds(120), 60);
        assert_eq!(play_threshold_seconds(1_000), 240);
    }

    #[test]
    fn playback_record_threshold() {
        let (controller, _events, _snapshot, _queue, _player) =
            AppController::bootstrap_memory_for_test();
        let server_id = ServerId::new("local:server:test");
        let saved = SavedServer {
            server: ServerIdentity {
                id: server_id.clone(),
                provider: LOCAL_PROVIDER_ID.to_string(),
                name: "Local".to_string(),
                base_url: String::new(),
            },
            user_id: "local".to_string(),
            username: "local".to_string(),
            trust_invalid_cert: false,
        };
        let track = library_track(1, None, AlbumId::fake(1), "Artist", &[]);
        controller
            .store
            .with_store(|store| {
                store.save_server(&saved)?;
                store.set_active_server(&server_id)?;
                store.upsert_tracks(&server_id, std::slice::from_ref(&track), 1)?;
                Ok(())
            })
            .expect("seed local server");
        let mut queue = QueueEngine::new(server_id.clone());
        queue.play_now(&track);
        let entry = queue.current().expect("current").clone();
        *controller.queue.lock().expect("queue") = Some(queue);

        controller.start_playback_activity(&server_id, &entry, 0);
        controller.record_playback_activity_progress(90);
        controller.record_playback_activity_progress(120);

        let detail = smart_detail_named(&controller, &server_id, "Most Played");
        assert_eq!(detail.tracks.len(), 1);
        assert_eq!(detail.tracks[0].id, track.id);
        assert_eq!(detail.tracks[0].play_count, Some(1));
    }

    #[test]
    fn playback_manual_count() {
        let (controller, events, snapshot, _queue, _player) =
            AppController::bootstrap_with_fake(FakeScale::Small);
        let server_id = snapshot.server.as_ref().expect("server").id.clone();
        let first = snapshot.tracks[0].clone();
        let second = snapshot.tracks[1].clone();
        controller.play_tracks_now(vec![first.clone(), second]);
        let _queue = wait_for_queue(&events).expect("queue");
        let _playback = wait_for_playback_state(&controller, &events, PlaybackState::Buffering);

        controller.next_track();
        let delta = wait_for_activity_delta(&events);
        assert_eq!(delta.tracks.stats, vec![first.id.clone()]);
        let _queue = wait_for_queue(&events).expect("next queue");

        let detail = smart_detail_named(&controller, &server_id, "Most Skipped");
        assert_eq!(detail.tracks.len(), 1);
        assert_eq!(detail.tracks[0].id, first.id);
        assert_eq!(detail.tracks[0].skip_count, Some(1));
    }

    #[test]
    fn playback_manual_skip() {
        let (controller, events, snapshot, _queue, _player) =
            AppController::bootstrap_with_fake(FakeScale::Small);
        let server_id = snapshot.server.as_ref().expect("server").id.clone();
        let first = snapshot.tracks[0].clone();
        let second = snapshot.tracks[1].clone();
        let seek_seconds = play_threshold_seconds(first.duration_seconds);
        controller.play_tracks_now(vec![first.clone(), second]);
        let _queue = wait_for_queue(&events).expect("queue");
        let _playback = wait_for_playback_track_position(&controller, &events, &first.id, 0);

        controller.seek_millis(u64::from(seek_seconds) * 1_000);
        let _playback = wait_for_playback_track_position(
            &controller,
            &events,
            &first.id,
            u64::from(seek_seconds) * 1_000,
        );
        controller.next_track();
        let _queue = wait_for_queue(&events).expect("next queue");

        let detail = smart_detail_named(&controller, &server_id, "Most Skipped");
        assert!(detail.tracks.is_empty());
    }

    #[test]
    fn playback_eos_count() {
        let (controller, events, snapshot, _queue, _player) =
            AppController::bootstrap_with_fake(FakeScale::Small);
        let server_id = snapshot.server.as_ref().expect("server").id.clone();
        let first = snapshot.tracks[0].clone();
        let second = snapshot.tracks[1].clone();
        controller.play_tracks_now(vec![first.clone(), second]);
        let _queue = wait_for_queue(&events).expect("queue");

        controller.advance_after_end_of_stream();
        let _queue = wait_for_queue(&events).expect("eos queue");
        *controller.playback.lock().expect("playback") = Box::new(PlaybackEvents {
            events: vec![PlaybackEvent::Error("stream failed".to_string())],
        });
        controller.poll_playback_events();

        let detail = smart_detail_named(&controller, &server_id, "Most Skipped");
        assert!(detail.tracks.is_empty());
    }

    fn smart_detail_named(
        controller: &AppController,
        server_id: &ServerId,
        name: &str,
    ) -> SmartPlaylistDetail {
        controller
            .store
            .with_store(|store| {
                let page = store.load_smart_playlists(server_id, 0, 20)?;
                let playlist = page
                    .items
                    .into_iter()
                    .find(|playlist| playlist.name == name)
                    .expect("smart playlist");
                store
                    .load_smart_playlist_detail(server_id, &playlist.id)
                    .map(|detail| detail.expect("smart playlist detail"))
            })
            .expect("smart detail")
    }

    fn wait_for_activity_delta(events: &Receiver<ControllerEvent>) -> LibraryDelta {
        loop {
            match events
                .recv_timeout(Duration::from_secs(5))
                .expect("controller event")
            {
                ControllerEvent::LibraryDelta(delta) => return *delta,
                ControllerEvent::Snapshot(_)
                | ControllerEvent::LibrarySyncStatus(_)
                | ControllerEvent::HomeSectionsUpdated { .. }
                | ControllerEvent::PlaylistChanged { .. }
                | ControllerEvent::SmartPlaylistChanged { .. }
                | ControllerEvent::FavoriteChanged { .. }
                | ControllerEvent::Queue(_)
                | ControllerEvent::Playback(_)
                | ControllerEvent::Visualizer(_)
                | ControllerEvent::Lyrics(_)
                | ControllerEvent::LyricsSearchResults { .. }
                | ControllerEvent::LyricsSearchFailed { .. }
                | ControllerEvent::LyricsSaved { .. }
                | ControllerEvent::FolderLoaded { .. }
                | ControllerEvent::FolderLoadFailed { .. }
                | ControllerEvent::HomeSectionPrefetched { .. }
                | ControllerEvent::ServerDiscovery { .. }
                | ControllerEvent::CoverReady { .. }
                | ControllerEvent::CoverUnavailable { .. }
                | ControllerEvent::CoverDeferred { .. }
                | ControllerEvent::LoginStatus(_) => {}
                ControllerEvent::Error(error) => panic!("controller error: {error}"),
            }
        }
    }
}
