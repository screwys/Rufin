//! Listening report formatting and screen-independent PNG export.
use artwork::{Artwork, ArtworkBinding};
use library::{ActivityOverview, CalendarActivityPeriod};
use localization::{msgid, tr_with};

use crate::settings::app::ActivityOverviewSettings;

mod render;
pub use render::ReportAppearance;

/// Render the selected report using the same preferences as the overview.
/// Hosts supply their resolved theme colors; no window or graphics context is needed.
pub async fn export_png(
    data: ActivityOverview,
    period: CalendarActivityPeriod,
    settings: ActivityOverviewSettings,
    appearance: ReportAppearance,
    artwork: Artwork,
) -> Result<Vec<u8>, String> {
    let limit = settings.result_count;
    let mut bindings = Vec::new();
    if settings.dynamic_background {
        bindings.extend(
            data.tracks
                .first()
                .and_then(|row| row.artwork_binding.clone()),
        );
    }
    if settings.tracks {
        bindings.extend(
            data.tracks
                .iter()
                .take(limit)
                .filter_map(|row| row.artwork_binding.clone()),
        );
    }
    if settings.artists {
        bindings.extend(
            data.artists
                .iter()
                .take(limit)
                .filter_map(|row| row.artwork_binding.clone()),
        );
    }
    if settings.albums {
        bindings.extend(
            data.albums
                .iter()
                .take(limit)
                .filter_map(|row| row.artwork_binding.clone()),
        );
    }
    bindings.sort();
    bindings.dedup();
    tokio::task::spawn_blocking(move || {
        let images = bindings
            .into_iter()
            .filter_map(|binding| {
                match artwork.cached_image(&ArtworkBinding::opaque(&binding), 512) {
                    Ok(image) => image.map(|image| (binding, image)),
                    Err(error) => {
                        tracing::debug!(%error, "Could not read report artwork");
                        None
                    }
                }
            })
            .collect();
        render::png(&data, period, &settings, &appearance, images)
    })
    .await
    .map_err(|error| error.to_string())?
}

pub fn period_key(period: CalendarActivityPeriod) -> String {
    match period {
        CalendarActivityPeriod::Month { year, month } => format!("{year:04}-{month:02}"),
        CalendarActivityPeriod::Year(year) => year.to_string(),
        CalendarActivityPeriod::Lifetime => String::new(),
    }
}

pub fn period_label(period: CalendarActivityPeriod) -> String {
    match period {
        CalendarActivityPeriod::Month { year, month } => {
            let message = match month {
                1 => msgid("January {year}"),
                2 => msgid("February {year}"),
                3 => msgid("March {year}"),
                4 => msgid("April {year}"),
                5 => msgid("May {year}"),
                6 => msgid("June {year}"),
                7 => msgid("July {year}"),
                8 => msgid("August {year}"),
                9 => msgid("September {year}"),
                10 => msgid("October {year}"),
                11 => msgid("November {year}"),
                12 => msgid("December {year}"),
                _ => return period_key(period),
            };
            tr_with(message, &[("year", &year.to_string())])
        }
        _ => period_key(period),
    }
}

pub fn percentage_change(current: i64, previous: i64) -> String {
    if previous == 0 {
        return String::new();
    }
    format!(
        "({:+.0}%)",
        (current as f64 - previous as f64) * 100.0 / previous as f64
    )
}
