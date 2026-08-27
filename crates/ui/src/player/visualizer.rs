use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use playback::TransportStatus;

use crate::shell::Shell;

const VISUALIZER_RISE_WEIGHT: f64 = 0.72;
const VISUALIZER_FALL_WEIGHT: f64 = 0.16;
const VISUALIZER_ZERO_THRESHOLD: f64 = 0.004;
const VISUALIZER_TOP_GAP: f64 = 50.0;

pub(crate) struct VisualizerParts {
    pub(crate) sidebar_area: gtk::DrawingArea,
    pub(crate) fullscreen_area: gtk::DrawingArea,
    levels: Rc<RefCell<Vec<f64>>>,
    targets: Rc<RefCell<Vec<f64>>>,
    generation: Rc<Cell<u64>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    active: Cell<bool>,
}

pub(crate) fn build_visualizer() -> VisualizerParts {
    let levels = Rc::new(RefCell::new(Vec::new()));
    let fullscreen_area = build_visualizer_area(Rc::clone(&levels));
    let sidebar_area = build_visualizer_area(Rc::clone(&levels));
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
    }
}

fn build_visualizer_area(levels: Rc<RefCell<Vec<f64>>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.add_css_class("fullscreen-player-visualizer-area");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_halign(gtk::Align::Fill);
    area.set_valign(gtk::Align::Fill);
    area.set_draw_func(move |_, context, width, height| {
        let levels = levels.borrow();
        if !levels.is_empty() {
            draw_visualizer_wave(context, width, height, &levels);
        }
    });
    area
}

fn draw_visualizer_wave(context: &gtk::cairo::Context, width: i32, height: i32, levels: &[f64]) {
    if levels.len() < 2 {
        return;
    }
    draw_visualizer_bars(context, width, height, levels);
}

fn draw_visualizer_bars(context: &gtk::cairo::Context, width: i32, height: i32, levels: &[f64]) {
    let width = f64::from(width.max(1));
    let height = f64::from(height.max(1));
    let column_gap = 1.0;
    let row_gap = 2.0;
    let left = width * 0.008;
    let (columns, cell) = visualizer_column_geometry(width, levels.len(), column_gap);
    let (rows, row_height) = visualizer_row_geometry(height, cell, row_gap);
    let row_stride = row_height + row_gap;
    let bottom = height;
    let bars = visualizer_bar_levels(levels, columns);
    let dark = adw::StyleManager::default().is_dark();
    if bars.is_empty() {
        return;
    }

    for (column, level) in bars.iter().copied().enumerate() {
        let scaled = level * rows as f64;
        let x = left + column as f64 * (cell + column_gap);
        let full_cells = scaled.floor().clamp(0.0, rows as f64) as usize;
        for row in 0..full_cells {
            let color_t = if full_cells > 1 {
                row as f64 / (full_cells - 1) as f64
            } else {
                0.0
            };
            let (red, green, blue) = visualizer_bar_color(color_t, dark);
            context.set_source_rgba(red, green, blue, 0.72 + color_t * 0.24);
            let y = bottom - row_height - row as f64 * row_stride;
            context.rectangle(x, y, cell, row_height);
            let _ = context.fill();
        }

        let cap_row = full_cells;
        let cap_alpha = scaled - scaled.floor();
        if cap_row < rows && cap_alpha >= 0.14 {
            let color_t = cap_row as f64 / rows.saturating_sub(1).max(1) as f64;
            let (red, green, blue) = visualizer_bar_color(color_t, dark);
            context.set_source_rgba(red, green, blue, cap_alpha * 0.76);
            let y = bottom - row_height - cap_row as f64 * row_stride;
            context.rectangle(x, y, cell, row_height);
            let _ = context.fill();
        }
    }
}

fn visualizer_column_geometry(width: f64, level_count: usize, gap: f64) -> (usize, f64) {
    let available_width = (width * 0.984).max(1.0);
    let fitting_columns = ((available_width + gap) / (2.0 + gap)).floor() as usize;
    let columns = level_count.min(fitting_columns.max(1)).max(1);
    let cell =
        ((available_width - gap * columns.saturating_sub(1) as f64) / columns as f64).max(2.0);
    (columns, cell)
}

fn visualizer_row_geometry(height: f64, cell: f64, gap: f64) -> (usize, f64) {
    let grid_height = (height - VISUALIZER_TOP_GAP).max(height * 0.64);
    let rows = (((grid_height + gap) / (cell + gap)).floor() as usize).clamp(8, 32);
    let row_height = ((grid_height - gap * rows.saturating_sub(1) as f64) / rows as f64).max(1.0);
    (rows, row_height)
}

fn visualizer_bar_color(row_t: f64, dark: bool) -> (f64, f64, f64) {
    let row_t = row_t.clamp(0.0, 1.0);
    if !dark {
        if row_t < 0.62 {
            let mix = row_t / 0.62;
            return (
                lerp(0.62, 0.82, mix),
                lerp(0.03, 0.22, mix),
                lerp(0.015, 0.02, mix),
            );
        }
        let mix = (row_t - 0.62) / 0.38;
        return (
            lerp(0.82, 0.92, mix),
            lerp(0.22, 0.43, mix),
            lerp(0.02, 0.04, mix),
        );
    }
    if row_t < 0.62 {
        let mix = row_t / 0.62;
        (
            lerp(0.86, 1.0, mix),
            lerp(0.10, 0.36, mix),
            lerp(0.04, 0.03, mix),
        )
    } else {
        let mix = (row_t - 0.62) / 0.38;
        (
            lerp(1.0, 1.0, mix),
            lerp(0.36, 0.66, mix),
            lerp(0.03, 0.08, mix),
        )
    }
}

fn lerp(start: f64, end: f64, mix: f64) -> f64 {
    start + (end - start) * mix
}

fn visualizer_bar_levels(levels: &[f64], columns: usize) -> Vec<f64> {
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

impl Shell {
    pub(crate) fn apply_visualizer_levels(self: &Rc<Self>, levels: Vec<f64>) {
        if levels.is_empty() {
            self.clear_visualizer();
            return;
        }
        if !self.player_view.visualizer.active.get() {
            return;
        }
        *self.player_view.visualizer.targets.borrow_mut() = levels
            .into_iter()
            .map(|level| level.clamp(0.0, 1.0))
            .collect();
        let generation = &self.player_view.visualizer.generation;
        generation.set(generation.get().wrapping_add(1).max(1));
        self.start_visualizer_tick();
    }

    pub(crate) fn sync_visualizer_state(self: &Rc<Self>) {
        let fullscreen_visible = self.fullscreen_player_visible()
            && self
                .player_view
                .fullscreen_player
                .stack
                .visible_child_name()
                .as_deref()
                == Some("visualizer");
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
        let changed = self.player_view.visualizer.active.replace(active) != active;
        if active {
            self.products
                .playback
                .transport
                .set_visualizer_enabled(true);
            self.start_visualizer_tick();
            return;
        }
        if changed {
            self.products
                .playback
                .transport
                .set_visualizer_enabled(false);
            self.stop_visualizer_tick();
            self.clear_visualizer();
        }
    }

    fn start_visualizer_tick(self: &Rc<Self>) {
        if self.player_view.visualizer.tick.borrow().is_some() {
            return;
        }
        let levels = Rc::clone(&self.player_view.visualizer.levels);
        let targets = Rc::clone(&self.player_view.visualizer.targets);
        let generation = Rc::clone(&self.player_view.visualizer.generation);
        let fullscreen_area = self.player_view.visualizer.fullscreen_area.clone();
        let sidebar_area = self.player_view.visualizer.sidebar_area.clone();
        let shell = Rc::downgrade(self);
        let seen_generation = Cell::new(generation.get());
        let stale_frames = Cell::new(0_u8);
        let tick = self.chrome.window.add_tick_callback(move |_, _| {
            let next_generation = generation.get();
            if next_generation == seen_generation.get() {
                stale_frames.set(stale_frames.get().saturating_add(1));
            } else {
                seen_generation.set(next_generation);
                stale_frames.set(0);
            }
            if stale_frames.get() >= 8 {
                targets.borrow_mut().fill(0.0);
            }
            let mut current = levels.borrow_mut();
            let target = targets.borrow();
            let len = target.len().max(current.len());
            current.resize(len, 0.0);
            let mut changed = false;
            for index in 0..len {
                let next = target.get(index).copied().unwrap_or(0.0);
                let value = current[index];
                let weight = if next >= value {
                    VISUALIZER_RISE_WEIGHT
                } else {
                    VISUALIZER_FALL_WEIGHT
                };
                let mut smoothed = next * weight + value * (1.0 - weight);
                if next == 0.0 && smoothed < VISUALIZER_ZERO_THRESHOLD {
                    smoothed = 0.0;
                }
                changed |= (smoothed - value).abs() > 0.0005;
                current[index] = smoothed;
            }
            if fullscreen_area.is_mapped() {
                fullscreen_area.queue_draw();
            } else if sidebar_area.is_mapped() {
                sidebar_area.queue_draw();
            }
            if stale_frames.get() >= 8 && !changed {
                let shell = shell.clone();
                glib::idle_add_local_once(move || {
                    if let Some(shell) = shell.upgrade() {
                        shell.player_view.visualizer.tick.borrow_mut().take();
                    }
                });
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.player_view.visualizer.tick.borrow_mut() = Some(tick);
    }

    fn stop_visualizer_tick(&self) {
        if let Some(tick) = self.player_view.visualizer.tick.borrow_mut().take() {
            tick.remove();
        }
    }

    fn clear_visualizer(&self) {
        self.player_view.visualizer.levels.borrow_mut().clear();
        self.player_view.visualizer.targets.borrow_mut().clear();
        self.player_view.visualizer.fullscreen_area.queue_draw();
        self.player_view.visualizer.sidebar_area.queue_draw();
    }
}

#[cfg(test)]
mod visualizer_tests {
    use super::*;

    #[test]
    fn sidebar_visualizer_fits_all_received_bands() {
        let width = 213.0;
        let gap = 1.0;
        let (columns, cell) = visualizer_column_geometry(width, 52, gap);
        let occupied = columns as f64 * cell + columns.saturating_sub(1) as f64 * gap;

        assert_eq!(columns, 52);
        assert!(cell >= 3.0);
        assert!(occupied <= width * 0.984 + f64::EPSILON);
    }

    #[test]
    fn silent_visualizer_bands_remain_zero() {
        assert_eq!(visualizer_bar_levels(&[0.0; 52], 52), vec![0.0; 52]);
    }
}
