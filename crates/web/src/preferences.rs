use super::*;
use rufin_core::runtime::CacheKind;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/preferences", get(read).post(write))
        .route("/api/preferences/storage", get(storage).post(clear))
}

/// The settings the browser's Preferences show, as JSON pointers into the stored settings. The
/// rest stay the host's: its window, tray, controller and secrets.
const SETTINGS: &[&str] = &[
    "/release_check_interval_hours",
    "/allow_permanent_delete",
    "/prefer_distinct_track_covers",
    "/use_sort_tags",
    "/external_metadata_enabled",
    "/cover_providers",
    "/artist_image_providers",
    "/fanart_api_key",
    "/external_lyrics_enabled",
    "/prefer_server_lyrics",
    "/external_lyrics_providers",
    "/reduce_motion",
    "/layout/default_profile/left_sidebar",
    "/layout/default_profile/right_sidebar",
    "/layout/narrow_enabled",
    "/layout/narrow_threshold",
    "/layout/narrow_profile/left_sidebar",
    "/layout/narrow_profile/right_sidebar",
    "/lyrics_panel_visible",
    "/visualizer_panel_visible",
    "/right_panel/combined",
    "/right_panel/queue_visible",
    "/show_bottom_bar_rating",
    "/seekbar_waveform_enabled",
    "/sidebar/pins_visible",
    "/context_menu/items",
    "/context_menu/rating_visible",
    "/theme_preference",
    "/accent_preference",
    "/discord_presence_enabled",
    "/discord_display_type",
    "/discord_links",
    "/discord_show_paused",
    "/discord_show_as_listening",
    "/external_site_links/enabled",
    "/external_site_links/lastfm",
    "/external_site_links/musicbrainz",
    "/external_site_links/server",
    "/playback/transition_mode",
    "/playback/crossfade_seconds",
    "/playback/skip_same_album_crossfade",
    "/playback/playback_rate",
    "/playback/preserve_pitch",
    "/playback/audio_fade_on_status_change",
    "/playback/skip_unplayable_tracks",
    "/playback/loudness_normalization",
    "/playback/loudness_normalization_scope",
    "/playback/ebu_r128_target_lufs",
    "/playback/write_ebu_r128_tags",
    "/playback/volume_scale",
    "/playback/stream_quality",
    "/auto_dj_refill_threshold",
    "/clear_queue_includes_current",
    "/fullscreen_lyrics_visible",
    "/fullscreen_visualizer_visible",
    "/fullscreen_dynamic_background",
    "/fullscreen_background_image",
    "/show_downloaded_badges",
    "/download_artwork_automatically",
    "/download_lyrics_automatically",
    "/private_mode",
    "/cast_proxy_enabled",
];

fn values(products: &ProductHandles) -> Result<Value, Error> {
    let settings = products.settings.load();
    let stored = serde_json::to_value(&*settings).map_err(internal)?;
    let mut values = serde_json::Map::new();
    for path in SETTINGS {
        if let Some(value) = stored.pointer(path) {
            values.insert((*path).to_owned(), value.clone());
        }
    }
    let routes = settings
        .sidebar
        .route_items
        .iter()
        .map(|entry| {
            let route = rufin_core::route::sidebar_route_descriptor(entry.item);
            let subtitle = if entry.item == rufin_core::settings::SidebarRouteItem::Moods {
                localization::tr("Files need Mood/BPM tags written on them. Not supported for Jellyfin")
            } else {
                String::new()
            };
            json!({"item": entry.item, "title": localization::tr(route.title), "subtitle": subtitle, "icon": route.icon_name, "visible": entry.visible})
        })
        .collect::<Vec<_>>();
    let (themes, theme_errors) = themes(&settings);
    drop(settings);
    let blocks = products
        .settings
        .home_block_rows()
        .into_iter()
        .map(|row| json!({"block": row.block, "title": localization::tr(&row.title), "visible": row.visible, "can_toggle": row.can_toggle}))
        .collect::<Vec<_>>();
    let lyrics = rufin_core::settings::ExternalLyricsProvider::all()
        .into_iter()
        .map(|provider| provider_json(json!(provider.key()), localization::tr(provider.title())))
        .collect::<Vec<_>>();
    let providers = json!({
        "/cover_providers": rufin_core::settings::CoverProvider::all().into_iter().map(|provider| provider_json(json!(provider), String::new())).collect::<Vec<_>>(),
        "/artist_image_providers": rufin_core::settings::ArtistImageProvider::all().into_iter().map(|provider| provider_json(json!(provider), String::new())).collect::<Vec<_>>(),
        "/external_lyrics_providers": lyrics,
    });
    Ok(
        json!({"values": values, "routes": routes, "home_blocks": blocks, "providers": providers, "themes": themes, "theme_errors": theme_errors}),
    )
}

/// A provider of an ordered list by its stored key, with its name and what it finds, as GTK's
/// Integrations rows show them.
fn provider_json(key: Value, title: String) -> Value {
    let (name, subtitle) = match key.as_str().unwrap_or_default() {
        "musicbrainz" => ("MusicBrainz", String::new()),
        "lastfm" => ("Last.fm", String::new()),
        "fanart" => ("fanart.tv", String::new()),
        "commons" => ("Wikimedia Commons", String::new()),
        "lrclib" | "simpmusic" => ("", localization::tr("Synced and plain lyrics")),
        "netease" => ("", localization::tr("Synced lyrics, some word by word")),
        "genius" => ("", localization::tr("Plain lyrics")),
        _ => ("", String::new()),
    };
    let title = if title.is_empty() {
        name.to_owned()
    } else {
        title
    };
    json!({"key": key, "title": title, "subtitle": subtitle})
}

/// Every theme GTK's Theme page shows, System first: the colors its preview draws and the
/// accents it offers. Custom themes are read from the folder each time, so new files show up.
fn themes(settings: &rufin_core::settings::Settings) -> (Vec<Value>, Vec<String>) {
    use rufin_core::settings::{AccentPreference, ThemePreference};
    let appearance = &settings.appearance;
    let directory = rufin_core::paths::roots().config_dir().join("themes");
    let mut themes = rufin_core::themes::builtins();
    let (custom, errors) = rufin_core::themes::custom(&directory);
    themes.extend(custom);
    let standard_accents = AccentPreference::ALL
        .iter()
        .map(|accent| json!({"name": accent, "title": localization::tr(&format!("{accent:?}")), "color": accent.color().unwrap_or("#3584e4")}))
        .collect::<Vec<_>>();
    let preview = |theme: &rufin_core::themes::Theme| {
        let colors = theme.colors(
            &themes,
            appearance.theme_accents.get(&theme.id).map(String::as_str),
        );
        let color = |name: &str| colors.get(name).cloned().unwrap_or_default();
        json!({
            "window": color("window-bg-color"), "sidebar": color("sidebar-bg-color"),
            "view": color("view-bg-color"), "secondary": color("secondary-sidebar-bg-color"),
            "text": color("view-fg-color"),
            "accent": theme.accent(appearance.theme_accents.get(&theme.id).map(String::as_str))
                .map(|accent| accent.color.clone())
                .or_else(|| appearance.accent_preference.color().map(String::from)),
        })
    };
    let mut tiles = vec![json!({
        "preference": ThemePreference::System,
        "name": localization::tr("System"),
        "light": preview(&themes[0]), "dark": preview(&themes[1]),
        "accents": standard_accents, "accent": appearance.accent_preference,
    })];
    for (index, theme) in themes.iter().enumerate() {
        let preference = match index {
            0 => ThemePreference::Light,
            1 => ThemePreference::Dark,
            _ => ThemePreference::Named(theme.id.clone()),
        };
        let (accents, accent) = if index < 2 {
            (
                standard_accents.clone(),
                json!(appearance.accent_preference),
            )
        } else if theme.accents.len() > 1 {
            let accents = theme.accents.iter().map(|accent| json!({"name": accent.name, "title": accent.name, "color": accent.color})).collect();
            (
                accents,
                json!(
                    theme
                        .accent(appearance.theme_accents.get(&theme.id).map(String::as_str))
                        .map(|accent| &accent.name)
                ),
            )
        } else {
            (Vec::new(), Value::Null)
        };
        let name = match index {
            0 => localization::tr("Light"),
            1 => localization::tr("Dark"),
            _ => theme.name.clone(),
        };
        tiles.push(json!({"preference": preference, "id": theme.id, "name": name, "preview": preview(theme), "accents": accents, "accent": accent}));
    }
    (tiles, errors)
}

async fn read(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(StatusCode::OK, values(&products)?))
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Edit {
    /// One of `SETTINGS`, replaced with `value`.
    Set { path: String, value: Value },
    /// A sidebar entry shown or hidden, moved by `delta`, or dropped before or after `target`.
    Route {
        item: rufin_core::settings::SidebarRouteItem,
        visible: Option<bool>,
        #[serde(default)]
        delta: isize,
        target: Option<rufin_core::settings::SidebarRouteItem>,
        #[serde(default)]
        after: bool,
    },
    /// A theme from the Theme page, with the accent its tile picked when it has its own.
    Theme {
        theme: rufin_core::settings::ThemePreference,
        accent: Option<String>,
    },
    HomeBlock {
        block: library::HomeBlockKind,
        visible: Option<bool>,
        #[serde(default)]
        delta: i32,
        target: Option<library::HomeBlockKind>,
        #[serde(default)]
        after: bool,
    },
}

async fn write(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    let edit: Edit = body(request).await?;
    let settings = products.settings.clone();
    let artwork = products.artwork.clone();
    tokio::task::spawn_blocking(move || match edit {
        Edit::Set { path, value } => {
            if !SETTINGS.contains(&path.as_str()) {
                return Err(bad_request(format!("{path} is not a preference")));
            }
            // Covers and artist images still missing are asked for again, as GTK does.
            let retry_artwork = match path.as_str() {
                "/external_metadata_enabled" => value == json!(true),
                "/cover_providers" | "/artist_image_providers" | "/fanart_api_key" => true,
                _ => false,
            };
            settings
                .update_preferences(|ui| {
                    if path == "/playback/volume_scale" {
                        let scale = serde_json::from_value(value).map_err(|e| e.to_string())?;
                        ui.playback.set_volume_scale_preserving_gain(scale);
                        return Ok(());
                    }
                    let mut stored = serde_json::to_value(&*ui).map_err(|e| e.to_string())?;
                    *stored.pointer_mut(&path).ok_or("unknown preference")? = value;
                    *ui = serde_json::from_value(stored).map_err(|e| e.to_string())?;
                    Ok(())
                })
                .map_err(bad_request)?;
            if retry_artwork && let Err(error) = artwork.retry_external() {
                tracing::warn!(%error, path, "failed to retry external artwork");
            }
            Ok(())
        }
        Edit::Route {
            item,
            visible,
            delta,
            target,
            after,
        } => {
            if let Some(visible) = visible {
                settings
                    .set_route_visible(item, visible)
                    .map_err(bad_request)?;
            }
            if delta != 0 {
                settings.move_route(item, delta).map_err(bad_request)?;
            }
            if let Some(target) = target {
                settings
                    .update_preferences(|ui| {
                        ui.sidebar.reorder_route(item, target, after);
                        Ok(())
                    })
                    .map_err(bad_request)?;
            }
            Ok(())
        }
        Edit::Theme { theme, accent } => settings
            .update_preferences(|ui| {
                if let (rufin_core::settings::ThemePreference::Named(id), Some(accent)) =
                    (&theme, accent)
                {
                    ui.appearance.theme_accents.insert(id.clone(), accent);
                }
                ui.appearance.theme_preference = theme;
                Ok(())
            })
            .map_err(bad_request),
        Edit::HomeBlock {
            block,
            visible,
            delta,
            target,
            after,
        } => {
            if let Some(visible) = visible {
                settings
                    .set_home_block_visible(block, visible)
                    .map_err(bad_request)?;
            }
            if delta != 0 {
                settings
                    .move_home_block(block, delta)
                    .map_err(bad_request)?;
            }
            if let Some(target) = target {
                settings
                    .reorder_home_block(block, target, after)
                    .map_err(bad_request)?;
            }
            Ok(())
        }
    })
    .await
    .map_err(internal)??;
    Ok(json_response(StatusCode::OK, values(&products)?))
}

const CACHES: [(&str, CacheKind); 7] = [
    ("waveforms", CacheKind::Waveforms),
    ("loudness", CacheKind::Loudness),
    ("fingerprints", CacheKind::Fingerprints),
    ("metadata_lookups", CacheKind::MetadataLookups),
    ("artwork", CacheKind::Artwork),
    ("lyrics", CacheKind::Lyrics),
    ("japanese_dictionary", CacheKind::JapaneseDictionary),
];

async fn summary(products: &ProductHandles, clear: Option<CacheKind>) -> Value {
    let summary = products.storage_summary(clear).await;
    json!({
        "total_bytes": summary.total_bytes,
        "errors": summary.errors,
        "caches": summary.caches.iter().filter_map(|cache| {
            let name = CACHES.iter().find(|(_, kind)| *kind == cache.kind)?.0;
            Some(json!({"kind": name, "entries": cache.usage.as_ref().map(|u| u.entries), "bytes": cache.usage.as_ref().map(|u| u.bytes)}))
        }).collect::<Vec<_>>(),
    })
}

async fn storage(State(products): State<ProductHandles>) -> Result<Response<Body>, Error> {
    Ok(json_response(
        StatusCode::OK,
        summary(&products, None).await,
    ))
}

async fn clear(
    State(products): State<ProductHandles>,
    request: Request<Body>,
) -> Result<Response<Body>, Error> {
    #[derive(Deserialize)]
    struct Input {
        kind: String,
    }
    let input: Input = body(request).await?;
    let kind = CACHES
        .iter()
        .find(|(name, _)| *name == input.kind)
        .ok_or_else(|| bad_request("Unknown cache"))?
        .1;
    Ok(json_response(
        StatusCode::OK,
        summary(&products, Some(kind)).await,
    ))
}
