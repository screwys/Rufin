use std::{collections::HashMap, fmt::Write};

use artwork::RgbaImage;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageEncoder;
use library::{ActivityOverview, CalendarActivityPeriod};
use localization::{tr, tr_with, trn_with};
use resvg::{tiny_skia, usvg};

use super::{percentage_change, period_label};
use crate::settings::app::ActivityOverviewSettings;

const WIDTH: f32 = 1536.0;
const MARGIN: f32 = 48.0;
const GAP: f32 = 40.0;
const COLUMN: f32 = (WIDTH - MARGIN * 2.0 - GAP) / 2.0;

/// Resolved application colors, independent of a platform's theme API.
#[derive(Clone, Debug)]
pub struct ReportAppearance {
    pub dark: bool,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub accent: [u8; 3],
}

pub(super) fn png(
    data: &ActivityOverview,
    period: CalendarActivityPeriod,
    settings: &ActivityOverviewSettings,
    appearance: &ReportAppearance,
    images: Vec<(Vec<u8>, RgbaImage)>,
) -> Result<Vec<u8>, String> {
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    // fontdb's system discovery currently excludes Android.
    #[cfg(target_os = "android")]
    options.fontdb_mut().load_fonts_dir("/system/fonts");
    let family = [
        "Adwaita Sans",
        "Noto Sans",
        "DejaVu Sans",
        "Roboto",
        "Arial",
    ]
    .into_iter()
    .find(|name| {
        options
            .fontdb
            .faces()
            .any(|face| face.families.iter().any(|(family, _)| family == name))
    })
    .or_else(|| {
        options
            .fontdb
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|(name, _)| name.as_str())
    })
    .ok_or("No fonts are available for the listening report")?
    .to_string();
    options.font_family = family.clone();
    options.fontdb_mut().set_sans_serif_family(family);
    let mut svg = ReportSvg {
        body: String::new(),
        options,
        foreground: color(appearance.foreground),
    };
    let images: HashMap<_, _> = images.into_iter().collect();
    let mut encoded = HashMap::new();
    for (key, image) in &images {
        encoded.insert(
            key.clone(),
            png_uri(image.rgba(), image.width(), image.height())?,
        );
    }
    let limit = settings.result_count;
    let title = tr_with(
        if settings.show_rufin_in_headline {
            "Your {period} in Rufin"
        } else {
            "Your {period}"
        },
        &[("period", &period_label(period))],
    );
    let lines = svg.wrap(&title, 52.0, 700, COLUMN);
    let heading_height = (lines.len() as f32 * 64.0 + 64.0).max(300.0);
    let first_baseline = MARGIN + (heading_height - lines.len() as f32 * 64.0) / 2.0 + 50.0;
    for (index, line) in lines.iter().enumerate() {
        svg.text(
            MARGIN,
            first_baseline + index as f32 * 64.0,
            52.0,
            700,
            line,
            COLUMN,
        );
    }
    let totals = &data.totals;
    let previous = &data.previous;
    let metrics = [
        (
            tr("Hours"),
            format!("{:.1}", totals.duration_millis as f64 / 3_600_000.0),
            totals.duration_millis,
            previous.duration_millis,
            include_str!(
                "../../../../resources/icons/hicolor/scalable/actions/rufin-history-symbolic.svg"
            ),
        ),
        (
            tr("Plays"),
            totals.plays.to_string(),
            totals.plays,
            previous.plays,
            include_str!(
                "../../../../resources/icons/hicolor/scalable/actions/rufin-media-playback-start-symbolic.svg"
            ),
        ),
        (
            tr("Artists"),
            totals.artists.to_string(),
            totals.artists,
            previous.artists,
            include_str!(
                "../../../../resources/icons/hicolor/scalable/actions/rufin-artists-symbolic.svg"
            ),
        ),
        (
            tr("Tracks"),
            totals.tracks.to_string(),
            totals.tracks,
            previous.tracks,
            include_str!(
                "../../../../resources/icons/hicolor/scalable/actions/rufin-tracks-symbolic.svg"
            ),
        ),
    ];
    for (index, (label, value, current, previous, icon)) in metrics.iter().enumerate() {
        let x = MARGIN + COLUMN + GAP + (index % 2) as f32 * (COLUMN + 20.0) / 2.0;
        let y = MARGIN + (index / 2) as f32 * 150.0;
        let width = (COLUMN - 20.0) / 2.0;
        let _ = write!(
            svg.body,
            "<rect x='{x}' y='{y}' width='{width}' height='130' rx='24' fill='{}' fill-opacity='.045' stroke='{}' stroke-opacity='.12'/>",
            svg.foreground, svg.foreground,
        );
        svg.icon(x + 26.0, y + 23.0, 26.0, icon, &color(appearance.accent));
        svg.text(x + 65.0, y + 46.0, 23.0, 400, label, width - 90.0);
        svg.text(x + 26.0, y + 99.0, 43.0, 700, value, width - 52.0);
        if settings.show_comparison {
            let change = percentage_change(*current, *previous);
            let value_width = svg.measure(value, 43.0, 700);
            svg.text(
                x + 40.0 + value_width,
                y + 98.0,
                18.0,
                400,
                &change,
                width - value_width - 64.0,
            );
        }
    }
    let mut y = MARGIN + heading_height + 54.0;
    let collection_count = usize::from(settings.artists) + usize::from(settings.albums);
    if collection_count > 0 {
        let width = if collection_count == 2 {
            COLUMN
        } else {
            WIDTH - 2.0 * MARGIN
        };
        let mut bottom = y;
        if settings.artists {
            let entries = data.artists.iter().take(limit).map(|row| {
                (
                    row.name.as_str(),
                    row.play_count,
                    row.artwork_binding.as_deref(),
                )
            });
            bottom =
                bottom.max(svg.collection(&tr("Top artists"), MARGIN, y, width, entries, &encoded));
        }
        if settings.albums {
            let x = MARGIN + if settings.artists { COLUMN + GAP } else { 0.0 };
            let entries = data.albums.iter().take(limit).map(|row| {
                (
                    row.title.as_str(),
                    row.play_count,
                    row.artwork_binding.as_deref(),
                )
            });
            bottom = bottom.max(svg.collection(&tr("Top albums"), x, y, width, entries, &encoded));
        }
        y = bottom + 44.0;
    }
    let show_genres = settings.genres && !data.genres.is_empty();
    if settings.tracks || show_genres {
        if collection_count > 0 {
            svg.rule(MARGIN, y, WIDTH - 2.0 * MARGIN);
            y += 66.0;
        }
        let width = if settings.tracks && show_genres {
            COLUMN
        } else {
            WIDTH - 2.0 * MARGIN
        };
        let mut bottom = y;
        if settings.tracks {
            svg.text(MARGIN, y, 32.0, 700, &tr("Top tracks"), width);
            let top = y + 58.0;
            svg.text(MARGIN + 28.0, top, 21.0, 400, "#", 50.0);
            svg.text(MARGIN + 90.0, top, 21.0, 400, &tr("Title"), width - 220.0);
            svg.text(MARGIN + width - 90.0, top, 21.0, 400, &tr("Plays"), 90.0);
            svg.rule(MARGIN, top + 18.0, width);
            for (index, row) in data.tracks.iter().take(limit).enumerate() {
                let row_y = top + 34.0 + index as f32 * 108.0;
                svg.text(
                    MARGIN + 28.0,
                    row_y + 46.0,
                    22.0,
                    400,
                    &(index + 1).to_string(),
                    46.0,
                );
                svg.cover(
                    MARGIN + 90.0,
                    row_y,
                    78.0,
                    row.artwork_binding
                        .as_deref()
                        .and_then(|key| encoded.get(key)),
                );
                svg.text(
                    MARGIN + 188.0,
                    row_y + 30.0,
                    23.0,
                    400,
                    &row.title,
                    width - 306.0,
                );
                svg.text(
                    MARGIN + 188.0,
                    row_y + 62.0,
                    21.0,
                    400,
                    &row.artist,
                    width - 306.0,
                );
                svg.text(
                    MARGIN + width - 90.0,
                    row_y + 46.0,
                    23.0,
                    400,
                    &row.play_count.to_string(),
                    90.0,
                );
                if index + 1 < data.tracks.len().min(limit) {
                    svg.rule(MARGIN, row_y + 92.0, width);
                }
            }
            bottom = bottom.max(top + 34.0 + data.tracks.len().min(limit) as f32 * 108.0);
        }
        if show_genres {
            let x = MARGIN + if settings.tracks { COLUMN + GAP } else { 0.0 };
            svg.text(x, y, 32.0, 700, &tr("Top genres"), width);
            let maximum = data.genres.first().map_or(1, |(_, count)| *count).max(1) as f32;
            for (index, (name, count)) in data.genres.iter().take(limit).enumerate() {
                let top = y + 52.0 + index as f32 * 82.0;
                svg.text(x, top, 23.0, 400, name, width - 100.0);
                let label = count.to_string();
                let count_width = svg.measure(&label, 23.0, 400);
                svg.text(
                    x + width - count_width,
                    top,
                    23.0,
                    400,
                    &label,
                    count_width + 1.0,
                );
                let bar_width = width * (*count as f32 / maximum).clamp(0.0, 1.0);
                let _ = write!(
                    svg.body,
                    "<rect x='{x}' y='{}' width='{width}' height='13' rx='6.5' fill='{}' fill-opacity='.18'/><rect x='{x}' y='{}' width='{bar_width}' height='13' rx='6.5' fill='{}'/>",
                    top + 20.0,
                    svg.foreground,
                    top + 20.0,
                    color(appearance.accent)
                );
            }
            bottom = bottom.max(y + 52.0 + data.genres.len().min(limit) as f32 * 82.0);
        }
        y = bottom;
    }
    let height = (y + MARGIN).ceil() as u32;
    let background = data
        .tracks
        .first()
        .and_then(|row| row.artwork_binding.as_ref())
        .and_then(|key| images.get(key));
    let backdrop = backdrop(background, settings, appearance, height)?;
    let document = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{WIDTH}' height='{height}'>{backdrop}<g fill='{}' font-family='sans-serif'>{}</g></svg>",
        svg.foreground, svg.body
    );
    let tree = usvg::Tree::from_str(&document, &svg.options).map_err(|error| error.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(WIDTH as u32, height)
        .ok_or("Could not allocate the listening report image")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|error| error.to_string())
}

struct ReportSvg {
    body: String,
    options: usvg::Options<'static>,
    foreground: String,
}

impl ReportSvg {
    fn measure(&self, text: &str, size: f32, weight: u16) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let document = format!(
            "<svg xmlns='http://www.w3.org/2000/svg' width='1536' height='100'><text font-family='sans-serif' font-size='{size}' font-weight='{weight}'>{}</text></svg>",
            escape(text)
        );
        usvg::Tree::from_str(&document, &self.options)
            .expect("generated text SVG")
            .root()
            .bounding_box()
            .width()
    }

    fn fit(&self, text: &str, size: f32, weight: u16, width: f32) -> String {
        if self.measure(text, size, weight) <= width {
            return text.to_string();
        }
        let boundaries: Vec<_> = text.char_indices().map(|(index, _)| index).collect();
        let mut lo = 0;
        let mut hi = boundaries.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let prefix = text.get(..boundaries[mid]).expect("character boundary");
            if self.measure(&format!("{prefix}…"), size, weight) <= width {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let end = lo.checked_sub(1).map_or(0, |index| boundaries[index]);
        format!("{}…", text.get(..end).expect("character boundary"))
    }

    fn wrap(&self, text: &str, size: f32, weight: u16, width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for ch in text.chars() {
            let next = format!("{line}{ch}");
            if !line.is_empty() && self.measure(&next, size, weight) > width {
                if let Some(space) = line.rfind(' ') {
                    lines.push(line.get(..space).expect("space boundary").to_string());
                    line = line.get(space + 1..).expect("space boundary").to_string();
                } else {
                    lines.push(std::mem::take(&mut line));
                }
            }
            if !line.is_empty() || !ch.is_whitespace() {
                line.push(ch);
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    fn text(&mut self, x: f32, y: f32, size: f32, weight: u16, text: &str, width: f32) {
        if width <= 0.0 || text.is_empty() {
            return;
        }
        let text = self.fit(text, size, weight, width);
        let _ = write!(
            self.body,
            "<text x='{x}' y='{y}' font-size='{size}' font-weight='{weight}'>{}</text>",
            escape(&text)
        );
    }

    fn rule(&mut self, x: f32, y: f32, width: f32) {
        let _ = write!(
            self.body,
            "<path d='M{x} {y}h{width}' stroke='{}' stroke-opacity='.18' stroke-width='1.5'/>",
            self.foreground
        );
    }

    fn cover(&mut self, x: f32, y: f32, size: f32, uri: Option<&String>) {
        let radius = (size * 0.1).min(22.0);
        let id = format!("cover-{}", self.body.len());
        let _ = write!(
            self.body,
            "<clipPath id='{id}'><rect x='{x}' y='{y}' width='{size}' height='{size}' rx='{radius}'/></clipPath>"
        );
        if let Some(uri) = uri {
            let _ = write!(
                self.body,
                "<image x='{x}' y='{y}' width='{size}' height='{size}' href='{uri}' preserveAspectRatio='xMidYMid slice' clip-path='url(#{id})'/>"
            );
        } else {
            let _ = write!(
                self.body,
                "<rect x='{x}' y='{y}' width='{size}' height='{size}' rx='{radius}' fill='#77767b'/>"
            );
            self.icon(
                x + size * 0.125,
                y + size * 0.125,
                size * 0.75,
                include_str!("../../../../resources/icons/hicolor/symbolic/apps/io.github.screwys.Rufin-symbolic.svg"),
                "#deddda",
            );
        }
    }

    fn icon(&mut self, x: f32, y: f32, size: f32, source: &str, color: &str) {
        let source = source
            .get(source.find("<svg").expect("bundled SVG icon")..)
            .expect("SVG element boundary")
            .replace("fill=\"#2e3436\"", "fill=\"currentColor\"");
        let _ = write!(
            self.body,
            "<g transform='translate({x} {y}) scale({})' fill='{color}' color='{color}'>{source}</g>",
            size / 16.0,
        );
    }

    fn collection<'a>(
        &mut self,
        title: &str,
        x: f32,
        y: f32,
        width: f32,
        entries: impl Iterator<Item = (&'a str, i64, Option<&'a [u8]>)>,
        images: &HashMap<Vec<u8>, String>,
    ) -> f32 {
        self.text(x, y, 32.0, 700, title, width);
        let columns = if width > COLUMN { 6 } else { 3 };
        let size = (width - 26.0 * (columns - 1) as f32) / columns as f32;
        let mut bottom = y + 30.0;
        for (index, (name, count, binding)) in entries.enumerate() {
            let left = x + (index % columns) as f32 * (size + 26.0);
            let top = y + 40.0 + (index / columns) as f32 * (size + 110.0);
            self.cover(left, top, size, binding.and_then(|key| images.get(key)));
            self.text(left, top + size + 30.0, 23.0, 400, name, size);
            let caption = trn_with(
                "{count} play",
                "{count} plays",
                count as u64,
                &[("count", &count.to_string())],
            );
            self.text(left, top + size + 64.0, 23.0, 600, &caption, size);
            bottom = top + size + 64.0;
        }
        bottom
    }
}

fn backdrop(
    image: Option<&RgbaImage>,
    settings: &ActivityOverviewSettings,
    appearance: &ReportAppearance,
    height: u32,
) -> Result<String, String> {
    let mut background = appearance.background;
    if settings.dynamic_background {
        let mut mean = [0.0; 3];
        if let Some(image) = image {
            let mut count = 0.0;
            for pixel in image.rgba().chunks_exact(4).step_by(64) {
                let alpha = f32::from(pixel[3]) / 255.0;
                for channel in 0..3 {
                    mean[channel] += f32::from(pixel[channel]) * alpha;
                }
                count += alpha;
            }
            if count > 0.0 {
                mean.iter_mut().for_each(|value| *value /= count);
            }
        }
        background = mean.map(|value| if appearance.dark { value * 0.22 } else { 255.0 * 0.85 + value * 0.15 } as u8);
    }
    let mut result = format!(
        "<rect width='{WIDTH}' height='{height}' fill='{}'/>",
        color(background)
    );
    if settings.dynamic_background
        && settings.background_image
        && let Some(image) = image
    {
        let source =
            image::RgbaImage::from_raw(image.width(), image.height(), image.rgba().to_vec())
                .expect("decoded RGBA dimensions");
        // Blur a small cover, then scale it to the report. Avoid a full-page filter buffer.
        let small = image::imageops::thumbnail(&source, 128, 128);
        let blurred = image::imageops::blur(&small, 8.0);
        let uri = png_uri(blurred.as_raw(), blurred.width(), blurred.height())?;
        let opacity = if appearance.dark { 0.25 } else { 0.12 };
        let _ = write!(
            result,
            "<image width='{WIDTH}' height='{height}' href='{uri}' preserveAspectRatio='xMidYMid slice' image-rendering='smooth' opacity='{opacity}'/>"
        );
    }
    Ok(result)
}

fn png_uri(rgba: &[u8], width: u32, height: u32) -> Result<String, String> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|error| error.to_string())?;
    Ok(format!("data:image/png;base64,{}", STANDARD.encode(bytes)))
}

fn color(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn escape(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '\'' => "&apos;".into(),
            '"' => "&quot;".into(),
            '\t' | '\n' | '\r' => " ".into(),
            ch if ch < ' ' || ch == '\u{fffe}' || ch == '\u{ffff}' => "�".into(),
            ch => ch.to_string(),
        })
        .collect()
}
