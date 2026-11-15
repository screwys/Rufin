//! Mix: what the saved settings pick in the selected library.

use std::sync::Arc;

use library::{Catalog, ReadCancellation, SourceKey};
use playback::QueueProvenance;

use crate::playback::{MixRequest, random_u64};
use crate::runtime::SelectedLibrary;
use crate::settings::MixSettings;

/// Plays the saved Mix settings in the selected library. True means nothing matched.
pub async fn play_saved_mix(
    queue: &Arc<crate::playback::PlaybackOwner>,
    settings: MixSettings,
    placement: crate::playback::QueuePlacement,
) -> Result<bool, String> {
    let queue = Arc::clone(queue);
    queue
        .source
        .clone()
        .reply(move |owner, catalog| async move {
            let selected = owner.selected_library().ok_or("No source selected")?;
            let genre = saved_mix_genre(&catalog, &settings, &selected).await?;
            let request = MixRequest {
                placement,
                requested: if settings.albums {
                    settings.album_limit
                } else {
                    settings.limit
                },
                criteria: library::MixCriteria {
                    min_year: settings.min_year.map(i64::from),
                    max_year: settings.max_year.map(i64::from),
                    genre: genre.map(|genre| library::GenreKey::from_raw(genre.key)),
                    played: settings.played_filter,
                    albums: settings.albums,
                },
            };
            queue_mix(&catalog, &selected, request, &queue).await
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?
}

/// The source and music folder Mix reads in `selected`: it is query-built, so the
/// folder narrows it (decisions.md, 2026-11-06).
fn mix_library(selected: &SelectedLibrary) -> (SourceKey, Option<library::FolderKey>) {
    (selected.source_key, selected.music_folder_key)
}

/// The genres Mix offers in `selected` whose names hold `filter`, and how many there
/// are.
pub async fn mix_genres(
    catalog: &Catalog,
    selected: &SelectedLibrary,
    filter: &str,
    range: std::ops::Range<usize>,
    cancellation: &ReadCancellation,
) -> library::LibraryResult<(usize, Vec<library::GenreChoice>)> {
    let (source, folder) = mix_library(selected);
    catalog
        .genre_choices(source, folder, filter, range, cancellation)
        .await
}

/// Finds the saved Mix genre when it was saved for `selected`: a source and one of
/// its music folders.
pub async fn saved_mix_genre(
    catalog: &Catalog,
    settings: &MixSettings,
    selected: &SelectedLibrary,
) -> Result<Option<library::TagRow>, String> {
    let Some(object_id) = settings.selected_genre_id(
        &selected.source_id,
        selected.music_folder_object_id.as_deref(),
    ) else {
        return Ok(None);
    };
    let cancellation = ReadCancellation::new();
    let Some(key) = catalog
        .catalog_keys(
            library::CatalogKind::Genre,
            Some(selected.source_key),
            &[object_id.to_owned()],
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .pop()
    else {
        return Ok(None);
    };
    Ok(catalog
        .rows_of(
            &[library::Item::Genre(library::GenreKey::from_raw(key))],
            false,
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .find_map(|row| match row {
            library::Row::Tag(tag) => Some(tag),
            _ => None,
        }))
}

/// Plays what `request` picks at random in `selected`. True means nothing matched.
pub async fn queue_mix(
    catalog: &Catalog,
    selected: &SelectedLibrary,
    request: MixRequest,
    queue: &crate::playback::PlaybackOwner,
) -> Result<bool, String> {
    let cancellation = ReadCancellation::new();
    let (source, folder) = mix_library(selected);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let pick = catalog
        .mix_pick(
            source,
            folder,
            &request.criteria,
            request.requested,
            now,
            random_u64() as i64,
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let load = match pick {
        library::MixPick::Tracks(tracks) if !tracks.is_empty() => {
            library::QueueLoad::track_keys(tracks, QueueProvenance::Mix)
        }
        // Each album whole, in its own order, as its page plays it.
        library::MixPick::Albums(albums) if !albums.is_empty() => library::QueueLoad::Tracks(
            albums
                .into_iter()
                .map(|album| {
                    (
                        library::MemberRead::new(library::Collection::Album(album)).selection(),
                        QueueProvenance::Mix,
                    )
                })
                .collect(),
        ),
        _ => return Ok(true),
    };
    queue.play_selection(crate::playback::PlayRequest::from_input(
        load,
        request.placement,
    ));
    Ok(false)
}
