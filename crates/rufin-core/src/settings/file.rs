//! Where the settings are kept. `rufin.toml` holds what a person chooses, laid out to be read
//! and edited by hand; `state.json` beside it holds what Rufin keeps for itself: sources and
//! logins, presets, pins, sizes, and what it last showed. Together they are one document, the
//! one a `settings.json` held before them.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use toml_edit::{DocumentMut, Item, Table};
use tracing::warn;

const STATE_FILE: &str = "state.json";
const LEGACY_FILE: &str = "settings.json";
const RETIRED_LEGACY_FILE: &str = "settings.json.bak";

const HEADER: &str = "\
# Rufin's settings. Rufin reads this file when it starts and saves only the keys you change in
# the app, so comments, order and your own edits stay as you leave them. An edit made while
# Rufin runs takes effect at its next start. A key left out takes its default.
";

/// Each choice: its place in `rufin.toml`, and its place in the settings document. A place
/// that holds a group keeps the whole group.
pub(super) const CHOICES: &[(&str, &str)] = &[
    ("general.language", "/language"),
    ("general.starting_page", "/starting_page"),
    ("general.type_to_search", "/type_to_search_enabled"),
    ("general.allow_permanent_delete", "/allow_permanent_delete"),
    ("general.new_playlist_current", "/new_playlist_current"),
    (
        "general.keep_running_after_close",
        "/keep_running_after_close",
    ),
    ("general.start_minimized", "/start_minimized"),
    ("general.tray", "/tray_enabled"),
    ("general.symbolic_tray_icon", "/symbolic_tray_icon"),
    ("general.notifications", "/notifications_enabled"),
    (
        "general.control_notifications",
        "/control_notifications_enabled",
    ),
    ("updates.automatic", "/automatic_updates_enabled"),
    (
        "updates.check_interval_hours",
        "/release_check_interval_hours",
    ),
    ("updates.notifications", "/release_notifications_enabled"),
    ("appearance.theme", "/theme_preference"),
    ("appearance.accent", "/accent_preference"),
    ("appearance.theme_accents", "/theme_accents"),
    ("appearance.reduce_motion", "/reduce_motion"),
    (
        "appearance.show_downloaded_badges",
        "/show_downloaded_badges",
    ),
    (
        "appearance.show_bottom_bar_rating",
        "/show_bottom_bar_rating",
    ),
    ("grids.favorite_badges", "/grid_favorite_badges"),
    ("grids.star_ratings", "/grid_star_ratings"),
    ("appearance.seekbar_waveform", "/seekbar_waveform_enabled"),
    ("appearance.lyrics_panel", "/lyrics_panel_visible"),
    ("appearance.visualizer_panel", "/visualizer_panel_visible"),
    ("appearance.combine_panels", "/right_panel/combined"),
    ("appearance.home_blocks", "/home_blocks"),
    ("home.size", "/home/size"),
    ("home.spacing", "/home/spacing"),
    ("home.blocks", "/home/blocks"),
    ("layout.default_profile", "/layout/default_profile"),
    ("layout.narrow_enabled", "/layout/narrow_enabled"),
    ("layout.narrow_threshold", "/layout/narrow_threshold"),
    ("layout.narrow_profile", "/layout/narrow_profile"),
    (
        "fullscreen.dynamic_background",
        "/fullscreen_dynamic_background",
    ),
    (
        "fullscreen.background_image",
        "/fullscreen_background_image",
    ),
    ("fullscreen.lyrics", "/fullscreen_lyrics_visible"),
    (
        "fullscreen.current_lyrics_line",
        "/fullscreen_current_lyrics_line_visible",
    ),
    ("fullscreen.visualizer", "/fullscreen_visualizer_visible"),
    ("library.use_sort_tags", "/use_sort_tags"),
    (
        "library.prefer_distinct_track_covers",
        "/prefer_distinct_track_covers",
    ),
    ("playback.transition_mode", "/playback/transition_mode"),
    ("playback.crossfade_seconds", "/playback/crossfade_seconds"),
    (
        "playback.skip_same_album_crossfade",
        "/playback/skip_same_album_crossfade",
    ),
    ("playback.playback_rate", "/playback/playback_rate"),
    ("playback.preserve_pitch", "/playback/preserve_pitch"),
    (
        "playback.audio_fade_on_status_change",
        "/playback/audio_fade_on_status_change",
    ),
    (
        "playback.skip_unplayable_tracks",
        "/playback/skip_unplayable_tracks",
    ),
    (
        "playback.clear_queue_includes_current",
        "/clear_queue_includes_current",
    ),
    (
        "playback.loudness_normalization",
        "/playback/loudness_normalization",
    ),
    (
        "playback.loudness_normalization_scope",
        "/playback/loudness_normalization_scope",
    ),
    (
        "playback.ebu_r128_target_lufs",
        "/playback/ebu_r128_target_lufs",
    ),
    (
        "playback.write_ebu_r128_tags",
        "/playback/write_ebu_r128_tags",
    ),
    ("playback.volume_scale", "/playback/volume_scale"),
    ("playback.stream_quality", "/playback/stream_quality"),
    (
        "playback.visualizer_fft_size",
        "/playback/visualizer_fft_size",
    ),
    (
        "playback.visualizer_ignore_volume",
        "/playback/visualizer_ignore_volume",
    ),
    ("auto_dj.refill_threshold", "/auto_dj_refill_threshold"),
    ("auto_dj.refill_count", "/auto_dj_refill_count"),
    (
        "auto_dj.override_server_recommendations",
        "/override_server_recommendations",
    ),
    ("auto_dj.similar_artists", "/auto_dj_similar_artists"),
    ("auto_dj.same_artist", "/auto_dj_same_artist"),
    ("auto_dj.skip_low_rated", "/auto_dj_skip_low_rated"),
    ("mix.albums", "/mix/albums"),
    ("mix.limit", "/mix/limit"),
    ("mix.album_limit", "/mix/album_limit"),
    ("mix.min_year", "/mix/min_year"),
    ("mix.max_year", "/mix/max_year"),
    ("mix.played_filter", "/mix/played_filter"),
    ("lyrics.external", "/external_lyrics_enabled"),
    ("lyrics.providers", "/external_lyrics_providers"),
    ("lyrics.prefer_server", "/prefer_server_lyrics"),
    (
        "lyrics.download_automatically",
        "/download_lyrics_automatically",
    ),
    ("lyrics.save_automatically", "/save_lyrics_automatically"),
    ("lyrics.save_as_sidecar", "/save_lyrics_as_sidecar"),
    ("lyrics.save_to_source", "/save_lyrics_to_source"),
    ("lyrics.karaoke_mode", "/karaoke_mode"),
    ("lyrics.show_romanization", "/show_romanization"),
    ("lyrics.show_furigana", "/show_furigana"),
    ("lyrics.prefer_translations", "/prefer_translations"),
    (
        "lyrics.preferred_translation_language",
        "/preferred_translation_language",
    ),
    ("lyrics.font_family", "/lyrics_font_family"),
    ("lyrics.font_size", "/lyrics_font_size"),
    ("lyrics.highlight_color", "/lyrics_highlight_color"),
    (
        "lyrics.fullscreen_font_family",
        "/fullscreen_lyrics_font_family",
    ),
    (
        "lyrics.fullscreen_font_size",
        "/fullscreen_lyrics_font_size",
    ),
    ("metadata.external_lookups", "/external_metadata_enabled"),
    ("metadata.cover_providers", "/cover_providers"),
    ("metadata.artist_image_providers", "/artist_image_providers"),
    (
        "metadata.download_artwork_automatically",
        "/download_artwork_automatically",
    ),
    ("metadata.lastfm_api_key", "/lastfm_api_key"),
    ("metadata.fanart_api_key", "/fanart_api_key"),
    ("metadata.acoustid_client_key", "/acoustid_client_key"),
    ("metadata.site_links", "/external_site_links"),
    ("privacy.private_mode", "/private_mode"),
    ("privacy.cast_proxy", "/cast_proxy_enabled"),
    ("privacy.cast_network_interface", "/cast_network_interface"),
    ("privacy.secret_storage", "/secret_storage_mode"),
    ("discord.enabled", "/discord_presence_enabled"),
    ("discord.client_id", "/discord_client_id"),
    ("discord.display_type", "/discord_display_type"),
    ("discord.link_type", "/discord_link_type"),
    ("discord.links", "/discord_links"),
    ("discord.show_as_listening", "/discord_show_as_listening"),
    ("discord.show_paused", "/discord_show_paused"),
    ("web_controller.enabled", "/web_controller/enabled"),
    ("web_controller.address", "/web_controller/address"),
    ("web_controller.port", "/web_controller/port"),
    ("context_menu", "/context_menu"),
    ("activity_overview.tracks", "/activity_overview/tracks"),
    ("activity_overview.albums", "/activity_overview/albums"),
    ("activity_overview.artists", "/activity_overview/artists"),
    ("activity_overview.genres", "/activity_overview/genres"),
    (
        "activity_overview.result_count",
        "/activity_overview/result_count",
    ),
    (
        "activity_overview.show_comparison",
        "/activity_overview/show_comparison",
    ),
    (
        "activity_overview.show_rufin_in_headline",
        "/activity_overview/show_rufin_in_headline",
    ),
    (
        "activity_overview.autoplay_first_track",
        "/activity_overview/autoplay_first_track",
    ),
    (
        "activity_overview.background_image",
        "/activity_overview/background_image",
    ),
    (
        "activity_overview.dynamic_background",
        "/activity_overview/dynamic_background",
    ),
    (
        "activity_overview.monthly_enabled",
        "/activity_overview/monthly_enabled",
    ),
    (
        "activity_overview.monthly_days_before_end",
        "/activity_overview/monthly_days_before_end",
    ),
    (
        "activity_overview.monthly_days_after_end",
        "/activity_overview/monthly_days_after_end",
    ),
    (
        "activity_overview.yearly_enabled",
        "/activity_overview/yearly_enabled",
    ),
    (
        "activity_overview.yearly_days_before_end",
        "/activity_overview/yearly_days_before_end",
    ),
    (
        "activity_overview.yearly_days_after_end",
        "/activity_overview/yearly_days_after_end",
    ),
    ("backup.enabled", "/backup/enabled"),
    ("backup.encrypt", "/backup/encrypt"),
    ("backup.destination_uri", "/backup/destination_uri"),
    ("backup.retention_count", "/backup/retention_count"),
    ("backup.contents", "/backup/contents"),
    ("backup.frequency", "/backup/schedule/frequency"),
    ("backup.hour", "/backup/schedule/hour"),
    ("backup.weekday", "/backup/schedule/weekday"),
    (
        "android.download_on_mobile_data",
        "/download_on_mobile_data",
    ),
    (
        "android.mobile_data_stream_quality",
        "/mobile_data_stream_quality",
    ),
];

/// What stays in `state.json`, by where it is in the settings document: every part of the
/// document is a choice above or state here, so a new setting is placed on purpose.
#[cfg(test)]
const STATE: &[&str] = &[
    "/activity_overview/opened_month",
    "/activity_overview/opened_year",
    "/auto_dj_enabled",
    "/backup/schedule/last_successful_at",
    "/backup/schedule/retention",
    "/backup/schedule/schedule_id",
    "/backup_notice_dismissed",
    "/cached_lyrics_sources",
    "/downloads",
    "/equalizer",
    "/jellyfin_device_id",
    "/layout/preferred_left_sidebar_width",
    "/layout/preferred_right_sidebar_width",
    "/library_lists",
    "/lyrics_cache_prompt_dismissed",
    "/lyrics_provider_settings_version",
    "/playback/audio_output",
    "/playback/muted",
    "/playback/volume",
    "/queue_lyrics_height",
    "/mix/genre",
    "/recent_search_results",
    "/release_notification_seen_version",
    "/repeat_mode",
    "/right_panel/lyrics_height",
    "/right_panel/order",
    "/right_panel/queue_visible",
    "/right_panel/visualizer_height",
    "/scrobbling",
    "/scrobbling_credentials",
    "/scrobbling_secrets_present",
    "/secret_scope_id",
    "/shared_settings",
    "/shuffle_enabled",
    "/sidebar",
    "/sources",
    "/suppressed_auto_lyrics_track_ids",
    "/visualizer",
    "/visualizer_preset_bar_counts_updated",
    "/window_height",
    "/window_width",
];

/// The settings document as it is saved: the choices of `path`, a `rufin.toml`, laid over the
/// state beside it. None where nothing is saved yet. A `settings.json` from before the two
/// files is read while neither exists.
pub(super) fn read(path: &Path) -> Result<Option<Value>, String> {
    let choices = read_text(path)?;
    let state = read_text(&sibling(path, STATE_FILE))?;
    if choices.is_none() && state.is_none() {
        return read_text(&sibling(path, LEGACY_FILE))?
            .map(|legacy| serde_json::from_str(&legacy).map_err(|error| error.to_string()))
            .transpose();
    }
    let mut document = match state {
        Some(state) => serde_json::from_str(&state).map_err(|error| error.to_string())?,
        None => Value::Object(Map::new()),
    };
    if let Some(choices) = choices {
        let choices = choices
            .parse::<DocumentMut>()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        for (place, pointer) in CHOICES {
            if let Some(value) = item_at(choices.as_table(), place).and_then(item_json) {
                put(&mut document, pointer, value);
            }
        }
    }
    Ok(Some(document))
}

/// Saves `document`: its choices into `path`, changing only the keys whose value changed, and
/// the rest into the state beside it. The `settings.json` they replace is set aside once both
/// are written.
pub(super) fn write(path: &Path, document: &Value) -> Result<(), String> {
    let saved = read_text(path)?;
    let mut choices = saved
        .as_deref()
        .unwrap_or_default()
        .parse::<DocumentMut>()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut state = document.clone();
    for (place, pointer) in CHOICES {
        set_item(choices.as_table_mut(), place, take(&mut state, pointer));
    }
    let mut json = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    json.push(b'\n');
    super::write_private(&sibling(path, STATE_FILE), &json)?;
    // A new file opens with how it is read; after that the note is the user's to keep.
    let choices = match saved {
        Some(_) => choices.to_string(),
        None => format!("{HEADER}\n{choices}"),
    };
    if saved.as_deref() != Some(choices.as_str()) {
        super::write_private(path, choices.as_bytes())?;
    }
    let legacy = sibling(path, LEGACY_FILE);
    if legacy.is_file()
        && let Err(error) = fs::rename(&legacy, sibling(path, RETIRED_LEGACY_FILE))
    {
        warn!(%error, path = %legacy.display(), "could not set the earlier settings file aside");
    }
    Ok(())
}

/// Saves `document` as the whole of what is kept, dropping what the files held before.
pub(super) fn replace(path: &Path, document: &Value) -> Result<(), String> {
    for stale in [path.to_path_buf(), sibling(path, STATE_FILE)] {
        match fs::remove_file(&stale) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    write(path, document)
}

/// The files that hold the settings whose choices are in `path`.
pub(crate) fn files(path: &Path) -> [PathBuf; 2] {
    [path.to_path_buf(), sibling(path, STATE_FILE)]
}

fn sibling(path: &Path, name: &str) -> PathBuf {
    path.with_file_name(name)
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Puts `value` at `pointer` in `document`, making the groups on the way.
fn put(document: &mut Value, pointer: &str, value: Value) {
    let mut keys = pointer.split('/').skip(1).peekable();
    let mut parent = document;
    while let Some(key) = keys.next() {
        if !parent.is_object() {
            *parent = Value::Object(Map::new());
        }
        let Some(fields) = parent.as_object_mut() else {
            return;
        };
        if keys.peek().is_none() {
            fields.insert(key.to_owned(), value);
            return;
        }
        parent = fields
            .entry(key.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Takes what is at `pointer` out of `document`, and a group it leaves empty with it.
fn take(document: &mut Value, pointer: &str) -> Option<Value> {
    let (parent, key) = pointer.rsplit_once('/')?;
    let fields = match parent {
        "" => document.as_object_mut()?,
        parent => document.pointer_mut(parent)?.as_object_mut()?,
    };
    let value = fields.remove(key);
    if fields.is_empty() && !parent.is_empty() {
        take(document, parent);
    }
    value.filter(|value| !value.is_null())
}

fn item_at<'a>(table: &'a Table, place: &str) -> Option<&'a Item> {
    let mut keys = place.split('.');
    let mut item = table.get(keys.next()?)?;
    for key in keys {
        item = item.as_table_like()?.get(key)?;
    }
    Some(item)
}

/// Makes the item at `place` hold `value`, or takes it out for None. An item that already
/// says the same is left as it is written.
fn set_item(table: &mut Table, place: &str, value: Option<Value>) {
    match place.split_once('.') {
        Some((group, rest)) => {
            if value.is_none() && !table.contains_key(group) {
                return;
            }
            let item = table
                .entry(group)
                .or_insert_with(|| Item::Table(Table::new()));
            if let Some(group) = item.as_table_mut() {
                set_item(group, rest, value);
            }
        }
        None => match value {
            None => {
                table.remove(place);
            }
            Some(value) => set_value(table, place, &value),
        },
    }
}

fn set_value(table: &mut Table, key: &str, value: &Value) {
    if table
        .get(key)
        .and_then(item_json)
        .as_ref()
        .is_some_and(|saved| same(saved, value))
    {
        return;
    }
    match value {
        // A group is a table of its own, each of its keys kept or changed in turn.
        Value::Object(fields) => {
            if !table.get(key).is_some_and(Item::is_table) {
                table.insert(key, Item::Table(Table::new()));
            }
            let Some(group) = table.get_mut(key).and_then(Item::as_table_mut) else {
                return;
            };
            let gone = group
                .iter()
                .map(|(key, _)| key.to_owned())
                .filter(|key| fields.get(key).is_none_or(Value::is_null))
                .collect::<Vec<_>>();
            for key in gone {
                group.remove(&key);
            }
            for (key, value) in fields.iter().filter(|(_, value)| !value.is_null()) {
                set_value(group, key, value);
            }
        }
        value => {
            let Some(mut next) = toml_value(value) else {
                return;
            };
            // What was written around the value, such as a comment after it, stays.
            if let Some(saved) = table.get(key).and_then(Item::as_value) {
                *next.decor_mut() = saved.decor().clone();
            }
            table.insert(key, Item::Value(next));
        }
    }
}

/// Whether a saved value says what `next` says: a whole number is the same written with or
/// without its fraction, and a missing key is the same as none.
fn same(saved: &Value, next: &Value) -> bool {
    match (saved, next) {
        (Value::Number(saved), Value::Number(next)) => saved.as_f64() == next.as_f64(),
        (Value::Array(saved), Value::Array(next)) => {
            saved.len() == next.len()
                && saved
                    .iter()
                    .zip(next)
                    .all(|(saved, next)| same(saved, next))
        }
        (Value::Object(saved), Value::Object(next)) => {
            let known = |fields: &Map<String, Value>| {
                fields.values().filter(|value| !value.is_null()).count()
            };
            known(saved) == known(next)
                && next
                    .iter()
                    .filter(|(_, value)| !value.is_null())
                    .all(|(key, next)| saved.get(key).is_some_and(|saved| same(saved, next)))
        }
        (saved, next) => saved == next,
    }
}

fn toml_value(value: &Value) -> Option<toml_edit::Value> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(value) => (*value).into(),
        Value::Number(number) => match number.as_i64() {
            Some(whole) => whole.into(),
            None => number.as_f64()?.into(),
        },
        Value::String(text) => text.as_str().into(),
        Value::Array(values) => {
            toml_edit::Value::Array(values.iter().filter_map(toml_value).collect())
        }
        Value::Object(fields) => toml_edit::Value::InlineTable(
            fields
                .iter()
                .filter_map(|(key, value)| Some((key.as_str(), toml_value(value)?)))
                .collect(),
        ),
    })
}

fn item_json(item: &Item) -> Option<Value> {
    match item {
        Item::None => None,
        Item::Value(value) => Some(value_json(value)),
        Item::Table(table) => Some(Value::Object(
            table
                .iter()
                .filter_map(|(key, item)| Some((key.to_owned(), item_json(item)?)))
                .collect(),
        )),
        Item::ArrayOfTables(tables) => Some(Value::Array(
            tables
                .iter()
                .filter_map(|table| item_json(&Item::Table(table.clone())))
                .collect(),
        )),
    }
}

fn value_json(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(text) => Value::String(text.value().clone()),
        toml_edit::Value::Integer(whole) => Value::from(*whole.value()),
        toml_edit::Value::Float(number) => Value::from(*number.value()),
        toml_edit::Value::Boolean(flag) => Value::Bool(*flag.value()),
        toml_edit::Value::Datetime(time) => Value::String(time.value().to_string()),
        toml_edit::Value::Array(values) => Value::Array(values.iter().map(value_json).collect()),
        toml_edit::Value::InlineTable(table) => Value::Object(
            table
                .iter()
                .map(|(key, value)| (key.to_owned(), value_json(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(value: &Value, pointer: String, found: &mut Vec<String>) {
        match value.as_object().filter(|fields| !fields.is_empty()) {
            Some(fields) => {
                for (key, value) in fields {
                    leaves(value, format!("{pointer}/{key}"), found);
                }
            }
            None => found.push(pointer),
        }
    }

    fn within(pointer: &str, place: &str) -> bool {
        pointer == place
            || pointer
                .strip_prefix(place)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    #[test]
    fn every_setting_is_a_choice_or_state() {
        let document = serde_json::to_value(super::super::StoredSettings::default()).unwrap();
        let mut found = Vec::new();
        leaves(&document, String::new(), &mut found);
        let misplaced = found
            .into_iter()
            .filter(|pointer| {
                let choice = CHOICES.iter().any(|(_, place)| within(pointer, place));
                let state = STATE.iter().any(|place| within(pointer, place));
                choice == state
            })
            .collect::<Vec<_>>();
        assert!(
            misplaced.is_empty(),
            "in neither or both of rufin.toml and state.json: {misplaced:?}"
        );
    }

    #[test]
    fn a_save_keeps_what_was_written_by_hand() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rufin.toml");
        let mut document = serde_json::json!({
            "auto_dj_refill_count": 5,
            "playback": {"playback_rate": 1.0, "volume": 0.5},
            "window_width": 900,
        });
        write(&path, &document).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("# Rufin's settings."));
        // A comment, a whole number for a rate, and a key this version does not know.
        let edited = written
            .replace("refill_count = 5", "refill_count = 5 # as many as a side")
            .replace("playback_rate = 1.0", "playback_rate = 1")
            + "\n[later]\nkey = true\n";
        fs::write(&path, &edited).unwrap();
        document["window_width"] = 1000.into();
        write(&path, &document).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        document["auto_dj_refill_count"] = 8.into();
        write(&path, &document).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("refill_count = 8 # as many as a side"));
        assert!(saved.contains("playback_rate = 1\n"));
        assert!(saved.contains("[later]\nkey = true"));
        let read = read(&path).unwrap().unwrap();
        assert_eq!(read["auto_dj_refill_count"], 8);
        assert_eq!(read["window_width"], 1000);
        assert_eq!(read["playback"]["volume"], 0.5);
    }
}
