//! Mix: what the saved settings pick in the selected library.

use std::sync::Arc;

use library::{Catalog, MixKind, ReadCancellation, SmartPlaylistDefinition, SourceKey};
use localization::msgid;
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
            let request = mix_request(&catalog, &settings, &selected, placement).await?;
            queue_mix(&catalog, &selected, request, &queue).await
        })
        .recv()
        .await
        .map_err(|error| error.to_string())?
}

/// What the Mix settings `settings` pick in `selected`, played at `placement`.
pub async fn mix_request(
    catalog: &Catalog,
    settings: &MixSettings,
    selected: &SelectedLibrary,
    placement: crate::playback::QueuePlacement,
) -> Result<MixRequest, String> {
    let genre = saved_mix_genre(catalog, settings, selected).await?;
    let mut definition = kind_definition(settings.kind);
    definition.source_id = Some(selected.source_id.clone());
    definition.conditions.extend(filters(
        settings.min_year,
        settings.max_year,
        genre.map(|genre| genre.name),
    ));
    Ok(MixRequest {
        placement,
        requested: if settings.albums {
            settings.album_limit
        } else {
            settings.limit
        },
        criteria: library::MixCriteria {
            definition,
            albums: settings.albums,
            favorites: settings.favorites,
        },
    })
}

/// A kind as Mix offers it: its name, untranslated, and its glyph.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MixKindChoice {
    pub kind: MixKind,
    pub title: String,
    pub icon: String,
    /// Whether its rules pick favorite tracks alone: picking tracks, Favorites only is on and
    /// can't be turned off. Picking albums it still narrows to favorite albums.
    pub favorites: bool,
}

/// The kinds in the order Mix pages through them: the five it always had, then the smart
/// playlist templates it has no kind for. Favorites is Mix's Favorites only.
pub fn mix_kinds() -> Vec<MixKindChoice> {
    use MixKind as K;
    let own = |kind, title: &str, icon: &str| MixKindChoice {
        kind,
        title: title.to_owned(),
        icon: icon.to_owned(),
        favorites: kind_definition(kind).rules().any(|rule| {
            rule.field == library::SmartPlaylistRuleField::Favorite
                && rule.operator == library::SmartPlaylistRuleOperator::Is
                && rule.value == Some(library::SmartPlaylistRuleValue::Bool(true))
        }),
    };
    let templates = crate::playlists::smart_playlist_templates();
    let template = |kind, name: &str| {
        templates
            .iter()
            .find(|template| template.name == name)
            .map(|template| own(kind, template.name, template.icon))
    };
    [
        Some(own(
            K::NotPlayedRecently,
            msgid("Not played recently"),
            "rufin-leaf-fall-symbolic",
        )),
        Some(own(K::All, msgid("Random"), "rufin-dice-symbolic")),
        Some(own(K::Unplayed, msgid("Unplayed"), "rufin-leaf-symbolic")),
        Some(own(
            K::Played,
            msgid("Played"),
            "rufin-circle-check-symbolic",
        )),
        template(K::RecentlyAdded, "Recently added"),
        template(K::ThisTimeLastYear, "This time last year"),
        template(K::OnRepeat, "On repeat"),
        template(K::Rediscover, "Rediscover"),
        template(K::MostPlayed, "Most played"),
        template(K::RecentlyPlayed, "Recently played"),
        template(K::ForgottenFavorites, "Forgotten favorites"),
        template(K::NewAndUnplayed, "New and unplayed"),
        template(K::TopRated, "Top rated"),
        template(K::MostSkipped, "Most skipped"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The rules of `kind`: its template's, or its own for the kinds no template has.
fn kind_definition(kind: MixKind) -> SmartPlaylistDefinition {
    use MixKind as K;
    use library::{
        SmartPlaylistCondition as C, SmartPlaylistRule as R, SmartPlaylistRuleField as F,
        SmartPlaylistRuleOperator as O, SmartPlaylistRuleValue as V,
    };
    let rule = |field, operator, value| {
        C::Rule(R {
            field,
            operator,
            value: Some(value),
        })
    };
    let own = |conditions| SmartPlaylistDefinition {
        conditions,
        ..SmartPlaylistDefinition::default()
    };
    let template = match kind {
        K::All => "Random",
        K::Unplayed => "Never played",
        K::Played => return own(vec![rule(F::Played, O::Is, V::Bool(true))]),
        // Never played counts too.
        K::NotPlayedRecently => {
            return own(vec![rule(F::LastPlayed, O::NotInTheLast, V::Number(180))]);
        }
        K::RecentlyAdded => "Recently added",
        K::ThisTimeLastYear => "This time last year",
        K::OnRepeat => "On repeat",
        K::Rediscover => "Rediscover",
        K::MostPlayed => "Most played",
        K::RecentlyPlayed => "Recently played",
        K::ForgottenFavorites => "Forgotten favorites",
        K::NewAndUnplayed => "New and unplayed",
        K::TopRated => "Top rated",
        K::MostSkipped => "Most skipped",
    };
    crate::playlists::smart_playlist_templates()
        .into_iter()
        .find(|found| found.name == template)
        .map(|found| found.definition)
        .unwrap_or_default()
}

/// Mix's years, inclusive, and genre as rules.
fn filters(
    min_year: Option<u16>,
    max_year: Option<u16>,
    genre: Option<String>,
) -> Vec<library::SmartPlaylistCondition> {
    use library::{
        SmartPlaylistCondition as C, SmartPlaylistRule as R, SmartPlaylistRuleField as F,
        SmartPlaylistRuleOperator as O, SmartPlaylistRuleValue as V,
    };
    let rule = |field, operator, value| {
        C::Rule(R {
            field,
            operator,
            value: Some(value),
        })
    };
    let year = match (min_year.map(i64::from), max_year.map(i64::from)) {
        (Some(min), Some(max)) => Some(rule(F::Year, O::Between, V::NumberRange { min, max })),
        (Some(min), None) => Some(rule(F::Year, O::Above, V::Number(min - 1))),
        (None, Some(max)) => Some(rule(F::Year, O::Below, V::Number(max + 1))),
        (None, None) => None,
    };
    year.into_iter()
        .chain(genre.map(|genre| rule(F::Genre, O::Equals, V::Text(genre))))
        .collect()
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
    let (_, folder) = mix_library(selected);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let pick = catalog
        .mix_pick(
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
