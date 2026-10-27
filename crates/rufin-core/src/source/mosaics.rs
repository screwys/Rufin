//! Set-once mosaics, installed while some playlist, smart playlist, genre or mood without a
//! picture has fewer than four covers picked. It keeps those owners in memory and picks for one
//! only when the change feed names its members: a playlist's entries, a smart playlist's rule, a
//! genre's or mood's added tracks. A smart playlist is picked again at launch and after a
//! reload. Once every mosaic is complete it goes.

use std::collections::HashSet;
use std::sync::{Arc, Weak};

use library::{Change, MosaicOwner, Scope};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use super::{Shared, SourceOwner};

impl SourceOwner {
    /// Installs the mosaics feature unless it runs; it goes again when nothing is left to pick.
    pub(super) fn ensure_mosaics(&self) {
        let mut installed = self
            .shared
            .mosaics
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if installed.is_some() {
            return;
        }
        let changes = self.shared.catalog.changes();
        *installed = Some(
            self.shared
                .runtime
                .spawn(run(Arc::downgrade(&self.shared), changes))
                .abort_handle(),
        );
    }
}

async fn run(shared: Weak<Shared>, mut changes: tokio::sync::broadcast::Receiver<Arc<Change>>) {
    let mut wanted = HashSet::new();
    // Whether smart playlists, whose members follow plays and dates, are picked again.
    let mut smart = true;
    let mut listed = true;
    let mut picks = HashSet::new();
    // The feature's own commits, which it does not hear again.
    let mut own = None;
    loop {
        let Some(owner) = shared.upgrade() else {
            return;
        };
        let catalog = &owner.catalog;
        if listed {
            match catalog.incomplete_mosaics().await {
                Ok(incomplete) => {
                    // Owners never picked get their first pick.
                    picks.extend(
                        incomplete
                            .iter()
                            .filter(|(owner, picked)| {
                                !picked || (smart && matches!(owner, MosaicOwner::Smart(_)))
                            })
                            .map(|(owner, _)| *owner),
                    );
                    wanted = incomplete.into_iter().map(|(owner, _)| owner).collect();
                }
                Err(error) => owner.warn_nonfatal(&error.to_string()),
            }
            smart = false;
            listed = false;
            picks.retain(|owner| wanted.contains(owner));
        }
        if !picks.is_empty() {
            let owners = picks.drain().collect::<Vec<_>>();
            match catalog.pick_mosaics(&owners).await {
                Ok((incomplete, revision)) => {
                    own = revision;
                    for owner in owners {
                        if !incomplete.contains(&owner) {
                            wanted.remove(&owner);
                        }
                    }
                }
                Err(error) => owner.warn_nonfatal(&error.to_string()),
            }
        }
        if wanted.is_empty() {
            let mut installed = owner.mosaics.lock().unwrap_or_else(|p| p.into_inner());
            // A Change that came meanwhile may name a new owner; the next one installs again.
            if changes.is_empty() {
                *installed = None;
                return;
            }
        }
        drop(owner);
        let mut next = changes.recv().await;
        loop {
            match next {
                Ok(change) if Some(change.revision) == own => {}
                Ok(change) => {
                    let Some(owner) = shared.upgrade() else {
                        return;
                    };
                    members_changed(
                        &owner.catalog,
                        &change,
                        &wanted,
                        &mut picks,
                        &mut listed,
                        &mut smart,
                    );
                }
                Err(RecvError::Lagged(_)) => (listed, smart) = (true, true),
                Err(RecvError::Closed) => return,
            }
            next = match changes.try_recv() {
                Ok(change) => Ok(change),
                Err(TryRecvError::Lagged(skipped)) => Err(RecvError::Lagged(skipped)),
                Err(_) => break,
            };
        }
    }
}

/// The incomplete owners whose members a Change may have changed. New playlists, smart
/// playlists, genres and moods, and pictures removed, come with the list read again.
fn members_changed(
    catalog: &library::Catalog,
    change: &Change,
    wanted: &HashSet<MosaicOwner>,
    picks: &mut HashSet<MosaicOwner>,
    listed: &mut bool,
    smart: &mut bool,
) {
    let mut added = Vec::new();
    for scope in &change.scopes {
        match scope {
            Scope::Reload => (*listed, *smart) = (true, true),
            Scope::Playlists
            | Scope::Collections { .. }
            | Scope::Playlist { entries: false, .. } => *listed = true,
            Scope::Playlist { key, entries: true } => {
                picks.insert(MosaicOwner::Playlist(*key));
            }
            Scope::Smart { key } => {
                *listed = true;
                picks.insert(MosaicOwner::Smart(*key));
            }
            Scope::Tracks { added: keys, .. } => added.extend_from_slice(keys),
            _ => {}
        }
    }
    if !added.is_empty() {
        picks.extend(
            catalog
                .track_tags(&added)
                .into_iter()
                .map(MosaicOwner::Tag)
                .filter(|owner| wanted.contains(owner)),
        );
    }
    // A relist keeps the owners still wanted; without one, only those wanted now.
    if !*listed {
        picks.retain(|owner| wanted.contains(owner));
    }
}
