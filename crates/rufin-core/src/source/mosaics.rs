//! Set-once mosaics for playlists, smart playlists, genres and moods without a picture and with
//! fewer than four covers picked. It keeps those owners in memory and picks for one only when the
//! change feed names its members: a playlist's entries, a smart playlist's rule, a genre's or
//! mood's added tracks. A smart playlist is picked again on each full pass. New owners and
//! removed pictures come with the list read again.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use library::{Change, MosaicOwner, Scope};

use super::Shared;

#[derive(Default)]
struct Picks {
    /// The incomplete owners.
    wanted: HashSet<MosaicOwner>,
    /// The revisions of the feature's own last commits, which it does not hear again.
    own: Vec<u64>,
}

pub(super) fn follow(shared: &Arc<Shared>) {
    let state = Arc::new(Mutex::new(Picks::default()));
    crate::features::follow(
        &shared.runtime,
        &shared.catalog,
        Arc::downgrade(shared),
        move |shared, changes| pick(shared, Arc::clone(&state), changes),
    );
}

async fn pick(shared: Arc<Shared>, state: Arc<Mutex<Picks>>, changes: Option<Vec<Arc<Change>>>) {
    let catalog = &shared.catalog;
    let (mut wanted, own) = {
        let mut picks = state.lock().unwrap_or_else(|p| p.into_inner());
        (std::mem::take(&mut picks.wanted), picks.own.clone())
    };
    // Whether smart playlists, whose members follow plays and dates, are picked again.
    let (mut listed, mut smart) = (changes.is_none(), changes.is_none());
    let mut picks = HashSet::new();
    for change in changes.iter().flatten() {
        if !own.contains(&change.revision) {
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
    let mut own = Vec::new();
    if !picks.is_empty() {
        let owners = picks.into_iter().collect::<Vec<_>>();
        match catalog.pick_mosaics(&owners).await {
            Ok((incomplete, revisions)) => {
                own = revisions;
                for owner in owners {
                    if !incomplete.contains(&owner) {
                        wanted.remove(&owner);
                    }
                }
            }
            Err(error) => shared.warn_nonfatal(&error.to_string()),
        }
    }
    let mut picks = state.lock().unwrap_or_else(|p| p.into_inner());
    picks.wanted = wanted;
    if !own.is_empty() {
        picks.own = own;
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
    if !added.is_empty() && !wanted.is_empty() {
        picks.extend(
            catalog
                .track_tags(&added)
                .into_iter()
                .map(MosaicOwner::Tag)
                .filter(|owner| wanted.contains(owner)),
        );
    }
}
