//! Peer playback uses the same authenticated requests and paged queue transfer as Connect.
use super::*;
use playback::QueueCommandPort;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ReceiverState {
    pub revision: u64,
    pub total: usize,
    pub offset: usize,
    pub current: Option<library::OccurrenceId>,
    pub rows: Vec<library::QueueOccurrence>,
    pub state: playback::TransportStatus,
    pub position: u64,
    pub duration: u64,
    pub volume: f64,
    pub muted: bool,
    pub repeat: playback::RepeatMode,
    pub shuffle: bool,
    pub auto_dj: bool,
    pub auto_dj_threshold: usize,
    pub content_id: String,
}

impl ConnectOwner {
    pub(crate) async fn select_receiver(&self, peer: &str, transfer: bool) -> Result<(), String> {
        self.request(peer, Request::CastFrom { transfer })
            .await
            .map(|_| ())
    }

    pub(crate) async fn receiver_state(&self, peer: &str) -> Result<ReceiverState, String> {
        serde_json::from_value(self.request(peer, Request::CastState).await?).map_err(error)
    }

    pub(crate) async fn receiver_control(
        &self,
        peer: &str,
        command: Control,
    ) -> Result<(), String> {
        self.request(peer, Request::Control { command })
            .await
            .map(|_| ())
    }

    pub(crate) async fn receiver_queue_page(
        &self,
        peer: &str,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<playback::QueuePage, String> {
        serde_json::from_value(
            self.request(
                peer,
                Request::CastQueuePage {
                    offset,
                    limit,
                    filter,
                },
            )
            .await?,
        )
        .map_err(error)
    }

    pub(crate) async fn stop_receiver_continuation(
        &self,
        peer: &str,
    ) -> Result<playback::ContinuationHeader, String> {
        serde_json::from_value(self.request(peer, Request::StopContinuation).await?).map_err(error)
    }

    pub(crate) async fn receiver_queue(
        &self,
        peer: &str,
    ) -> Result<Option<playback::Continuation>, String> {
        let header: Option<playback::ContinuationHeader> =
            serde_json::from_value(self.request(peer, Request::CastQueue).await?).map_err(error)?;
        let Some(header) = header else {
            return Ok(None);
        };
        let transfer = format!("receiver-{}", blake3::hash(peer.as_bytes()).to_hex());
        for offset in (0..header.total).step_by(library::QUEUE_CONTEXT_LIMIT) {
            let page: library::QueueTransferPage =
                serde_json::from_value(self.request(peer, Request::Queue { offset }).await?)
                    .map_err(error)?;
            if page.occurrences.is_empty() {
                return Err("The source queue changed during transfer".into());
            }
            self.database
                .stage_queue_transfer_page(&transfer, &page)
                .await
                .map_err(error)?;
        }
        let mut queue = self
            .database
            .prepare_queue_transfer(&transfer, header.total, &header.current)
            .await
            .map_err(error)?;
        self.database
            .import_queue_transfer_metadata(&transfer, header.total)
            .await
            .map_err(error)?;
        queue.progress_millis = header.position_millis.min(i64::MAX as u64) as i64;
        queue.repeat_mode = header.repeat;
        queue.shuffled = header.shuffled;
        Ok(Some(playback::Continuation { queue, header }))
    }

    pub(crate) async fn send_cast_input(
        &self,
        peer: &str,
        queue: library::QueueRestore,
        placement: library::QueuePlacement,
        target: Option<library::QueueReorderTarget>,
    ) -> Result<(), String> {
        let Some(current) = queue.current().cloned() else {
            return Ok(());
        };
        let header = playback::ContinuationHeader {
            total: queue.entries.len(),
            current,
            position_millis: 0,
            repeat: queue.repeat_mode,
            shuffled: queue.shuffled,
            auto_dj: false,
            auto_dj_refill_threshold: 1,
            playback_rate: 1.0,
            desired_playing: true,
            listen: None,
        };
        self.transfers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                peer.into(),
                (
                    Instant::now(),
                    playback::Continuation {
                        header: header.clone(),
                        queue,
                    },
                ),
            );
        let result = self
            .request(
                peer,
                Request::CastInput {
                    header,
                    placement,
                    target,
                },
            )
            .await;
        self.transfers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(peer);
        result.map(|_| ())
    }

    pub(super) async fn receive_cast_input(
        &self,
        peer: &str,
        header: playback::ContinuationHeader,
        placement: library::QueuePlacement,
        target: Option<library::QueueReorderTarget>,
    ) -> Result<(), String> {
        let transfer = format!("cast-{}", blake3::hash(peer.as_bytes()).to_hex());
        let result = async {
            let mut offset = 0;
            while offset < header.total {
                let page: library::QueueTransferPage =
                    serde_json::from_value(self.request(peer, Request::Queue { offset }).await?)
                        .map_err(error)?;
                if page.occurrences.is_empty() {
                    return Err("The source queue changed during transfer".into());
                }
                offset += page.occurrences.len();
                self.playback
                    .stage_continuation_page(&transfer, &page)
                    .await?;
            }
            let input = self
                .database
                .queue_input_from_transfer(&transfer, header.total, &header.current)
                .await
                .map_err(error)?;
            let playback = Arc::clone(&self.playback);
            tokio::task::spawn_blocking(move || match target {
                Some(target) => playback.insert(input, target),
                None => playback::QueueCommandPort::play(
                    playback.as_ref(),
                    playback::PlayRequest::ordered(input, 0, placement, false),
                ),
            })
            .await
            .map_err(error)?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = self.playback.discard_continuation(&transfer).await;
        }
        result
    }
}
