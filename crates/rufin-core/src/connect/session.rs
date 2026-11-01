//! Handoff and casting between Rufin devices. Each device records its playback session and
//! queue; "Continue from" restores another device's queue here, and casting sends this
//! device's queue to another device, which streams its playback state back.
use super::{
    ConnectOwner, Control, Presence, Request, Session, Stream, error,
    records::{Copy as TrackCopy, Item, Records},
};
use crate::playback::state::ContinuedListen;
use library::{QueuePlacement, QueueProvenance, QueueReorderTarget, QueueRepeatMode};
use rufin_connect::{
    media::PeerByteResponse,
    store::{self, Select},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt};

/// How long Continue waits for a playing device to record where it paused.
const PAUSED: Duration = Duration::from_secs(3);

/// A queue entry as it leaves this device.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct QueueItem {
    uri: String,
    copy: TrackCopy,
    provenance: QueueProvenance,
}

/// Where this device's playback stands: the current entry by its position in play order, as
/// the Store last saved it.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SessionValue {
    current: Option<(usize, QueueItem)>,
    position_ms: i64,
    /// When the session last changed, unix milliseconds.
    at: i64,
    queue_revision: String,
    /// The listen of the current entry as far as this device heard it.
    listen: Option<ContinuedListen>,
}

#[derive(Serialize, Deserialize)]
struct QueueValue {
    revision: String,
    items: Vec<QueueItem>,
    order: Vec<u32>,
    repeat: QueueRepeatMode,
    shuffle: bool,
    auto_dj: bool,
}

/// This device's queue for another device to play. With a placement the items go into the
/// receiver's queue there; without one they replace it at the position.
#[derive(Serialize, Deserialize)]
pub(super) struct Cast {
    items: Vec<QueueItem>,
    order: Vec<u32>,
    current: usize,
    position_ms: i64,
    playing: bool,
    repeat: QueueRepeatMode,
    shuffle: bool,
    auto_dj: bool,
    placement: Option<QueuePlacement>,
    target: Option<QueueReorderTarget>,
    listen: Option<ContinuedListen>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct QueuePage {
    rows: Vec<(usize, QueueItem)>,
    total: u64,
    window_offset: u64,
}

/// One frame of a receiver's playback state; a frame goes out when it changes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Observed {
    pub(crate) current: Option<usize>,
    pub(crate) state: playback::TransportStatus,
    pub(crate) position_ms: u64,
    pub(crate) duration_ms: u64,
    pub(crate) volume: f64,
    pub(crate) muted: bool,
    pub(crate) repeat: playback::QueueRepeatMode,
    pub(crate) shuffle: bool,
    pub(crate) auto_dj: bool,
    pub(crate) revision: u64,
    pub(crate) total: usize,
}

/// The playback state frames of a receiver.
pub(crate) struct Observing {
    lines: tokio::io::Lines<tokio::io::BufReader<Pin<Box<dyn AsyncRead + Send + Unpin>>>>,
}

impl Observing {
    /// The next frame; none when the receiver ended the stream.
    pub(crate) async fn next(&mut self) -> Result<Option<Observed>, String> {
        match self.lines.next_line().await.map_err(error)? {
            Some(line) => serde_json::from_str(&line).map(Some).map_err(error),
            None => Ok(None),
        }
    }
}

/// A session record of another device that this device can continue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Continuation {
    pub device: String,
    pub name: String,
    pub title: String,
    pub artist: String,
    pub position_ms: u64,
    pub at: i64,
}

impl Records {
    pub(super) async fn load_sessions(&self) -> Result<(), String> {
        self.remember_sessions(
            self.store
                .bodies("session")
                .await
                .map_err(error)?
                .into_iter()
                .map(|row| (row.id, row.value))
                .collect(),
        );
        Ok(())
    }

    /// Keeps the stored session records by device; a record without a value is gone.
    pub(super) fn remember_sessions(&self, rows: Vec<(String, Option<Value>)>) {
        if rows.is_empty() {
            return;
        }
        self.sessions.send_modify(|sessions| {
            for (id, value) in rows {
                let Some(device) = id.strip_prefix("session:") else {
                    continue;
                };
                match value.and_then(|value| serde_json::from_value(value).ok()) {
                    Some(value) => sessions.insert(device.to_owned(), value),
                    None => sessions.remove(device),
                };
            }
        });
    }

    /// This device's queue and session records as the Store last saved them. The queue is
    /// encoded again only when it changed.
    pub(super) async fn session_changes(&self) -> Result<Vec<store::Change>, String> {
        let auto_dj = self.owner()?.settings.load().ui.auto_dj_enabled;
        let queue = self.catalog.restore_queue().await.map_err(error)?;
        let revision = store::digest(&(&queue.entries, &queue.order)).map_err(error)?;
        let ids = [
            format!("queue:{}", self.device),
            format!("session:{}", self.device),
        ];
        let stored = self.store.digests(Select::Ids(&ids)).await.map_err(error)?;
        let shape = (&revision, queue.repeat_mode, queue.shuffled, auto_dj);
        let mut items = Vec::new();
        if stored.get(&ids[0]) != Some(&store::digest(&shape).map_err(error)?) {
            let value = QueueValue {
                revision: revision.clone(),
                items: self.queue_items(&queue.entries).await?,
                order: queue.order.to_vec(),
                repeat: queue.repeat_mode,
                shuffle: queue.shuffled,
                auto_dj,
            };
            items.push(Item::digested(ids[0].clone(), None, &value, &shape)?);
        }
        let mut current = None;
        if let Some(row) = queue.rows.first() {
            let item = self
                .items_of(vec![(row.track.clone(), row.provenance.clone())])
                .await?
                .pop();
            current = item.map(|item| (row.position, item));
        }
        let listen = match current {
            Some(_) => self.owner()?.playback.current_listen().await,
            None => None,
        };
        let session = SessionValue {
            current,
            position_ms: queue.progress_millis,
            at: now_millis(),
            queue_revision: revision,
            listen,
        };
        // A session that did not move keeps the time it last changed.
        let digest = (
            session
                .current
                .as_ref()
                .map(|(position, item)| (position, &item.uri)),
            session.position_ms,
            &session.queue_revision,
        );
        items.push(Item::digested(ids[1].clone(), None, &session, &digest)?);
        self.diff(items, Select::Ids(&ids), false).await
    }

    async fn queue_items(&self, entries: &[library::QueueEntry]) -> Result<Vec<QueueItem>, String> {
        let tracks = self.queue_tracks(entries).await?;
        let tracks = entries
            .iter()
            .map(|entry| {
                let track = tracks
                    .get(&entry.track_key)
                    .cloned()
                    .ok_or("The current track is unavailable")?;
                Ok((track, entry.provenance.clone()))
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.items_of(tracks).await
    }

    /// Queue items of the entries whose track the Store holds, as the queue panel shows them.
    async fn shown_items(
        &self,
        entries: Vec<library::QueueEntry>,
    ) -> Result<Vec<QueueItem>, String> {
        let tracks = self.queue_tracks(&entries).await?;
        let shown = entries
            .into_iter()
            .filter_map(|entry| Some((tracks.get(&entry.track_key)?.clone(), entry.provenance)))
            .collect();
        self.items_of(shown).await
    }

    /// The tracks of `entries` the Store holds, by key.
    async fn queue_tracks(
        &self,
        entries: &[library::QueueEntry],
    ) -> Result<std::collections::HashMap<library::TrackKey, library::TrackView>, String> {
        let mut keys = entries
            .iter()
            .map(|entry| entry.track_key)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        let mut tracks = std::collections::HashMap::with_capacity(keys.len());
        for page in keys.chunks(library::QUEUE_CONTEXT_LIMIT) {
            tracks.extend(
                self.catalog
                    .tracks(page, &library::ReadCancellation::new())
                    .await
                    .map_err(error)?
                    .into_iter()
                    .map(|track| (track.track_key, track)),
            );
        }
        Ok(tracks)
    }

    async fn items_of(
        &self,
        tracks: Vec<(library::TrackView, QueueProvenance)>,
    ) -> Result<Vec<QueueItem>, String> {
        let portable = self
            .portable(
                &tracks
                    .iter()
                    .map(|(track, _)| track.media_uri.clone())
                    .collect::<Vec<_>>(),
            )
            .await?;
        Ok(tracks
            .into_iter()
            .map(|(track, provenance)| QueueItem {
                uri: portable
                    .get(&track.media_uri)
                    .cloned()
                    .unwrap_or(track.media_uri),
                copy: TrackCopy {
                    title: track.metadata.title,
                    artist: track.metadata.artist,
                    album: track.metadata.album,
                    duration_ms: track.metadata.duration_millis,
                },
                provenance,
            })
            .collect())
    }

    /// Queue entries of items, with tracks this device does not hold written from their copy.
    async fn entries(&self, items: Vec<QueueItem>) -> Result<Vec<library::QueueEntry>, String> {
        let uris = self
            .local_all(
                &items
                    .iter()
                    .map(|item| item.uri.clone())
                    .collect::<Vec<_>>(),
            )
            .await?;
        let load = library::QueueLoad::items(
            items
                .into_iter()
                .zip(uris)
                .map(|(item, uri)| {
                    (
                        library::TrackReference::direct(
                            uri,
                            item.copy.title,
                            item.copy.artist,
                            item.copy.album,
                            item.copy.duration_ms,
                        ),
                        item.provenance,
                    )
                })
                .collect(),
        );
        Ok(self
            .catalog
            .queue_entries(load, None)
            .await
            .map_err(error)?
            .entries)
    }

    /// The queue another device recorded, at its session's position; none when it recorded
    /// no current entry.
    #[allow(clippy::type_complexity)]
    async fn queue_of(
        &self,
        device: &str,
    ) -> Result<Option<(library::QueueRestore, bool, Option<ContinuedListen>)>, String> {
        let Some(session) = self
            .sessions
            .borrow()
            .get(device)
            .cloned()
            .filter(|session| session.current.is_some())
        else {
            return Ok(None);
        };
        let id = format!("queue:{device}");
        let Some(queue) = self
            .store
            .bodies("queue")
            .await
            .map_err(error)?
            .into_iter()
            .find(|row| row.id == id)
            .and_then(|row| serde_json::from_value::<QueueValue>(row.value?).ok())
            .filter(|queue| queue.revision == session.queue_revision)
        else {
            return Ok(None);
        };
        let entries = self.entries(queue.items).await?;
        Ok(Some((
            library::QueueRestore {
                entries: entries.into(),
                order: queue.order.into(),
                current_index: session.current.map(|(position, _)| position),
                progress_millis: session.position_ms,
                repeat_mode: queue.repeat,
                shuffled: queue.shuffle,
                ..Default::default()
            },
            queue.auto_dj,
            session.listen,
        )))
    }
}

impl ConnectOwner {
    /// Follows the session records and whether this device plays, for the Continue offer.
    pub(super) fn follow_sessions(self: &Arc<Self>, session: &Session) {
        let mut sessions = session.records.sessions.subscribe();
        let mut updates = self.playback.updates.subscribe();
        let weak = Arc::downgrade(self);
        self.runtime
            .spawn(session.stop.clone().run_until_cancelled_owned(async move {
                let mut playing = false;
                loop {
                    tokio::select! {
                        changed = sessions.changed() => if changed.is_err() {
                            return;
                        },
                        update = updates.recv() => {
                            let Ok(projection) = update else { return };
                            let now = projection.is_some_and(|projection| {
                                projection.view.transport.effective_state()
                                    == playback::TransportStatus::Playing
                            });
                            if std::mem::replace(&mut playing, now) == now {
                                continue;
                            }
                        }
                    }
                    let Some(owner) = weak.upgrade() else { return };
                    owner.offer();
                }
            }));
        self.offer();
    }

    /// Marks each paired device whose session holds something to continue, and offers the
    /// newest session of another trusted device that is newer than this device's own, while
    /// this device does not play.
    pub(super) fn offer(&self) {
        let playing = self
            .playback
            .current_projection()
            .is_some_and(|projection| {
                projection.view.transport.effective_state() == playback::TransportStatus::Playing
            });
        let mut continuable = std::collections::BTreeSet::new();
        let mut continuation: Option<Continuation> = None;
        if let Some(session) = self.session() {
            let devices = session.records.devices();
            let sessions = session.records.sessions.borrow();
            let own = sessions
                .get(&session.records.device)
                .map_or(i64::MIN, |own| own.at);
            for (device, value) in sessions.iter() {
                let Some((_, item)) = value.current.as_ref() else {
                    continue;
                };
                continuable.insert(device.clone());
                let Some(name) = devices.get(device) else {
                    continue;
                };
                if playing
                    || value.at <= own
                    || continuation
                        .as_ref()
                        .is_some_and(|offer| offer.at > value.at)
                {
                    continue;
                }
                continuation = Some(Continuation {
                    device: device.clone(),
                    name: name.clone(),
                    title: item.copy.title.clone(),
                    artist: item.copy.artist.clone(),
                    position_ms: value.position_ms.max(0) as u64,
                    at: value.at,
                });
            }
        }
        self.status.send_if_modified(|status| {
            let mut changed = status.continuation != continuation;
            status.continuation = continuation;
            for device in &mut status.devices {
                let next = device.enrolled && continuable.contains(&device.id);
                changed |= std::mem::replace(&mut device.continuable, next) != next;
            }
            changed
        });
    }

    /// Takes over another device's queue at its position. A device that plays is paused first,
    /// so its session records where it stopped; an unreachable one is continued from its
    /// records here. False when that device recorded nothing to continue.
    pub(crate) async fn continue_from(
        self: &Arc<Self>,
        peer: &str,
        playing: bool,
    ) -> Result<bool, String> {
        let session = self.active()?;
        let records = &session.records;
        let presence = self
            .request(peer, Request::Presence)
            .await
            .ok()
            .and_then(|answer| serde_json::from_value::<Presence>(answer).ok());
        if let Some(presence) = presence {
            if presence.playing {
                let before = records.sessions.borrow().get(peer).map(|value| value.at);
                let mut sessions = records.sessions.subscribe();
                self.request(
                    peer,
                    Request::Control {
                        command: Control::Pause,
                    },
                )
                .await?;
                let _ = tokio::time::timeout(
                    PAUSED,
                    sessions.wait_for(|sessions| sessions.get(peer).map(|value| value.at) > before),
                )
                .await;
            } else {
                self.pull_when_behind(&session, peer, presence.sequence)
                    .await;
            }
        }
        let Some((queue, auto_dj, listen)) = records.queue_of(peer).await? else {
            return Ok(false);
        };
        self.playback.continue_queue(queue, playing, listen).await?;
        let playback = Arc::clone(&self.playback);
        tokio::task::spawn_blocking(move || playback.send_cast_auto_dj(auto_dj))
            .await
            .map_err(error)?;
        Ok(true)
    }

    /// Plays `queue` on another device: from its current entry at its position, or at
    /// `placement` in the receiver's queue.
    pub(crate) async fn cast(
        &self,
        peer: &str,
        queue: &library::QueueRestore,
        playing: bool,
        placement: Option<(QueuePlacement, Option<QueueReorderTarget>)>,
    ) -> Result<(), String> {
        let records = Arc::clone(&self.active()?.records);
        let (placement, target) = placement.unzip();
        // The receiver that takes the queue over at its position carries on its listen.
        let listen = match placement {
            None => self.playback.current_listen().await,
            Some(_) => None,
        };
        let cast = Cast {
            items: records.queue_items(&queue.entries).await?,
            order: queue.order.to_vec(),
            current: queue.current_index.unwrap_or_default(),
            position_ms: queue.progress_millis,
            playing,
            repeat: queue.repeat_mode,
            shuffle: queue.shuffled,
            auto_dj: self.settings.load().ui.auto_dj_enabled,
            placement,
            target: target.flatten(),
            listen,
        };
        self.request(peer, Request::Cast(cast)).await.map(drop)
    }

    pub(super) async fn receive_cast(&self, cast: Cast) -> Result<(), String> {
        let records = Arc::clone(&self.active()?.records);
        let entries = records.entries(cast.items).await?;
        let playback = Arc::clone(&self.playback);
        let Some(placement) = cast.placement else {
            playback
                .continue_queue(
                    library::QueueRestore {
                        entries: entries.into(),
                        order: cast.order.into(),
                        current_index: Some(cast.current),
                        progress_millis: cast.position_ms,
                        repeat_mode: cast.repeat,
                        shuffled: cast.shuffle,
                        ..Default::default()
                    },
                    cast.playing,
                    cast.listen,
                )
                .await?;
            return tokio::task::spawn_blocking(move || playback.send_cast_auto_dj(cast.auto_dj))
                .await
                .map_err(error);
        };
        let input = library::QueueLoad::Captured(entries.into());
        tokio::task::spawn_blocking(move || match cast.target {
            Some(target) => playback.insert_load(input, target),
            None => playback.play_selection(
                crate::playback::PlayRequest::from_input(input, placement)
                    .with_anchor_index(cast.current),
            ),
        })
        .await
        .map_err(error)
    }

    pub(crate) async fn control(&self, peer: &str, command: Control) -> Result<(), String> {
        self.request(peer, Request::Control { command })
            .await
            .map(drop)
    }

    /// Another device's whole queue in play order, as its queue panel shows it, as queue
    /// entries of this device.
    pub(crate) async fn queue(&self, peer: &str) -> Result<Vec<library::QueueEntry>, String> {
        let records = Arc::clone(&self.active()?.records);
        let items: Vec<QueueItem> =
            serde_json::from_value(self.request(peer, Request::Queue).await?).map_err(error)?;
        records.entries(items).await
    }

    pub(super) async fn answer_queue(&self) -> Result<Vec<QueueItem>, String> {
        let records = Arc::clone(&self.active()?.records);
        records
            .shown_items(self.playback.queue_entries().await?)
            .await
    }

    /// A page of another device's queue, as queue items of this device.
    pub(crate) async fn queue_page(
        &self,
        peer: &str,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<(Vec<(usize, library::QueueEntry)>, u64, u64), String> {
        let records = Arc::clone(&self.active()?.records);
        let page: QueuePage = serde_json::from_value(
            self.request(
                peer,
                Request::QueuePage {
                    offset,
                    limit,
                    filter,
                },
            )
            .await?,
        )
        .map_err(error)?;
        let (positions, items): (Vec<_>, Vec<_>) = page.rows.into_iter().unzip();
        let entries = records.entries(items).await?;
        Ok((
            positions.into_iter().zip(entries).collect(),
            page.total,
            page.window_offset,
        ))
    }

    pub(super) async fn answer_queue_page(
        &self,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<QueuePage, String> {
        let records = Arc::clone(&self.active()?.records);
        let page = self
            .playback
            .receiver_queue_page(offset, limit, filter)
            .await?;
        let positions = page.rows.iter().map(|row| row.position).collect::<Vec<_>>();
        let items = records
            .items_of(
                page.rows
                    .iter()
                    .map(|row| (row.track.clone(), row.provenance.clone()))
                    .collect(),
            )
            .await?;
        Ok(QueuePage {
            rows: positions.into_iter().zip(items).collect(),
            total: page.total,
            window_offset: page.window_offset,
        })
    }

    /// Streams another device's playback state.
    pub(crate) async fn observe(&self, peer: &str) -> Result<Observing, String> {
        let network = self
            .network
            .lock()
            .await
            .clone()
            .ok_or_else(|| localization::tr("Connect is not enabled"))?;
        let response = network
            .stream_request(peer, serde_json::to_vec(&Stream::Observe).map_err(error)?)
            .await
            .map_err(error)?;
        Ok(Observing {
            lines: tokio::io::BufReader::new(response.reader).lines(),
        })
    }

    /// This device's playback state for a device that casts here: one line per change, until
    /// that device closes the stream or Connect stops.
    pub(super) fn observed(&self) -> Result<PeerByteResponse, String> {
        let session = self.active()?;
        let (mut frames, reader) = tokio::io::duplex(4096);
        let mut updates = self.playback.updates.subscribe();
        let playback = Arc::downgrade(&self.playback);
        self.runtime
            .spawn(session.stop.clone().run_until_cancelled_owned(async move {
                let mut sent: Option<Observed> = None;
                while let Ok(projection) = updates.recv().await {
                    let Some(playback) = playback.upgrade() else {
                        return;
                    };
                    let frame = playback.observed(projection.as_ref());
                    let seeked = projection.as_ref().is_some_and(|projection| {
                        projection.notices.iter().any(|notice| {
                            matches!(notice, playback::PlaybackNotice::PositionDiscontinuity(_))
                        })
                    });
                    let unchanged = sent.as_ref().is_some_and(|sent| {
                        Observed {
                            position_ms: frame.position_ms,
                            ..sent.clone()
                        } == frame
                    });
                    if unchanged && !seeked {
                        continue;
                    }
                    let Ok(mut line) = serde_json::to_vec(&frame) else {
                        return;
                    };
                    line.push(b'\n');
                    if frames.write_all(&line).await.is_err() {
                        return;
                    }
                    sent = Some(frame);
                }
            }));
        Ok(PeerByteResponse {
            metadata: Value::Null,
            reader: Box::pin(reader),
        })
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as i64)
}
