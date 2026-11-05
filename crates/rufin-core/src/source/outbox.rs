//! Delivers favorites and ratings to their servers while a configured server takes them. A
//! queued delivery wakes it, and so does a refresh that may have made a server reachable. It
//! retries deferred ones when they are due.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use library::{Catalog, Change, Delivery, DeliveryKind, Scope, SourceKind, SourcePhase};
use sources::{SourceEntityKind, SourceError, SourceId};
use tracing::info;

use super::{Shared, SourceOwner, source_error_is_temporary, unix_seconds};
use crate::features::{Feature, Pass};
use crate::runtime::{SourceEvent, SourceNotice, SourceNoticeKind};
use crate::settings::StoredSettings;

/// How long a server that could not take a delivery waits before the next try.
const RETRY_SECONDS: i64 = 30;
const PAGE: usize = 64;

fn keeps_copy(kind: SourceKind, delivery: &Delivery) -> bool {
    delivery.kind == DeliveryKind::Rating && kind.keeps_rating(delivery.value.unwrap_or(0))
}

pub(crate) struct Outbox {
    shared: Weak<Shared>,
    /// The pass that runs when the next deferred delivery is due.
    retry: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Outbox {
    pub(super) fn new(shared: Weak<Shared>) -> Self {
        Self {
            shared,
            retry: Mutex::new(None),
        }
    }
}

impl Feature for Outbox {
    fn wanted(&self, settings: &StoredSettings, _: &Catalog) -> bool {
        settings.sources.configured.iter().any(|source| {
            source.configuration.is(|kind| {
                kind.delivers(DeliveryKind::Favorite) || kind.delivers(DeliveryKind::Rating)
            })
        })
    }

    fn hears(&self, scope: &Scope) -> bool {
        matches!(
            scope,
            Scope::Held { .. }
                | Scope::Source {
                    phase: SourcePhase::Completed,
                    ..
                }
        )
    }

    /// Delivers what is due, then waits for the next due time.
    fn apply(self: Arc<Self>, _: Option<Vec<Arc<Change>>>) -> Pass {
        Box::pin(async move {
            loop {
                let Some(owner) = self.shared.upgrade().map(|shared| SourceOwner { shared }) else {
                    return;
                };
                let next = match owner.shared.catalog.next_delivery_at().await {
                    Ok(next) => next,
                    Err(error) => {
                        owner.shared.warn_nonfatal(&error.to_string());
                        Some(unix_seconds().saturating_add(RETRY_SECONDS))
                    }
                };
                let delay = next.map(|next| next.saturating_sub(unix_seconds()).max(0) as u64);
                if delay == Some(0) {
                    owner.deliver_due().await;
                    continue;
                }
                let retry = delay.map(|delay| {
                    let outbox = Arc::clone(&self);
                    owner
                        .shared
                        .runtime
                        .spawn(async move {
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                            outbox.apply(None).await;
                        })
                        .abort_handle()
                });
                let previous = std::mem::replace(
                    &mut *self.retry.lock().unwrap_or_else(|p| p.into_inner()),
                    retry,
                );
                if let Some(previous) = previous {
                    previous.abort();
                }
                return;
            }
        })
    }

    fn stopped(&self) {
        if let Some(retry) = self.retry.lock().unwrap_or_else(|p| p.into_inner()).take() {
            retry.abort();
        }
    }
}

impl SourceOwner {
    /// One pass over the deliveries that are due. A server that cannot take one now keeps its
    /// later deliveries waiting too.
    async fn deliver_due(&self) {
        let due = match self
            .shared
            .catalog
            .due_deliveries(unix_seconds(), PAGE)
            .await
        {
            Ok(due) => due,
            Err(error) => {
                self.shared.warn_nonfatal(&error.to_string());
                return;
            }
        };
        let mut unavailable = HashSet::new();
        for delivery in due {
            let Some((source_id, entity, object_id)) = delivery_target(&delivery) else {
                self.drop_delivery(&delivery, None).await;
                continue;
            };
            if unavailable.contains(&source_id) {
                self.defer(&delivery).await;
                continue;
            }
            match self.send(&source_id, entity, &object_id, &delivery).await {
                Sent::Accepted(kind) => {
                    info!(kind = ?delivery.kind, "Delivery accepted");
                    if let Err(error) = self
                        .shared
                        .catalog
                        .delivered(&delivery, keeps_copy(kind, &delivery))
                        .await
                    {
                        self.shared.warn_nonfatal(&error.to_string());
                    }
                }
                Sent::Later => {
                    unavailable.insert(source_id);
                    self.defer(&delivery).await;
                }
                Sent::Refused => self.drop_delivery(&delivery, Some(&source_id)).await,
                Sent::Undeliverable => self.drop_delivery(&delivery, None).await,
            }
        }
    }

    async fn send(
        &self,
        source_id: &SourceId,
        entity: SourceEntityKind,
        object_id: &str,
        delivery: &Delivery,
    ) -> Sent {
        let Some(kind) = self
            .configuration(source_id)
            .and_then(|configuration| configuration.source_kind())
            .filter(|kind| kind.delivers(delivery.kind))
        else {
            return Sent::Undeliverable;
        };
        let Ok(source) = self.open_client(source_id.clone()).await else {
            return Sent::Later;
        };
        let result = match delivery.kind {
            DeliveryKind::Favorite => {
                source
                    .set_favorite(entity, object_id, delivery.value == Some(1))
                    .await
            }
            DeliveryKind::Rating => {
                let rating = delivery.value.map(|value| (value / 10).clamp(0, 10) as u8);
                source.set_rating(object_id, rating).await
            }
        };
        match result {
            Ok(()) => Sent::Accepted(kind),
            // A login that expired is fixed by signing in again, not by dropping the value.
            Err(
                SourceError::Auth(_)
                | SourceError::Server {
                    status: 401 | 403, ..
                },
            ) => Sent::Later,
            Err(error) if source_error_is_temporary(&error) => Sent::Later,
            Err(_) => Sent::Refused,
        }
    }

    /// Drops a delivery. A favorite the server refused goes with Rufin's copy, so the source's
    /// own favorite shows again; a refused rating, and a value no server takes, stay Rufin's own.
    async fn drop_delivery(&self, delivery: &Delivery, refused_by: Option<&SourceId>) {
        info!(kind = ?delivery.kind, refused = refused_by.is_some(), "Delivery dropped");
        let refused_favorite = refused_by.filter(|_| delivery.kind == DeliveryKind::Favorite);
        match self
            .shared
            .catalog
            .drop_delivery(delivery, refused_favorite.is_none())
            .await
        {
            Ok(true) => {
                if let Some(source_id) = refused_favorite {
                    self.rejected(source_id).await;
                }
            }
            Ok(false) => {}
            Err(error) => self.shared.warn_nonfatal(&error.to_string()),
        }
    }

    async fn defer(&self, delivery: &Delivery) {
        info!(kind = ?delivery.kind, "Delivery deferred");
        if let Err(error) = self
            .shared
            .catalog
            .defer_delivery(delivery, unix_seconds().saturating_add(RETRY_SECONDS))
            .await
        {
            self.shared.warn_nonfatal(&error.to_string());
        }
    }

    async fn rejected(&self, source_id: &SourceId) {
        let source_key = match self.shared.selected() {
            Some(selected) => selected.source_key,
            None => match self.shared.catalog.source_key(source_id) {
                Some(key) => key,
                None => return,
            },
        };
        self.shared
            .send(SourceEvent::Notice(SourceNotice {
                source_key,
                kind: SourceNoticeKind::FavoriteRejected,
            }))
            .await;
    }
}

enum Sent {
    /// The server took it; the source's kind decides whether Rufin keeps its copy.
    Accepted(SourceKind),
    /// The server cannot take it now.
    Later,
    /// The server refused it.
    Refused,
    /// No configured source takes it.
    Undeliverable,
}

/// The source, kind and object a delivery names. A URI that names no source item has no
/// server to take it.
fn delivery_target(delivery: &Delivery) -> Option<(SourceId, SourceEntityKind, String)> {
    let (source_id, kind, object_id) = library::source_entity_parts(&delivery.media_uri)?;
    let kind = match kind.as_str() {
        "track" => SourceEntityKind::Track,
        "album" => SourceEntityKind::Album,
        "artist" => SourceEntityKind::Artist,
        _ => return None,
    };
    Some((source_id, kind, object_id))
}
