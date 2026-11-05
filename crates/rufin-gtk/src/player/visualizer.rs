use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use rufin_core::settings::visualizer::{
    VISUALIZER_REFERENCE_FRAME_MICROS, VISUALIZER_STALE_MICROS, VisualizerAppearance,
    VisualizerPeak as Peak, VisualizerStyle, visualizer_accent_gradient, visualizer_bar_levels,
    visualizer_column_geometry, visualizer_lerp as lerp, visualizer_radial_shadow_geometry,
    visualizer_row_geometry, visualizer_shadow_geometry, visualizer_smoothed_level,
    visualizer_smoothing_weights, visualizer_spectrum_levels,
};

pub struct VisualizerParts {
    pub sidebar_area: Visualizer,
    pub fullscreen_area: Visualizer,
    pub levels: Rc<RefCell<Vec<f64>>>,
    pub targets: Rc<RefCell<Vec<f64>>>,
    pub generation: Rc<Cell<u64>>,
    pub tick: RefCell<Option<gtk::TickCallbackId>>,
    pub active: Cell<bool>,
    pub appearance: Rc<RefCell<VisualizerAppearance>>,
    peaks: Rc<RefCell<Vec<Peak>>>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Visualizer {
        pub(super) levels: OnceCell<Rc<RefCell<Vec<f64>>>>,
        pub(super) appearance: OnceCell<Rc<RefCell<VisualizerAppearance>>>,
        pub(super) peaks: OnceCell<Rc<RefCell<Vec<Peak>>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Visualizer {
        const NAME: &'static str = "RufinVisualizer";
        type Type = super::Visualizer;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Visualizer {}

    impl WidgetImpl for Visualizer {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let appearance = self.appearance.get().unwrap().borrow();
            let (main_height, shadow_height) =
                visualizer_shadow_geometry(f64::from(widget.height().max(1)), &appearance);
            // The device pixel grid, which the coverage geometry aligns with.
            let native = widget.native();
            let scale = native
                .as_ref()
                .and_then(|native| native.surface())
                .map_or(f64::from(widget.scale_factor()), |surface| surface.scale());
            let origin = native
                .as_ref()
                .and_then(|native| {
                    let (x, y) = native.surface_transform();
                    widget
                        .compute_point(native, &gtk::graphene::Point::zero())
                        .map(|point| (f64::from(point.x()) + x, f64::from(point.y()) + y))
                })
                .unwrap_or_default();
            let columns = Axis {
                scale,
                offset: origin.0 * scale,
            };
            let draw = |snapshot: &gtk::Snapshot, rows: Axis| {
                draw_visualizer(
                    snapshot,
                    widget.width(),
                    main_height,
                    &self.levels.get().unwrap().borrow(),
                    &self.peaks.get().unwrap().borrow(),
                    &appearance,
                    widget.color(),
                    (columns, rows),
                );
            };
            draw(
                snapshot,
                Axis {
                    scale,
                    offset: origin.1 * scale,
                },
            );
            if shadow_height == 0.0 {
                return;
            }
            // The reflection is drawn again for the squashed grid it lands on, so its edges
            // get the same antialiasing as the visualizer's.
            let squash = shadow_height / main_height;
            snapshot.push_clip(&rect(
                0.0,
                0.0,
                widget.width().into(),
                widget.height().into(),
            ));
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(
                0.0,
                (main_height + shadow_height) as f32,
            ));
            snapshot.scale(1.0, -squash as f32);
            let brightness = appearance.shadow_brightness as f32;
            snapshot.push_opacity(appearance.shadow_opacity);
            snapshot.push_color_matrix(
                &gtk::graphene::Matrix::new_scale(brightness, brightness, brightness),
                &gtk::graphene::Vec4::zero(),
            );
            draw(
                snapshot,
                Axis {
                    scale: -squash * scale,
                    offset: (origin.1 + main_height + shadow_height) * scale,
                },
            );
            snapshot.pop();
            snapshot.pop();
            snapshot.restore();
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Visualizer(ObjectSubclass<imp::Visualizer>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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
) -> Visualizer {
    let area: Visualizer = glib::Object::new();
    area.imp().levels.set(levels).ok().unwrap();
    area.imp().appearance.set(appearance).ok().unwrap();
    area.imp().peaks.set(peaks).ok().unwrap();
    area.add_css_class("fullscreen-player-visualizer-area");
    area.add_css_class("visualizer-accent");
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_halign(gtk::Align::Fill);
    area.set_valign(gtk::Align::Fill);
    area
}

fn draw_visualizer(
    snapshot: &gtk::Snapshot,
    width: i32,
    height: f64,
    levels: &[f64],
    peaks: &[Peak],
    appearance: &VisualizerAppearance,
    accent: gtk::gdk::RGBA,
    grid: (Axis, Axis),
) {
    if levels.len() < 2 {
        return;
    }
    let width = f64::from(width.max(1));
    let height = height.max(1.0);
    let column_gap = appearance.spacing;
    let row_gap = 2.0;
    let left = width * 0.008;
    let (columns, cell) = visualizer_column_geometry(
        width,
        appearance.bar_count.clamp(8, 256) as usize,
        column_gap,
    );
    let (rows, row_height) = visualizer_row_geometry(height, cell, row_gap);
    let row_stride = row_height + row_gap;
    let bottom = height;
    let bars = visualizer_bar_levels(levels, columns, appearance.mirrored);
    let colors = appearance.colors.unwrap_or_else(|| accent_gradient(accent));
    let shadow_colors = colors.map(|rgb| {
        rgb.map(|component| {
            (f64::from(component) * appearance.shadow_brightness).clamp(0.0, 1.0) as f32
        })
    });
    if bars.is_empty() {
        return;
    }

    let peak_bars = if appearance.peaks {
        let peak_levels = peaks.iter().map(|peak| peak.level).collect::<Vec<_>>();
        visualizer_bar_levels(&peak_levels, columns, appearance.mirrored)
    } else {
        Vec::new()
    };
    let graph_height = rows as f64 * row_stride - row_gap;
    let bounds = rect(0.0, 0.0, width, height);
    snapshot.push_clip(&bounds);
    let gradient = |bounds: &gtk::graphene::Rect| {
        snapshot.append_linear_gradient(
            bounds,
            &gtk::graphene::Point::new(0.0, bottom as f32),
            &gtk::graphene::Point::new(0.0, (bottom - graph_height) as f32),
            &[
                gtk::gsk::ColorStop::new(0.0, color(colors, 0.0, 1.0)),
                gtk::gsk::ColorStop::new(1.0, color(colors, 1.0, 1.0)),
            ],
        );
    };
    let center = (width / 2.0, height / 2.0);
    let radius = width.min(height) * 0.22;
    let extent = width.min(height) * 0.25;
    let (shadow_scale, shadow_limit) =
        visualizer_radial_shadow_geometry(radius, extent, appearance);
    let angle = |column: usize| {
        (column as f64 + if appearance.mirrored { 0.5 } else { 0.0 }) / columns as f64
            * std::f64::consts::TAU
            - std::f64::consts::FRAC_PI_2
    };
    if matches!(
        appearance.style,
        VisualizerStyle::Line
            | VisualizerStyle::Filled
            | VisualizerStyle::Ribbon
            | VisualizerStyle::Halo
    ) {
        // These styles draw the color source through a coverage mask of gradient nodes,
        // which the GPU renders. A Cairo node or a GSK path is rasterized on the CPU every
        // frame, and GTK caches each new animated path.
        let ribbon = appearance.style == VisualizerStyle::Ribbon;
        let halo = appearance.style == VisualizerStyle::Halo;
        let points = |inward: bool| {
            bars.iter()
                .enumerate()
                .map(|(column, level)| {
                    if halo {
                        let radius = radius
                            + if inward {
                                -(level * shadow_scale).min(shadow_limit)
                            } else {
                                level * extent
                            };
                        let (sin, cos) = angle(column).sin_cos();
                        (center.0 + radius * cos, center.1 + radius * sin)
                    } else {
                        (
                            left + cell / 2.0 + column as f64 * (cell + column_gap),
                            if ribbon {
                                height / 2.0 - level * graph_height / 2.0
                            } else {
                                bottom - level * graph_height
                            },
                        )
                    }
                })
                .collect::<Vec<_>>()
        };
        let outline = points(false);
        let pixels = (grid.0.pixel(), grid.1.pixel());
        let pixel = pixels.0.min(pixels.1);
        let closed_curve = |controls: &[(f64, f64)]| {
            let start = midpoint(controls[controls.len() - 1], controls[0]);
            let mut curve = thin(smooth_curve(start, controls, start, pixel), pixel);
            curve.pop();
            curve
        };
        let snap_x = |value| grid.0.snap(value);
        snapshot.push_mask(gtk::gsk::MaskMode::Alpha);
        match appearance.style {
            VisualizerStyle::Filled => fill(snapshot, &[(&outline, bottom, true)], pixels, snap_x),
            VisualizerStyle::Ribbon => {
                let middle = height / 2.0;
                let top = smooth_curve(
                    (outline[0].0, middle),
                    &outline,
                    (outline[outline.len() - 1].0, middle),
                    pixel,
                );
                let mirrored = top
                    .iter()
                    .map(|&(x, y)| (x, height - y))
                    .collect::<Vec<_>>();
                let base = grid.1.snap(middle);
                fill(
                    snapshot,
                    &[(&top, base, true), (&mirrored, base, false)],
                    pixels,
                    snap_x,
                );
            }
            _ if halo => stroke(snapshot, &closed_curve(&outline), true, pixels),
            _ => stroke(snapshot, &outline, false, pixels),
        }
        snapshot.pop();
        gradient(&bounds);
        snapshot.pop();
        if halo && shadow_limit > 0.0 {
            snapshot.push_mask(gtk::gsk::MaskMode::Alpha);
            stroke(snapshot, &closed_curve(&points(true)), true, pixels);
            snapshot.pop();
            snapshot.append_color(
                &color(shadow_colors, 0.5, appearance.shadow_opacity),
                &bounds,
            );
            snapshot.pop();
        }
    }
    for (column, level) in bars.iter().copied().enumerate() {
        let scaled = level * rows as f64;
        let x = left + column as f64 * (cell + column_gap);
        let bar_height = level * graph_height;
        if matches!(
            appearance.style,
            VisualizerStyle::Circular | VisualizerStyle::Halo
        ) {
            let angle = angle(column);
            let (sin, cos) = angle.sin_cos();
            let shadow_length = (level * shadow_scale).min(shadow_limit);
            let shadow_color = color(shadow_colors, level, appearance.shadow_opacity);
            let color = color(colors, level, 1.0);
            let thickness = (std::f64::consts::TAU * radius / columns as f64 - column_gap).max(1.0);
            if level > 0.0 && appearance.style == VisualizerStyle::Circular {
                snapshot.save();
                snapshot.translate(&gtk::graphene::Point::new(center.0 as f32, center.1 as f32));
                snapshot.rotate(angle.to_degrees() as f32);
                if shadow_length > 0.0 {
                    snapshot.append_color(
                        &shadow_color,
                        &rect(
                            radius - shadow_length,
                            -thickness / 2.0,
                            shadow_length,
                            thickness,
                        ),
                    );
                }
                snapshot.append_color(
                    &color,
                    &rect(radius, -thickness / 2.0, extent * level, thickness),
                );
                snapshot.restore();
            }
            if appearance.peaks
                && let Some(peak) = peak_bars.get(column).filter(|peak| **peak > 0.0)
            {
                let bounds = rect(
                    center.0 + cos * (radius + extent * peak) - 1.5,
                    center.1 + sin * (radius + extent * peak) - 1.5,
                    3.0,
                    3.0,
                );
                snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(bounds, 1.5));
                snapshot.append_color(&color, &bounds);
                snapshot.pop();
                let shadow_peak = peak * shadow_scale;
                if shadow_peak > 0.0 && shadow_peak <= shadow_limit {
                    let bounds = rect(
                        center.0 + cos * (radius - shadow_peak) - 1.5,
                        center.1 + sin * (radius - shadow_peak) - 1.5,
                        3.0,
                        3.0,
                    );
                    snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(bounds, 1.5));
                    snapshot.append_color(&shadow_color, &bounds);
                    snapshot.pop();
                }
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
            if bar_height > 0.0 {
                match appearance.style {
                    VisualizerStyle::Rounded => {
                        let radius = (cell / 2.0).min(bar_height / 2.0);
                        let bounds = rect(x, y, cell, bar_height);
                        snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(
                            bounds,
                            radius as f32,
                        ));
                        // Let the rounded clip antialias the edge only once.
                        gradient(&rect(x - 1.0, y - 1.0, cell + 2.0, bar_height + 2.0));
                        snapshot.pop();
                    }
                    VisualizerStyle::Outline => {
                        let height = bar_height.max(1.0);
                        gradient(&rect(x, y, cell, 1.0));
                        if height > 1.0 {
                            gradient(&rect(x, y + 1.0, 1.0, height - 1.0));
                            gradient(&rect(x + cell - 1.0, y + 1.0, 1.0, height - 1.0));
                            gradient(&rect(
                                x + 1.0,
                                y + (height - 1.0).max(1.0),
                                (cell - 2.0).max(0.0),
                                height - (height - 1.0).max(1.0),
                            ));
                        }
                    }
                    VisualizerStyle::Solid | VisualizerStyle::Mirrored => {
                        gradient(&rect(x, y, cell, bar_height));
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
                let y = bottom - row_height - row as f64 * row_stride;
                snapshot.append_color(
                    &color(colors, color_t, 0.72 + color_t * 0.24),
                    &rect(x, y, cell, row_height),
                );
            }

            let cap_row = full_cells;
            let cap_alpha = scaled - scaled.floor();
            if cap_row < rows && cap_alpha >= 0.14 {
                let color_t = cap_row as f64 / rows.saturating_sub(1).max(1) as f64;
                let y = bottom - row_height - cap_row as f64 * row_stride;
                snapshot.append_color(
                    &color(colors, color_t, cap_alpha * 0.76),
                    &rect(x, y, cell, row_height),
                );
            }
        }
        if appearance.peaks
            && let Some(peak) = peak_bars.get(column).filter(|peak| **peak > 0.0)
        {
            let color = color(colors, 1.0, 1.0);
            let y = if matches!(
                appearance.style,
                VisualizerStyle::Mirrored | VisualizerStyle::Ribbon
            ) {
                height / 2.0 - peak * graph_height / 2.0
            } else {
                bottom - peak * graph_height
            };
            snapshot.append_color(&color, &rect(x, y, cell, 2.0));
            if matches!(
                appearance.style,
                VisualizerStyle::Mirrored | VisualizerStyle::Ribbon
            ) {
                snapshot.append_color(&color, &rect(x, height - y - 2.0, cell, 2.0));
            }
        }
    }
    snapshot.pop();
}

fn midpoint(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

/// Points along the quadratic B-spline that runs from `start` through the midpoints between
/// `controls` to `end`, within a quarter device pixel of it.
fn smooth_curve(
    start: (f64, f64),
    controls: &[(f64, f64)],
    end: (f64, f64),
    pixel: f64,
) -> Vec<(f64, f64)> {
    let mut curve = vec![start];
    let mut from = start;
    for (index, &control) in controls.iter().enumerate() {
        let to = controls
            .get(index + 1)
            .map_or(end, |&next| midpoint(control, next));
        let bend = (from.0 - 2.0 * control.0 + to.0).hypot(from.1 - 2.0 * control.1 + to.1);
        let steps = (bend / pixel).sqrt().ceil().max(1.0);
        for step in 1..=steps as usize {
            let t = step as f64 / steps;
            let (a, b, c) = ((1.0 - t) * (1.0 - t), 2.0 * t * (1.0 - t), t * t);
            curve.push((
                a * from.0 + b * control.0 + c * to.0,
                a * from.1 + b * control.1 + c * to.1,
            ));
        }
        from = to;
    }
    curve
}

/// Drops points closer than a device pixel to the previous one, keeping both ends.
fn thin(points: Vec<(f64, f64)>, pixel: f64) -> Vec<(f64, f64)> {
    let mut kept: Vec<(f64, f64)> = Vec::with_capacity(points.len());
    for (index, &(x, y)) in points.iter().enumerate() {
        let replaces_end = index + 1 == points.len() && kept.len() > 1;
        match kept.last_mut() {
            Some(last) if (x - last.0).hypot(y - last.1) < pixel => {
                if replaces_end {
                    *last = (x, y);
                }
            }
            _ => kept.push((x, y)),
        }
    }
    kept
}

/// White coverage between each x-monotone edge and the line y = base, below the edge when
/// the flag is set. Each edge segment is a strip whose slanted side is a gradient ramp. Strips
/// meet on device pixel edges (`snap`), so each pixel takes its coverage from the segment
/// above its center and neighbouring strips leave no seam; a segment narrower than a pixel
/// may get no strip.
fn fill(
    snapshot: &gtk::Snapshot,
    edges: &[(&[(f64, f64)], f64, bool)],
    pixel: (f64, f64),
    snap: impl Fn(f64) -> f64,
) {
    for &(edge, base, below) in edges {
        let (first, last) = (edge[0].0, edge[edge.len() - 1].0);
        for pair in edge.windows(2) {
            let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
            let left = if x0 == first { x0 } else { snap(x0).max(first) };
            let right = if x1 == last { x1 } else { snap(x1).min(last) };
            if right <= left {
                continue;
            }
            let side = if below { 1.0 } else { -1.0 };
            let length = (x1 - x0).hypot(y1 - y0);
            let (nx, ny) = (side * (y0 - y1) / length, side * (x1 - x0) / length);
            let ramp = ramp(nx, ny, pixel);
            let (top, bottom) = if below {
                (y0.min(y1) - ramp, base)
            } else {
                (base, y0.max(y1) + ramp)
            };
            snapshot.append_linear_gradient(
                &rect(left, top, right - left, bottom - top),
                &point(x0 - nx * ramp, y0 - ny * ramp),
                &point(x0 + nx * ramp, y0 + ny * ramp),
                &[stop(0.0, 0.0), stop(1.0, 1.0)],
            );
        }
    }
}

/// Half the width of the antialiasing ramp across an edge with normal (nx, ny), for device
/// pixels of size `pixel`. GSK samples gradients four times per pixel, a quarter pixel apart,
/// which widens the ramp to about the one-pixel footprint Cairo gives the same edge.
fn ramp(nx: f64, ny: f64, pixel: (f64, f64)) -> f64 {
    (nx.abs() * pixel.0 + ny.abs() * pixel.1) / 4.0
}

/// White coverage of a 2 px wide stroke along `points` as Cairo strokes it: butt ends, miter
/// joins up to a miter length of 10 widths and bevels beyond. Each segment is a rotated rect
/// whose sides are gradient ramps. Segments reach as far as the miter where it is shorter
/// than the width, and half a device pixel into the next segment so their antialiased ends
/// leave no seam. A longer miter tip is one more rect, cut by the outer side of the next
/// segment.
fn stroke(snapshot: &gtk::Snapshot, points: &[(f64, f64)], closed: bool, pixel: (f64, f64)) {
    const HALF: f64 = 1.0;
    // The length of one device pixel along a unit direction.
    let along = |dx: f64, dy: f64| (dx / pixel.0).hypot(dy / pixel.1).recip();
    let count = points.len();
    let direction = |index: usize| {
        let (a, b) = (points[index % count], points[(index + 1) % count]);
        let length = (b.0 - a.0).hypot(b.1 - a.1);
        ((b.0 - a.0) / length, (b.1 - a.1) / length, length)
    };
    // How far the segments before and after a vertex reach past it, and the miter tip they
    // leave. The one before overlaps the one after by half a device pixel.
    let joint = |vertex: usize| {
        if !closed && (vertex == 0 || vertex == count - 1) {
            return (0.0, 0.0, None);
        }
        let (ax, ay, _) = direction(vertex + count - 1);
        let (bx, by, _) = direction(vertex);
        let (sin, cos) = (ax * by - ay * bx, ax * bx + ay * by);
        let turn = sin.abs().atan2(cos);
        // Cairo applies the miter limit to the turn on device pixels, which differs from the
        // local turn in the squashed reflection.
        let device = |x: f64, y: f64| (x / pixel.0, y / pixel.1);
        let ((dax, day), (dbx, dby)) = (device(ax, ay), device(bx, by));
        let device_cos = (dax * dbx + day * dby) / (dax.hypot(day) * dbx.hypot(dby));
        if (1.0 + device_cos) / 2.0 < 0.1 * 0.1 {
            return (0.0, 0.0, None);
        }
        let miter = HALF * (turn / 2.0).tan();
        let overlap = along(ax, ay) / 2.0;
        if turn <= std::f64::consts::FRAC_PI_2 {
            (miter.max(overlap), miter, None)
        } else {
            (overlap, 0.0, Some((miter, sin, cos)))
        }
    };
    let segments = if closed { count } else { count - 1 };
    for index in 0..segments {
        let (tx, ty, length) = direction(index);
        let (start, end) = (joint(index).1, joint(index + 1).0);
        // GSK antialiases a rect edge just past a whole local pixel, so the open ends sit
        // there, and the last segment is placed from its end.
        let step = along(tx, ty) * 1e-3;
        let (anchor, from, to) = if !closed && index + 1 == segments {
            (points[index + 1], -length - start, step)
        } else if !closed && index == 0 {
            (points[index], -step, length + end)
        } else {
            (points[index], -start, length + end)
        };
        let ramp = ramp(-ty, tx, pixel);
        let reach = HALF + ramp;
        let edge = (ramp / reach) as f32;
        let margin = along(-ty, tx);
        snapshot.save();
        snapshot.translate(&point(anchor.0, anchor.1));
        snapshot.rotate(ty.atan2(tx).to_degrees() as f32);
        snapshot.append_linear_gradient(
            &rect(from, -reach - margin, to - from, 2.0 * (reach + margin)),
            &point(0.0, -reach),
            &point(0.0, reach),
            &[
                stop(0.0, 0.0),
                stop(edge, 1.0),
                stop(1.0 - edge, 1.0),
                stop(1.0, 0.0),
            ],
        );
        snapshot.restore();
    }
    for vertex in 0..count {
        let Some((miter, sin, cos)) = joint(vertex).2 else {
            continue;
        };
        let (ax, ay, _) = direction(vertex + count - 1);
        let (bx, by, _) = direction(vertex);
        // In the incoming segment's frame, the outer side is the one the path turns away
        // from, and the next segment's outer side has this normal.
        let outer = -sin.signum();
        let (nx, ny) = (outer * -sin, outer * cos);
        let ramp = ramp(-by, bx, pixel);
        let step = along(-ay, ax) * 1e-3;
        let (low, high) = if outer > 0.0 {
            (-HALF, HALF + step)
        } else {
            (-HALF - step, HALF)
        };
        snapshot.save();
        snapshot.translate(&point(points[vertex].0, points[vertex].1));
        snapshot.rotate(ay.atan2(ax).to_degrees() as f32);
        snapshot.append_linear_gradient(
            &rect(0.0, low, miter + 2.0 * along(ax, ay), high - low),
            &point(nx * (HALF - ramp), ny * (HALF - ramp)),
            &point(nx * (HALF + ramp), ny * (HALF + ramp)),
            &[stop(0.0, 1.0), stop(1.0, 0.0)],
        );
        snapshot.restore();
    }
}

/// How local coordinates on one axis map to device pixels: `local * scale + offset`.
#[derive(Clone, Copy)]
struct Axis {
    scale: f64,
    offset: f64,
}

impl Axis {
    /// The local length of one device pixel.
    fn pixel(self) -> f64 {
        self.scale.abs().recip()
    }

    /// The device pixel edge nearest to `value`.
    fn snap(self, value: f64) -> f64 {
        ((value * self.scale + self.offset).round() - self.offset) / self.scale
    }
}

fn point(x: f64, y: f64) -> gtk::graphene::Point {
    gtk::graphene::Point::new(x as f32, y as f32)
}

fn stop(offset: f32, alpha: f32) -> gtk::gsk::ColorStop {
    gtk::gsk::ColorStop::new(offset, gtk::gdk::RGBA::new(1.0, 1.0, 1.0, alpha))
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> gtk::graphene::Rect {
    gtk::graphene::Rect::new(x as f32, y as f32, width as f32, height as f32)
}

pub(super) fn accent_gradient(accent: gtk::gdk::RGBA) -> [[f32; 3]; 2] {
    visualizer_accent_gradient([accent.red(), accent.green(), accent.blue()])
}

fn color(colors: [[f32; 3]; 2], mix: f64, alpha: f64) -> gtk::gdk::RGBA {
    let color: [f64; 3] =
        std::array::from_fn(|index| lerp(colors[0][index].into(), colors[1][index].into(), mix));
    gtk::gdk::RGBA::new(
        color[0] as f32,
        color[1] as f32,
        color[2] as f32,
        alpha as f32,
    )
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
        for (count, expected_columns) in [(8, 8), (24, 24), (52, 52), (96, 70), (128, 70)] {
            let (columns, cell) = visualizer_column_geometry(width, count, gap);
            let occupied = columns as f64 * cell + columns.saturating_sub(1) as f64 * gap;
            assert_eq!(columns, expected_columns);
            if count == 52 {
                assert!(cell >= 3.0);
            }
            assert_eq!(
                visualizer_bar_levels(&[0.5; 52], columns, false),
                vec![0.5; columns]
            );
            assert!(cell >= 2.0);
            assert!(occupied <= width * 0.984 + f64::EPSILON);
            let (wide_columns, _) = visualizer_column_geometry(1200.0, count, gap);
            assert_eq!(wide_columns, count);
        }
        let interpolated = visualizer_bar_levels(&[0.0, 1.0], 3, false);
        assert_eq!(interpolated, vec![0.0, 0.5, 1.0]);
        let mut appearance = VisualizerAppearance::default();
        assert_eq!(visualizer_shadow_geometry(400.0, &appearance), (400.0, 0.0));
        assert_eq!(
            visualizer_bar_levels(&[0.0, 1.0], 4, true),
            vec![1.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(
            visualizer_bar_levels(&[0.0, 0.5, 1.0], 5, true),
            vec![1.0, 0.5, 0.0, 0.5, 1.0]
        );
        appearance.shadow = true;
        assert_eq!(
            visualizer_shadow_geometry(400.0, &appearance),
            (300.0, 100.0)
        );
        appearance.shadow_fit = false;
        assert_eq!(
            visualizer_shadow_geometry(400.0, &appearance),
            (300.0, 300.0)
        );
        appearance.shadow_ratio = 0.95;
        let (main, _) = visualizer_shadow_geometry(400.0, &appearance);
        let (rows, row_height) = visualizer_row_geometry(main / 2.0, 4.0, 2.0);
        assert!(rows as f64 * (row_height + 2.0) - 2.0 <= main / 2.0);
        appearance.style = VisualizerStyle::Circular;
        appearance.shadow_ratio = 0.25;
        appearance.shadow_fit = true;
        assert_eq!(visualizer_shadow_geometry(400.0, &appearance), (400.0, 0.0));
        assert_eq!(
            visualizer_radial_shadow_geometry(88.0, 100.0, &appearance),
            (22.0, 22.0)
        );
        appearance.shadow_fit = false;
        assert_eq!(
            visualizer_radial_shadow_geometry(88.0, 100.0, &appearance),
            (100.0, 22.0)
        );
        appearance.shadow = false;
        assert_eq!(
            visualizer_radial_shadow_geometry(88.0, 100.0, &appearance),
            (0.0, 0.0)
        );
    }

    #[test]
    pub fn silent_visualizer_bands_remain_zero() {
        assert_eq!(visualizer_bar_levels(&[0.0; 52], 52, false), vec![0.0; 52]);
    }
}

use gtk::glib;
use playback::TransportStatus;
impl crate::player::PlayerUi {
    pub fn apply_visualizer_appearance(&self) {
        let Some(shell) = self.shell() else {
            return;
        };
        let appearance = shell
            .settings
            .current
            .borrow()
            .visualizer
            .appearance
            .clone();
        let sidebar_opacity = appearance.opacity
            * if self.lyrics.panel_visible.get()
                && shell.settings.current.borrow().right_panel.combined
            {
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
        let Some(shell) = self.shell() else {
            return;
        };
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
        let visible = fullscreen_visible || sidebar_visible;
        shell.products.playback.set_visualizer_enabled(visible);
        let active = visible
            && self.selected_playback().as_deref().is_some_and(|player| {
                matches!(
                    player.transport.effective_state(),
                    TransportStatus::Playing | TransportStatus::Buffering
                )
            });
        let changed = self.views.visualizer.active.replace(active) != active;
        if active {
            self.start_visualizer_tick();
            return;
        }
        if changed {
            self.stop_visualizer_tick();
            self.clear_visualizer();
        }
    }

    fn start_visualizer_tick(self: &Rc<Self>) {
        let Some(shell) = self.shell() else {
            return;
        };
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
        let window = shell.chrome.window.clone();
        let settings = Rc::clone(&shell.settings);
        let shell = Rc::downgrade(self);
        let seen_generation = Cell::new(generation.get());
        let last_data_time = Cell::new(None);
        let last_frame_time = Cell::new(None);
        let next_frame_time = Cell::new(None::<f64>);
        let last_limit = Cell::new(0);
        let tick = window.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            if shell.strong_count() == 0 {
                return glib::ControlFlow::Break;
            }
            let limit = settings.current.borrow().visualizer.fps_limit.clamp(1, 180);
            if last_limit.replace(limit) != limit {
                next_frame_time.set(None);
            }
            let deadline = next_frame_time.get().unwrap_or(now as f64);
            if (now as f64) < deadline {
                return glib::ControlFlow::Continue;
            }
            // Carry fractional frame time so caps need not divide the display refresh rate.
            let interval = 1_000_000.0 / f64::from(limit);
            next_frame_time.set(Some((deadline + interval).max(now as f64)));
            let elapsed_frames = last_frame_time.replace(Some(now)).map_or(1.0, |last| {
                (now - last) as f64 / VISUALIZER_REFERENCE_FRAME_MICROS
            });
            let appearance = appearance.borrow();
            let (rise_weight, fall_weight) =
                visualizer_smoothing_weights(elapsed_frames, appearance.rise, appearance.fall);
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
            let target = visualizer_spectrum_levels(&targets.borrow(), &appearance);
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
                let smoothed = visualizer_smoothed_level(value, next, weight);
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
