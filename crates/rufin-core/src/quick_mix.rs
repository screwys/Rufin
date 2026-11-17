//! Quick mix in the selected library: what pressing an entry plays, and the editor's list of
//! the user's smart playlists to copy from.

use std::sync::Arc;

use async_channel::Receiver;
use library::{QueueProvenance, ReadCancellation};

use crate::runtime::SelectedLibrary;
use crate::settings::quick_mix::{QuickMixEditor, QuickMixEntry, add_quick_mix, quick_mix_editor};
use crate::source::SourceOwner;

/// The entries' rules in `selected`: one that names no source reads the selected one.
pub(crate) fn scoped(
    entries: &[QuickMixEntry],
    selected: &SelectedLibrary,
) -> Vec<library::SmartPlaylistDefinition> {
    entries
        .iter()
        .map(|entry| {
            let mut definition = entry.definition.clone();
            definition
                .source_id
                .get_or_insert_with(|| selected.source_id.clone());
            definition
        })
        .collect()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Plays the entry at `index` in the selected library at `placement`, in the shuffle Home's
/// covers show, then moves Quick mix to its next shuffle. True means nothing matched.
pub async fn play_quick_mix(
    queue: &Arc<crate::playback::PlaybackOwner>,
    index: u32,
    placement: crate::playback::QueuePlacement,
) -> Result<bool, String> {
    let queue = Arc::clone(queue);
    queue
        .source
        .clone()
        .reply(move |owner, catalog| async move {
            let selected = owner.selected_library().ok_or("No source selected")?;
            let settings = owner.shared.settings.load();
            let Some(entry) = settings.ui.application.quick_mix.get(index as usize) else {
                return Ok(true);
            };
            let [definition] = &scoped(std::slice::from_ref(entry), &selected)[..] else {
                return Ok(true);
            };
            let tracks = catalog
                .quick_mix_press(
                    selected.music_folder_key,
                    definition,
                    owner.quick_mix_seeds(std::slice::from_ref(definition))[0],
                    now(),
                    &ReadCancellation::new(),
                )
                .await
                .map_err(|error| error.to_string())?
                .tracks;
            owner.next_quick_mix(selected.source_key, definition);
            if tracks.is_empty() {
                return Ok(true);
            }
            queue.play_selection(crate::playback::PlayRequest::from_input(
                library::QueueLoad::track_keys(tracks.to_vec(), QueueProvenance::Mix),
                placement,
            ));
            Ok(false)
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?
}

impl SourceOwner {
    /// Quick mix as its editor lists it, with the user's smart playlists by name.
    pub fn quick_mix_editor(&self) -> Receiver<Result<QuickMixEditor, String>> {
        self.reply(move |owner, catalog| async move {
            let mut playlists = catalog
                .smart_playlist_copies(None)
                .await
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|copy| (copy.object_id, copy.name))
                .collect::<Vec<_>>();
            playlists.sort_by_cached_key(|(_, name)| name.to_lowercase());
            let settings = owner.shared.settings.load();
            Ok(quick_mix_editor(
                &settings.ui.application.quick_mix,
                playlists,
            ))
        })
    }

    /// Appends a copy of the smart playlist `object_id`'s rules and name; later edits of it
    /// change nothing here. False when it is gone or Quick mix is full.
    pub fn add_quick_mix_playlist(&self, object_id: String) -> Receiver<Result<bool, String>> {
        self.reply(move |owner, catalog| async move {
            let Some(copy) = catalog
                .smart_playlist_copies(Some(std::slice::from_ref(&object_id)))
                .await
                .map_err(|error| error.to_string())?
                .pop()
            else {
                return Ok(false);
            };
            let definition =
                serde_json::from_str(&copy.definition).map_err(|error| error.to_string())?;
            owner.shared.settings.update(|stored| {
                Ok(add_quick_mix(
                    &mut stored.ui.application.quick_mix,
                    QuickMixEntry {
                        template: None,
                        playlist: Some(copy.object_id),
                        name: copy.name,
                        definition,
                    },
                ))
            })
        })
    }
}
