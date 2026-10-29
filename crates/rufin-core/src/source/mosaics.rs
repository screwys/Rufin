//! Set-once mosaics, installed while some playlist, smart playlist, genre or mood without a
//! picture has fewer than four covers picked. It keeps those owners in memory and picks for one
//! only when the change feed names its members: a playlist's entries, a smart playlist's rule, a
//! genre's or mood's added tracks. A smart playlist is picked again on each full pass. Once every
//! mosaic is complete it goes, until a new owner or a removed picture may need one.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};

use futures_util::future::BoxFuture;
use library::{Change, MosaicOwner, Scope};

use super::Shared;
use crate::features::{Feature, Next};

pub(super) struct Mosaics {
    shared: Weak<Shared>,
    picks: Mutex<Picks>,
}

#[derive(Default)]
struct Picks {
    /// The incomplete owners.
    wanted: HashSet<MosaicOwner>,
    /// The revision of the feature's own last commit, which it does not hear again.
    own: Option<u64>,
}

impl Mosaics {
    pub(super) fn new(shared: Weak<Shared>) -> Self {
        Self {
            shared,
            picks: Mutex::new(Picks::default()),
        }
    }

    fn picks(&self) -> std::sync::MutexGuard<'_, Picks> {
        self.picks.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Feature for Mosaics {
    /// The full pass reads which owners are incomplete.
    fn wanted(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn hears(&self, change: &Change) -> bool {
        let picks = self.picks();
        Some(change.revision) != picks.own
            && change.scopes.iter().any(|scope| match scope {
                Scope::Reload
                | Scope::Playlists
                | Scope::Collections { .. }
                | Scope::Playlist { .. }
                | Scope::Smart { .. } => true,
                Scope::Tracks { added, .. } => !added.is_empty() && !picks.wanted.is_empty(),
                _ => false,
            })
    }

    fn apply(&self, changes: Option<Vec<Arc<Change>>>) -> BoxFuture<'_, Next> {
        Box::pin(async move {
            let Some(shared) = self.shared.upgrade() else {
                return Next::Done;
            };
            let catalog = &shared.catalog;
            let (mut wanted, own) = {
                let mut picks = self.picks();
                (std::mem::take(&mut picks.wanted), picks.own)
            };
            // Whether smart playlists, whose members follow plays and dates, are picked again.
            let (mut listed, mut smart) = (changes.is_none(), changes.is_none());
            let mut picks = HashSet::new();
            for change in changes.iter().flatten() {
                if Some(change.revision) != own {
                    members_changed(
                        catalog,
                        change,
                        &wanted,
                        &mut picks,
                        &mut listed,
                        &mut smart,
                    );
                }
            }
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
                    Err(error) => shared.warn_nonfatal(&error.to_string()),
                }
            }
            picks.retain(|owner| wanted.contains(owner));
            let mut own = None;
            if !picks.is_empty() {
                let owners = picks.into_iter().collect::<Vec<_>>();
                match catalog.pick_mosaics(&owners).await {
                    Ok((incomplete, revision)) => {
                        own = revision;
                        for owner in owners {
                            if !incomplete.contains(&owner) {
                                wanted.remove(&owner);
                            }
                        }
                    }
                    Err(error) => shared.warn_nonfatal(&error.to_string()),
                }
            }
            let done = wanted.is_empty();
            let mut picks = self.picks();
            picks.wanted = wanted;
            if own.is_some() {
                picks.own = own;
            }
            if done { Next::Done } else { Next::Wait }
        })
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
}
