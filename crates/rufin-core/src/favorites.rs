use std::{collections::HashMap, sync::Mutex};

use library::FavoriteTarget;
use tokio::sync::watch;

/// Projects the latest user intent while favorite requests are being delivered.
pub struct FavoriteIntents {
    pending: Mutex<HashMap<FavoriteTarget, bool>>,
    revision: watch::Sender<u64>,
}

impl Default for FavoriteIntents {
    fn default() -> Self {
        Self {
            pending: Mutex::default(),
            revision: watch::channel(0).0,
        }
    }
}

impl FavoriteIntents {
    pub fn projected_item_favorite(&self, item: &FavoriteTarget, fallback: bool) -> bool {
        self.pending
            .lock()
            .unwrap()
            .get(item)
            .copied()
            .unwrap_or(fallback)
    }

    pub fn set_pending(&self, item: FavoriteTarget, favorite: bool) {
        self.pending.lock().unwrap().insert(item, favorite);
        self.revision.send_modify(|revision| *revision += 1);
    }

    pub fn response_matches_pending(&self, item: &FavoriteTarget, requested: bool) -> bool {
        let mut pending = self.pending.lock().unwrap();
        match pending.get(item).copied() {
            Some(intent) if intent == requested => {
                pending.remove(item);
                drop(pending);
                self.revision.send_modify(|revision| *revision += 1);
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }
}
