use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use rufin_core::settings::visualizer::{VisualizerAppearance, VisualizerStyle};

pub const VISUALIZER_ZERO_THRESHOLD: f64 = 0.004;
pub const VISUALIZER_TOP_GAP: f64 = 50.0;
const VISUALIZER_REFERENCE_FRAME_MICROS: f64 = 1_000_000.0 / 60.0;
const VISUALIZER_STALE_MICROS: i64 = 133_333;

#[derive(Clone, Copy, Default)]
struct Peak {
    level: f64,
    hold: f64,
}

impl Peak {
    fn advance(&mut self, level: f64, elapsed: f64, hold: f64, fall: f64) {
        if level >= self.level {
            self.level = level;
            self.hold = hold;
        } else {
            let falling = (elapsed - self.hold).max(0.0);
            self.hold = (self.hold - elapsed).max(0.0);
            self.level = (self.level - falling * fall).max(level);
        }
    }
}

pub struct VisualizerParts {
    pub sidebar_area: gtk::DrawingArea,
    pub fullscreen_area: gtk::DrawingArea,
    pub levels: Rc<RefCell<Vec<f64>>>,
    pub targets: Rc<RefCell<Vec<f64>>>,
    pub generation: Rc<Cell<u64>>,
    pub tick: RefCell<Option<gtk::TickCallbackId>>,
    pub active: Cell<bool>,
    pub appearance: Rc<RefCell<VisualizerAppearance>>,
    peaks: Rc<RefCell<Vec<Peak>>>,
}

pub fn build_visualizer() -> VisualizerParts {
    let levels = Rc::new(RefCell::new(Vec::new()));
    let appearance = Rc::new(RefCell::new(VisualizerAppearance::default()));
    let peaks = Rc::new(RefCell::new(Vec::new()));
    let fullscreen_area = build_visualizer_area(
        Rc::clone(&levels),
        Rc::clone(&appearance),
        Rc::clone(&peaks),
    );
    let sidebar_area = build_visualizer_area(
        Rc::clone(&levels),
        Rc::clone(&appearance),
        Rc::clone(&peaks),
    );
    sidebar_area.remove_css_class("fullscreen-player-visualizer-area");
    sidebar_area.add_css_class("sidebar-visualizer-area");
    VisualizerParts {
        sidebar_area,
        fullscreen_area,
        levels,
        targets: Rc::new(RefCell::new(Vec::new())),
        generation: Rc::new(Cell::new(0)),
        tick: RefCell::new(None),
        active: Cell::new(false),
        appearance,
        peaks,
    }
}

fn build_visualizer_area(
    levels: Rc<RefCell<Vec<f64>>>,
    appearance: Rc<RefCell<VisualizerAppearance>>,
    peaks: Rc<RefCell<Vec<Peak>>>,
) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.add_css_class("fullscreen-player-visualizer-area");
    area.add_css_class("visualizer-accent");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_halign(gtk::Align::Fill);
    area.set_valign(gtk::Align::Fill);
    area.set_draw_func(move |area, context, width, height| {
        let levels = levels.borrow();
        if !levels.is_empty() {
            draw_visualizer(
                context,
                width,
                height,
                &levels,
                &peaks.borrow(),
                &appearance.borrow(),
                area.color(),
            );
        }
    });
    area
}

fn draw_visualizer(
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    levels: &[f64],
    peaks: &[Peak],
    appearance: &VisualizerAppearance,
    accent: gtk::gdk::RGBA,
) {
    if levels.len() < 2 {
        return;
    }
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let column_gap = appearance.spacing;
    let row_gap = 2.0;
    let left = width * 0.008;
    let (columns, cell) = visualizer_column_geometry(width, levels.len(), column_gap);
    let (rows, row_height) = visualizer_row_geometry(height, cell, row_gap);
    let row_stride = row_height + row_gap;
    let bottom = height;
    let bars = visualizer_bar_levels(levels, columns);
    let colors = appearance.colors.unwrap_or_else(|| accent_gradient(accent));
    if bars.is_empty() {
        return;
    }

    let peak_bars = if appearance.peaks {
        let peak_levels = peaks.iter().map(|peak| peak.level).collect::<Vec<_>>();
        visualizer_bar_levels(&peak_levels, columns)
    } else {
        Vec::new()
    };
    let graph_height = rows as f64 * row_stride - row_gap;
    let gradient = gtk::cairo::LinearGradient::new(0.0, bottom, 0.0, bottom - graph_height);
    for (offset, color) in [(0.0, colors[0]), (1.0, colors[1])] {
        gradient.add_color_stop_rgb(offset, color[0].into(), color[1].into(), color[2].into());
    }
    if matches!(
        appearance.style,
        VisualizerStyle::Line | VisualizerStyle::Filled
    ) {
        let _ = context.set_source(&gradient);
        for (column, level) in bars.iter().enumerate() {
            let x = left + cell / 2.0 + column as f64 * (cell + column_gap);
            let y = bottom - level * graph_height;
            if column == 0 {
                context.move_to(x, y);
            } else {
                context.line_to(x, y);
            }
        }
        if appearance.style == VisualizerStyle::Filled {
            context.line_to(
                left + cell / 2.0 + (columns - 1) as f64 * (cell + column_gap),
                bottom,
            );
            context.line_to(left + cell / 2.0, bottom);
            context.close_path();
            let _ = context.fill();
        } else {
            context.set_line_width(2.0);
            let _ = context.stroke();
        }
    }
    for (column, level) in bars.iter().copied().enumerate() {
        let scaled = level * rows as f64;
        let x = left + column as f64 * (cell + column_gap);
        let bar_height = level * graph_height;
        if appearance.style == VisualizerStyle::Circular {
            let center = (width / 2.0, height / 2.0);
            let radius = width.min(height) * 0.22;
            let extent = width.min(height) * 0.25;
            let angle = column as f64 / columns as f64 * std::f64::consts::TAU
                - std::f64::consts::FRAC_PI_2;
            let (sin, cos) = angle.sin_cos();
            set_color(context, colors, level, 1.0);
            context.set_line_width(
                (std::f64::consts::TAU * radius / columns as f64 - column_gap).max(1.0),
            );
            if level > 0.0 {
                context.move_to(center.0 + cos * radius, center.1 + sin * radius);
                context.line_to(
                    center.0 + cos * (radius + extent * level),
                    center.1 + sin * (radius + extent * level),
                );
                let _ = context.stroke();
            }
            if appearance.peaks
                && let Some(peak) = peak_bars.get(column).filter(|peak| **peak > 0.0)
            {
                context.arc(
                    center.0 + cos * (radius + extent * peak),
                    center.1 + sin * (radius + extent * peak),
                    1.5,
                    0.0,
                    std::f64::consts::TAU,
                );
                let _ = context.fill();
            }
            continue;
        }
        if appearance.style != VisualizerStyle::Segmented {
            let mirrored = appearance.style == VisualizerStyle::Mirrored;
            let y = if mirrored {
                height / 2.0 - bar_height / 2.0
            } else {
                bottom - bar_height
            };
            let _ = context.set_source(&gradient);
            if bar_height > 0.0 {
                match appearance.style {
                    VisualizerStyle::Rounded => {
                        let radius = (cell / 2.0).min(bar_height / 2.0);
                        context.new_sub_path();
                        context.arc(
                            x + cell - radius,
                            y + radius,
                            radius,
                            -std::f64::consts::FRAC_PI_2,
                            0.0,
                        );
                        context.arc(
                            x + cell - radius,
                            bottom - radius,
                            radius,
                            0.0,
                            std::f64::consts::FRAC_PI_2,
                        );
                        context.arc(
                            x + radius,
                            bottom - radius,
                            radius,
                            std::f64::consts::FRAC_PI_2,
                            std::f64::consts::PI,
                        );
                        context.arc(
                            x + radius,
                            y + radius,
                            radius,
                            std::f64::consts::PI,
                            3.0 * std::f64::consts::FRAC_PI_2,
                        );
                        context.close_path();
                        let _ = context.fill();
                    }
                    VisualizerStyle::Outline => {
                        context.set_line_width(1.0);
                        context.rectangle(
                            x + 0.5,
                            y + 0.5,
                            (cell - 1.0).max(0.0),
                            (bar_height - 1.0).max(0.0),
                        );
                        let _ = context.stroke();
                    }
                    VisualizerStyle::Solid | VisualizerStyle::Mirrored => {
                        context.rectangle(x, y, cell, bar_height);
                        let _ = context.fill();
                    }
                    _ => {}
                }
            }
        } else {
            let full_cells = scaled.floor().clamp(0.0, rows as f64) as usize;
            for row in 0..full_cells {
                let color_t = if full_cells > 1 {
                    row as f64 / (full_cells - 1) as f64
                } else {
                    0.0
                };
                set_color(context, colors, color_t, 0.72 + color_t * 0.24);
                let y = bottom - row_height - row as f64 * row_stride;
                context.rectangle(x, y, cell, row_height);
                let _ = context.fill();
            }

            let cap_row = full_cells;
            let cap_alpha = scaled - scaled.floor();
            if cap_row < rows && cap_alpha >= 0.14 {
                let color_t = cap_row as f64 / rows.saturating_sub(1).max(1) as f64;
                set_color(context, colors, color_t, cap_alpha * 0.76);
                let y = bottom - row_height - cap_row as f64 * row_stride;
                context.rectangle(x, y, cell, row_height);
                let _ = context.fill();
            }
        }
        if appearance.peaks
            && let Some(peak) = peak_bars.get(column).filter(|peak| **peak > 0.0)
        {
            set_color(context, colors, 1.0, 1.0);
            let y = if appearance.style == VisualizerStyle::Mirrored {
                height / 2.0 - peak * graph_height / 2.0
            } else {
                bottom - peak * graph_height
            };
            context.rectangle(x, y, cell, 2.0);
            if appearance.style == VisualizerStyle::Mirrored {
                context.rectangle(x, height - y - 2.0, cell, 2.0);
            }
            let _ = context.fill();
        }
    }
}

pub fn visualizer_column_geometry(width: f64, level_count: usize, gap: f64) -> (usize, f64) {
    let available_width = (width * 0.984).max(1.0);
    let fitting_columns = ((available_width + gap) / (2.0 + gap)).floor() as usize;
    let columns = level_count.min(fitting_columns.max(1)).max(1);
    let cell =
        ((available_width - gap * columns.saturating_sub(1) as f64) / columns as f64).max(2.0);
    (columns, cell)
}

pub fn visualizer_row_geometry(height: f64, cell: f64, gap: f64) -> (usize, f64) {
    let grid_height = (height - VISUALIZER_TOP_GAP).max(height * 0.64);
    let rows = (((grid_height + gap) / (cell + gap)).floor() as usize).clamp(8, 32);
    let row_height = ((grid_height - gap * rows.saturating_sub(1) as f64) / rows as f64).max(1.0);
    (rows, row_height)
}

pub(super) fn accent_gradient(accent: gtk::gdk::RGBA) -> [[f32; 3]; 2] {
    let base = [accent.red(), accent.green(), accent.blue()];
    [
        base.map(|value| value * 0.75),
        base.map(|value| value + (1.0 - value) * 0.32),
    ]
}

fn set_color(context: &gtk::cairo::Context, colors: [[f32; 3]; 2], mix: f64, alpha: f64) {
    let color: [f64; 3] =
        std::array::from_fn(|index| lerp(colors[0][index].into(), colors[1][index].into(), mix));
    context.set_source_rgba(color[0], color[1], color[2], alpha);
}

pub fn lerp(start: f64, end: f64, mix: f64) -> f64 {
    start + (end - start) * mix
}

pub fn visualizer_bar_levels(levels: &[f64], columns: usize) -> Vec<f64> {
    if columns == 0 || levels.is_empty() {
        return Vec::new();
    }
    (0..columns)
        .map(|column| {
            let start = column * levels.len() / columns;
            let end = ((column + 1) * levels.len() / columns).max(start + 1);
            let mut total = 0.0;
            let mut peak = 0.0_f64;
            let mut count = 0;
            for level in &levels[start..end.min(levels.len())] {
                let level = level.clamp(0.0, 1.0);
                total += level;
                peak = peak.max(level);
                count += 1;
            }
            let average = if count == 0 {
                0.0
            } else {
                total / count as f64
            };
            average * 0.4 + peak * 0.6
        })
        .collect()
}

#[cfg(test)]
mod visualizer_tests {
    use super::*;

    #[test]
    fn peak_holds_then_falls_independent_of_frame_rate() {
        let mut peak = Peak::default();
        peak.advance(0.8, 0.0, 0.5, 0.6);
        peak.advance(0.2, 0.4, 0.5, 0.6);
        assert_eq!(peak.level, 0.8);
        let mut stepped = peak;
        peak.advance(0.2, 0.3, 0.5, 0.6);
        for _ in 0..30 {
            stepped.advance(0.2, 0.01, 0.5, 0.6);
        }
        assert!((peak.level - 0.68).abs() < 1e-9);
        assert!((peak.level - stepped.level).abs() < 1e-9);
        peak.advance(0.0, 2.0, 0.5, 0.6);
        assert_eq!(peak.level, 0.0);
    }

    #[test]
    pub fn sidebar_visualizer_fits_all_received_bands() {
        let width = 213.0;
        let gap = 1.0;
        let (columns, cell) = visualizer_column_geometry(width, 52, gap);
        let occupied = columns as f64 * cell + columns.saturating_sub(1) as f64 * gap;

        assert_eq!(columns, 52);
        assert!(cell >= 3.0);
        assert!(occupied <= width * 0.984 + f64::EPSILON);
    }

    #[test]
    pub fn silent_visualizer_bands_remain_zero() {
        assert_eq!(visualizer_bar_levels(&[0.0; 52], 52), vec![0.0; 52]);
    }
}

use gtk::glib;
use playback::TransportStatus;
impl crate::PlayerUi {
    pub fn apply_visualizer_appearance(&self) {
        let appearance = self.settings.current.borrow().visualizer.appearance.clone();
        let sidebar_opacity = appearance.opacity
            * if self.lyrics.panel_visible.get() {
                appearance.sidebar_lyrics_opacity
            } else {
                1.0
            };
        let fullscreen_opacity = appearance.opacity
            * if self.views.fullscreen_player.lyrics_enabled.get() {
                appearance.fullscreen_lyrics_opacity
            } else {
                1.0
            };
        *self.views.visualizer.appearance.borrow_mut() = appearance;
        self.views
            .visualizer
            .sidebar_area
            .set_opacity(sidebar_opacity);
        self.views
            .visualizer
            .fullscreen_area
            .set_opacity(fullscreen_opacity);
        self.views.visualizer.sidebar_area.queue_draw();
        self.views.visualizer.fullscreen_area.queue_draw();
    }

    pub fn apply_visualizer_levels(self: &Rc<Self>, levels: Vec<f64>) {
        if levels.is_empty() {
            self.clear_visualizer();
            return;
        }
        if !self.views.visualizer.active.get() {
            return;
        }
        *self.views.visualizer.targets.borrow_mut() = levels
            .into_iter()
            .map(|level| level.clamp(0.0, 1.0))
            .collect();
        let generation = &self.views.visualizer.generation;
        generation.set(generation.get().wrapping_add(1).max(1));
        self.start_visualizer_tick();
    }

    pub fn sync_visualizer_state(self: &Rc<Self>) {
        self.apply_visualizer_appearance();
        if let Some(lyrics) = self.selected_lyrics() {
            lyrics
                .right_pane
                .set_lyrics_visible(self.lyrics.panel_visible.get());
            lyrics
                .fullscreen_pane
                .set_lyrics_visible(self.views.fullscreen_player.lyrics_enabled.get());
        }
        let fullscreen_visible = self.fullscreen_player_visible()
            && self.views.fullscreen_player.experience_visible()
            && self.views.fullscreen_player.visualizer_enabled.get();
        let sidebar_visible = !self.fullscreen_player_visible()
            && self.right_sidebar_visible()
            && self.right_panel.visualizer_visible.get();
        let active = (fullscreen_visible || sidebar_visible)
            && self.selected_playback().as_deref().is_some_and(|player| {
                matches!(
                    player.transport.effective_state(),
                    TransportStatus::Playing | TransportStatus::Buffering
                )
            });
        let changed = self.views.visualizer.active.replace(active) != active;
        if active {
            self.playback_handles.transport.set_visualizer_enabled(true);
            self.start_visualizer_tick();
            return;
        }
        if changed {
            self.playback_handles
                .transport
                .set_visualizer_enabled(false);
            self.stop_visualizer_tick();
            self.clear_visualizer();
        }
    }

    fn start_visualizer_tick(self: &Rc<Self>) {
        if self.views.visualizer.tick.borrow().is_some() {
            return;
        }
        let levels = Rc::clone(&self.views.visualizer.levels);
        let targets = Rc::clone(&self.views.visualizer.targets);
        let generation = Rc::clone(&self.views.visualizer.generation);
        let appearance = Rc::clone(&self.views.visualizer.appearance);
        let peaks = Rc::clone(&self.views.visualizer.peaks);
        let fullscreen_area = self.views.visualizer.fullscreen_area.clone();
        let sidebar_area = self.views.visualizer.sidebar_area.clone();
        let shell = Rc::downgrade(self);
        let seen_generation = Cell::new(generation.get());
        let last_data_time = Cell::new(None);
        let last_frame_time = Cell::new(None);
        let Some(window) = self.window.upgrade() else {
            return;
        };
        let tick = window.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let elapsed_frames = last_frame_time.replace(Some(now)).map_or(1.0, |last| {
                (now - last) as f64 / VISUALIZER_REFERENCE_FRAME_MICROS
            });
            let appearance = appearance.borrow();
            let rise_weight = 1.0 - (1.0 - appearance.rise).powf(elapsed_frames);
            let fall_weight = 1.0 - (1.0 - appearance.fall).powf(elapsed_frames);
            let next_generation = generation.get();
            if next_generation != seen_generation.get() || last_data_time.get().is_none() {
                seen_generation.set(next_generation);
                last_data_time.set(Some(now));
            }
            let stale = now - last_data_time.get().unwrap_or(now) >= VISUALIZER_STALE_MICROS;
            if stale {
                targets.borrow_mut().fill(0.0);
            }
            let mut current = levels.borrow_mut();
            let target = targets.borrow();
            let len = target.len().max(current.len());
            current.resize(len, 0.0);
            let mut changed = false;
            let mut peaks = peaks.borrow_mut();
            peaks.resize(len, Peak::default());
            for index in 0..len {
                let next = target.get(index).copied().unwrap_or(0.0);
                let value = current[index];
                let weight = if next >= value {
                    rise_weight
                } else {
                    fall_weight
                };
                let mut smoothed = next * weight + value * (1.0 - weight);
                if next == 0.0 && smoothed < VISUALIZER_ZERO_THRESHOLD {
                    smoothed = 0.0;
                }
                changed |= (smoothed - value).abs() > 0.0005;
                current[index] = smoothed;
                if appearance.peaks {
                    peaks[index].advance(
                        smoothed,
                        elapsed_frames / 60.0,
                        appearance.peak_hold,
                        appearance.peak_fall,
                    );
                    changed |= peaks[index].level > 0.0;
                } else {
                    peaks[index] = Peak::default();
                }
            }
            if fullscreen_area.is_mapped() {
                fullscreen_area.queue_draw();
            } else if sidebar_area.is_mapped() {
                sidebar_area.queue_draw();
            }
            if stale && !changed {
                if let Some(shell) = shell.upgrade() {
                    shell.views.visualizer.tick.borrow_mut().take();
                }
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.views.visualizer.tick.borrow_mut() = Some(tick);
    }

    fn stop_visualizer_tick(&self) {
        if let Some(tick) = self.views.visualizer.tick.borrow_mut().take() {
            tick.remove();
        }
    }

    fn clear_visualizer(&self) {
        self.views.visualizer.peaks.borrow_mut().clear();
        self.views.visualizer.levels.borrow_mut().clear();
        self.views.visualizer.targets.borrow_mut().clear();
        self.views.visualizer.fullscreen_area.queue_draw();
        self.views.visualizer.sidebar_area.queue_draw();
    }
}
