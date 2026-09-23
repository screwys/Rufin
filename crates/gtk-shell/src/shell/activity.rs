use super::Shell;
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use gtk_library::activity::ActivityCollections;
use gtk_player::fullscreen_background::FullscreenBackground;
use gtk_widgets::{
    artwork::{ArtworkTile, cover_fetch_size_for_display},
    mounted_route::route_current_track,
    route::Route,
};
use library::{ActivityOverview, CalendarActivityPeriod};
use localization::{tr, tr_with};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

pub(super) struct ActivityView {
    root: gtk::Box,
    report: gtk::Overlay,
    content: adw::Clamp,
    outgoing_content: gtk::Picture,
    transition: adw::TimedAnimation,
    background: FullscreenBackground,
    collections: ActivityCollections,
    sections: [gtk::Box; 4],
    values: [gtk::Label; 4],
    changes: [gtk::Label; 4],
    title: gtk::Label,
    status: gtk::Label,
    empty_text: gtk::Label,
    error_text: gtk::Label,
    genres: gtk::Box,
    genre_rows: Vec<(gtk::Box, gtk::Label, gtk::Label, gtk::ProgressBar)>,
    period_kind: gtk::DropDown,
    period: gtk::DropDown,
    previous: gtk::Button,
    next: gtk::Button,
    export: gtk::Button,
    export_dialog: gtk::FileDialog,
    months: RefCell<Vec<(i32, u8)>>,
    periods: RefCell<Vec<CalendarActivityPeriod>>,
    changing_period: Cell<bool>,
    data: RefCell<Option<ActivityOverview>>,
    task: RefCell<Option<tokio::task::AbortHandle>>,
    bg_tile: ArtworkTile,
    shell: Weak<Shell>,
}

impl Drop for ActivityView {
    fn drop(&mut self) {
        self.transition.skip();
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
        self.bg_tile.cancel_artwork_request();
    }
}

impl Shell {
    pub(super) fn open_startup_activity(self: &Rc<Self>) {
        let now = glib::DateTime::now_local().expect("local clock");
        let periods = self
            .settings
            .current
            .borrow()
            .activity_overview
            .startup_periods(&now);
        if periods.iter().all(Option::is_none) {
            return;
        }
        let db = self.products.library.clone();
        let (sender, receiver) = async_channel::bounded(1);
        self.products.runtime.spawn(async move {
            let _ = sender.send(db.activity_months().await).await;
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(Ok(months)) = receiver.recv().await else {
                return;
            };
            let Some(shell) = weak.upgrade() else {
                return;
            };
            let period = periods.into_iter().flatten().find(|period| match period {
                CalendarActivityPeriod::Month { .. } => months.contains(&period_key(*period)),
                CalendarActivityPeriod::Year(year) => months
                    .iter()
                    .any(|month| month.starts_with(&format!("{year}-"))),
                CalendarActivityPeriod::Lifetime => false,
            });
            let Some(period) = period else {
                return;
            };
            shell.open_activity(Some(period));
            shell
                .settings
                .update_app_settings("automatic activity overview", |settings| {
                    let settings = &mut settings.activity_overview;
                    match period {
                        CalendarActivityPeriod::Month { .. } => {
                            settings.opened_month = Some(period_key(period))
                        }
                        CalendarActivityPeriod::Year(year) => settings.opened_year = Some(year),
                        CalendarActivityPeriod::Lifetime => {}
                    }
                    true
                });
        });
    }

    pub(crate) fn open_activity(self: &Rc<Self>, requested: Option<CalendarActivityPeriod>) {
        self.close_activity();
        self.player_ui.close_fullscreen_player();
        let view = ActivityView::new(self);
        self.chrome.app_content_overlay.add_overlay(&view.root);
        self.chrome
            .app_content_overlay
            .set_measure_overlay(&view.root, false);
        self.chrome
            .app_content_overlay
            .set_clip_overlay(&view.root, true);
        self.activity.replace(Some(Rc::clone(&view)));
        view.root.child_focus(gtk::DirectionType::TabForward);
        view.load_periods(requested);
    }

    pub(crate) fn close_activity(&self) -> bool {
        let view = self.activity.borrow_mut().take();
        if let Some(view) = view {
            self.chrome.app_content_overlay.remove_overlay(&view.root);
            true
        } else {
            false
        }
    }

    pub(crate) fn refresh_activity_playback(&self) {
        if let Some(view) = self.activity.borrow().as_ref() {
            view.refresh_playback();
        }
    }
}

impl ActivityView {
    fn new(shell: &Rc<Shell>) -> Rc<Self> {
        let resource = crate::ui_resource::ACTIVITY_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            root: gtk::Box, report: gtk::Overlay, content_clamp: adw::Clamp,
            outgoing_content: gtk::Picture,
            report_host: adw::Bin, heading_row: gtk::Box, artwork_row: gtk::Box, ranking_row: gtk::Box,
            minimize: gtk::Button, period_kind: gtk::DropDown, period: gtk::DropDown,
            previous: gtk::Button, next: gtk::Button, export: gtk::Button,
            title: gtk::Label, status: gtk::Label, empty_text: gtk::Label, error_text: gtk::Label,
            hours_value: gtk::Label, plays_value: gtk::Label, artists_value: gtk::Label, tracks_value: gtk::Label,
            hours_change: gtk::Label, plays_change: gtk::Label, artists_change: gtk::Label, tracks_change: gtk::Label,
            tracks_section: gtk::Box, artists_section: gtk::Box, albums_section: gtk::Box, genres_section: gtk::Box,
            tracks_host: gtk::Box, artists_host: gtk::Box, albums_host: gtk::Box, genres_host: gtk::Box,
            autoplay_first_track: adw::SwitchRow,
            dynamic_background: adw::SwitchRow, background_image: adw::SwitchRow,
            show_tracks: adw::SwitchRow, show_artists: adw::SwitchRow, show_albums: adw::SwitchRow,
            show_genres: adw::SwitchRow, show_comparison: adw::SwitchRow,
            show_rufin_in_headline: adw::SwitchRow,
            result_count: gtk::DropDown, export_dialog: gtk::FileDialog,
        });
        let background = FullscreenBackground::new();
        report.set_child(None::<&gtk::Widget>);
        report.set_child(Some(&background));
        report.add_overlay(&content_clamp);
        report.set_measure_overlay(&content_clamp, true);
        report.add_overlay(&outgoing_content);
        report.set_clip_overlay(&outgoing_content, true);
        let content_weak = content_clamp.downgrade();
        let outgoing_weak = outgoing_content.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            if let Some(content) = content_weak.upgrade() {
                content.set_opacity(value);
            }
            if let Some(outgoing) = outgoing_weak.upgrade() {
                outgoing.set_opacity(1.0 - value);
            }
        });
        let transition = adw::TimedAnimation::new(&report, 0.0, 1.0, 220, target);
        let outgoing_weak = outgoing_content.downgrade();
        transition.connect_done(move |_| {
            if let Some(outgoing) = outgoing_weak.upgrade() {
                outgoing.set_visible(false);
                outgoing.set_paintable(None::<&gdk::Paintable>);
            }
        });
        let columns = [heading_row, artwork_row, ranking_row].map(|row| row.downgrade());
        let owner = gtk_widgets::layout::width_allocation_owner(&report, move |width| {
            let orientation = if width < 760 {
                gtk::Orientation::Vertical
            } else {
                gtk::Orientation::Horizontal
            };
            for row in &columns {
                if let Some(row) = row.upgrade() {
                    if row.orientation() != orientation {
                        row.set_orientation(orientation);
                    }
                    row.set_homogeneous(orientation == gtk::Orientation::Horizontal);
                }
            }
        });
        report_host.set_child(Some(&owner));
        let collections = ActivityCollections::new(shell.build_catalog(&Route::History, None));
        tracks_host.append(&collections.tracks);
        artists_host.append(&collections.artists);
        albums_host.append(&collections.albums);
        let genre_rows = (0..10).map(|_| {
            let resource = crate::ui_resource::ACTIVITY_GENRE_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            gtk_widgets::objects!(builder, resource, { row: gtk::Box, name: gtk::Label, count: gtk::Label, bar: gtk::ProgressBar });
            genres_host.append(&row);
            (row, name, count, bar)
        }).collect();
        let view = Rc::new(Self {
            root,
            report,
            content: content_clamp,
            outgoing_content,
            transition,
            background,
            collections,
            sections: [
                tracks_section,
                artists_section,
                albums_section,
                genres_section,
            ],
            values: [hours_value, plays_value, artists_value, tracks_value],
            changes: [hours_change, plays_change, artists_change, tracks_change],
            title,
            status,
            empty_text,
            error_text,
            genres: genres_host,
            genre_rows,
            period_kind,
            period,
            previous,
            next,
            export,
            export_dialog,
            months: RefCell::new(Vec::new()),
            periods: RefCell::new(Vec::new()),
            changing_period: Cell::new(false),
            data: RefCell::new(None),
            task: RefCell::new(None),
            bg_tile: ArtworkTile::new(256),
            shell: Rc::downgrade(shell),
        });
        let weak = Rc::downgrade(shell);
        minimize.connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.close_activity();
            }
        });
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(shell);
        key.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape
                && let Some(shell) = weak.upgrade()
            {
                shell.close_activity();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        view.root.add_controller(key);
        let settings = shell.settings.current.borrow().activity_overview.clone();
        background_image.set_sensitive(settings.dynamic_background);
        let image_setting = background_image.downgrade();
        dynamic_background.connect_active_notify(move |row| {
            if let Some(image) = image_setting.upgrade() {
                image.set_sensitive(row.is_active());
            }
        });
        let rows = [
            autoplay_first_track,
            dynamic_background,
            background_image,
            show_tracks,
            show_artists,
            show_albums,
            show_genres,
            show_comparison,
            show_rufin_in_headline,
        ];
        let active = [
            settings.autoplay_first_track,
            settings.dynamic_background,
            settings.background_image,
            settings.tracks,
            settings.artists,
            settings.albums,
            settings.genres,
            settings.show_comparison,
            settings.show_rufin_in_headline,
        ];
        for (index, (row, active)) in rows.into_iter().zip(active).enumerate() {
            row.set_active(active);
            let weak = Rc::downgrade(&view);
            row.connect_active_notify(move |row| {
                let Some(view) = weak.upgrade() else {
                    return;
                };
                let Some(shell) = view.shell.upgrade() else {
                    return;
                };
                shell
                    .settings
                    .update_app_settings("listening overview", |settings| {
                        let settings = &mut settings.activity_overview;
                        let target = match index {
                            0 => &mut settings.autoplay_first_track,
                            1 => &mut settings.dynamic_background,
                            2 => &mut settings.background_image,
                            3 => &mut settings.tracks,
                            4 => &mut settings.artists,
                            5 => &mut settings.albums,
                            6 => &mut settings.genres,
                            7 => &mut settings.show_comparison,
                            _ => &mut settings.show_rufin_in_headline,
                        };
                        let changed = *target != row.is_active();
                        *target = row.is_active();
                        changed
                    });
                view.apply_display_settings();
            });
        }
        let counts = [3, 5, 10];
        gtk_widgets::interactions::keep_parent_grab_for_dropdown(&result_count);
        result_count.set_selected(
            counts
                .iter()
                .position(|count| *count == settings.result_count)
                .unwrap_or(0) as u32,
        );
        let weak = Rc::downgrade(&view);
        result_count.connect_selected_notify(move |row| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            let Some(shell) = view.shell.upgrade() else {
                return;
            };
            shell
                .settings
                .update_app_settings("listening overview", |settings| {
                    settings.activity_overview.result_count = counts[row.selected() as usize];
                    true
                });
            view.apply_data();
        });
        let weak = Rc::downgrade(&view);
        view.period_kind.connect_selected_notify(move |_| {
            if let Some(view) = weak.upgrade()
                && !view.changing_period.get()
            {
                let selected = view.selected_period();
                view.set_periods(selected, false);
            }
        });
        let weak = Rc::downgrade(&view);
        view.period.connect_selected_notify(move |_| {
            if let Some(view) = weak.upgrade()
                && !view.changing_period.get()
            {
                view.load(false);
            }
        });
        for (button, delta) in [(&view.previous, 1_i32), (&view.next, -1)] {
            let weak = Rc::downgrade(&view);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    let index = view.period.selected() as i32 + delta;
                    if index >= 0 && (index as usize) < view.periods.borrow().len() {
                        view.period.set_selected(index as u32);
                    }
                }
            });
        }
        let weak = Rc::downgrade(&view);
        view.export.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.export_png();
            }
        });
        let weak = Rc::downgrade(&view);
        view.bg_tile
            .drag_paintable_source()
            .connect_paintable_notify(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.refresh_background();
                }
            });
        view.refresh_background();
        minimize.grab_focus();
        view
    }

    fn selected_period(&self) -> Option<CalendarActivityPeriod> {
        self.periods
            .borrow()
            .get(self.period.selected() as usize)
            .copied()
    }

    fn load_periods(self: &Rc<Self>, requested: Option<CalendarActivityPeriod>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let db = shell.products.library.clone();
        let (sender, receiver) = async_channel::bounded(1);
        let task = shell.products.runtime.spawn(async move {
            let _ = sender.send(db.activity_months().await).await;
        });
        self.task.replace(Some(task.abort_handle()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(view) = weak.upgrade() else {
                return;
            };
            let months = match result {
                Ok(months) => months,
                Err(error) => {
                    tracing::warn!(%error, "Could not load Activity periods");
                    view.status.set_text(&view.error_text.text());
                    return;
                }
            };
            let now = glib::DateTime::now_local().expect("local clock");
            let current = now.year() * 12 + now.month() - 1;
            let first = months
                .iter()
                .filter_map(|month| parse_month(month))
                .map(|(y, m)| y * 12 + i32::from(m) - 1)
                .min()
                .unwrap_or(current);
            view.months.replace(
                (first..=current)
                    .rev()
                    .map(|index| (index / 12, (index % 12 + 1) as u8))
                    .collect(),
            );
            let default = CalendarActivityPeriod::Month {
                year: now.year(),
                month: now.month() as u8,
            };
            let last = default.previous();
            let selected = requested.unwrap_or_else(|| {
                if months.contains(&period_key(last)) {
                    last
                } else {
                    default
                }
            });
            view.changing_period.set(true);
            view.period_kind.set_selected(u32::from(matches!(
                selected,
                CalendarActivityPeriod::Year(_)
            )));
            view.changing_period.set(false);
            view.set_periods(Some(selected), true);
        });
    }

    fn set_periods(self: &Rc<Self>, selected: Option<CalendarActivityPeriod>, autoplay: bool) {
        self.changing_period.set(true);
        let yearly = self.period_kind.selected() == 1;
        let mut periods = Vec::new();
        for &(year, month) in self.months.borrow().iter() {
            let period = if yearly {
                CalendarActivityPeriod::Year(year)
            } else {
                CalendarActivityPeriod::Month { year, month }
            };
            if periods.last() != Some(&period) {
                periods.push(period);
            }
        }
        let labels = periods
            .iter()
            .map(|period| period_label(*period))
            .collect::<Vec<_>>();
        let labels = labels.iter().map(String::as_str).collect::<Vec<_>>();
        self.period.set_model(Some(&gtk::StringList::new(&labels)));
        let index = periods
            .iter()
            .position(|period| Some(*period) == selected)
            .or_else(|| {
                selected.and_then(|selected| {
                    let year = |period| match period {
                        CalendarActivityPeriod::Month { year, .. }
                        | CalendarActivityPeriod::Year(year) => Some(year),
                        CalendarActivityPeriod::Lifetime => None,
                    };
                    periods
                        .iter()
                        .position(|period| year(*period) == year(selected))
                })
            })
            .unwrap_or(0);
        self.periods.replace(periods);
        self.period.set_selected(index as u32);
        self.changing_period.set(false);
        self.load(autoplay);
    }

    fn load(self: &Rc<Self>, autoplay: bool) {
        let Some(period) = self.selected_period() else {
            return;
        };
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if let Some(task) = self.task.borrow_mut().take() {
            task.abort();
        }
        self.export.set_sensitive(false);
        if self.data.borrow().is_none() || !self.report.is_visible() {
            self.report.set_visible(false);
            self.status.set_text(&tr("Loading..."));
            self.status.set_visible(true);
        }
        self.previous
            .set_sensitive((self.period.selected() as usize + 1) < self.periods.borrow().len());
        self.next.set_sensitive(self.period.selected() > 0);
        let db = shell.products.library.clone();
        let (sender, receiver) = async_channel::bounded(1);
        let task = shell.products.runtime.spawn(async move {
            let result = db
                .activity_overview(period, 10, &library::ReadCancellation::new())
                .await;
            let _ = sender.send(result).await;
        });
        self.task.replace(Some(task.abort_handle()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let Some(view) = weak.upgrade() else {
                return;
            };
            if view.selected_period() != Some(period) {
                return;
            }
            match result {
                Ok(data) => {
                    let top = data.tracks.first().map(|row| row.media_uri.clone());
                    view.transition.skip();
                    let animate = view.report.is_visible() && view.report.is_mapped();
                    if animate {
                        let image = gtk::WidgetPaintable::new(Some(&view.content)).current_image();
                        view.outgoing_content.set_paintable(Some(&image));
                        view.outgoing_content
                            .set_height_request(view.content.height());
                    }
                    let first_artwork = data
                        .tracks
                        .first()
                        .and_then(|row| row.artwork_binding.as_deref())
                        .map(artwork::ArtworkBinding::opaque)
                        .unwrap_or_default();
                    view.data.replace(Some(data));
                    if let Some(shell) = view.shell.upgrade() {
                        let fetch_size = cover_fetch_size_for_display(256);
                        shell.artwork.bind_artwork_tile(
                            &view.bg_tile,
                            first_artwork,
                            256,
                            fetch_size,
                        );
                    }
                    view.apply_data();
                    if animate {
                        view.outgoing_content.set_visible(true);
                        view.transition.play();
                    }
                    if autoplay
                        && let Some(shell) = view.shell.upgrade()
                        && shell
                            .settings
                            .current
                            .borrow()
                            .activity_overview
                            .autoplay_first_track
                        && let Some(uri) = top
                    {
                        (shell.media_menus.play_target)(
                            &rufin_core::playback::PlaybackTarget::Track(uri),
                            playback::QueuePlacement::Now,
                            false,
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Could not load Activity overview");
                    view.status.set_text(&view.error_text.text());
                    view.status.set_visible(true);
                    view.report.set_visible(false);
                }
            }
        });
    }

    fn apply_data(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let settings = shell.settings.current.borrow().activity_overview.clone();
        self.apply_display_settings();
        let data = self.data.borrow();
        let Some(data) = data.as_ref() else {
            return;
        };
        let limit = settings.result_count;
        self.collections.apply(data, limit);
        let current = [
            &data.totals.duration_millis,
            &data.totals.plays,
            &data.totals.artists,
            &data.totals.tracks,
        ];
        let previous = [
            &data.previous.duration_millis,
            &data.previous.plays,
            &data.previous.artists,
            &data.previous.tracks,
        ];
        for (index, ((value, change), (current, previous))) in self
            .values
            .iter()
            .zip(&self.changes)
            .zip(current.into_iter().zip(previous))
            .enumerate()
        {
            value.set_text(&if index == 0 {
                format!("{:.1}", *current as f64 / 3_600_000.0)
            } else {
                current.to_string()
            });
            let comparison = percentage_change(*current, *previous);
            change.set_text(&comparison);
            change.set_visible(settings.show_comparison && !comparison.is_empty());
        }
        let maximum = data.genres.first().map_or(1, |(_, count)| *count).max(1) as f64;
        for (index, (row, name, count, bar)) in self.genre_rows.iter().enumerate() {
            let value = data.genres.get(index).filter(|_| index < limit);
            row.set_visible(value.is_some());
            if let Some((genre, plays)) = value {
                name.set_text(genre);
                count.set_text(&plays.to_string());
                bar.set_fraction(*plays as f64 / maximum);
            }
        }
        self.genres.set_visible(!data.genres.is_empty());
        self.status.set_text(&self.empty_text.text());
        self.status.set_visible(data.totals.plays == 0);
        self.report.set_visible(true);
        self.export.set_sensitive(true);
        self.refresh_playback();
    }

    fn apply_display_settings(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let settings = shell.settings.current.borrow().activity_overview.clone();
        if let Some(period) = self.selected_period() {
            let label = period_label(period);
            let args = [("period", label.as_str())];
            self.title.set_text(&if settings.show_rufin_in_headline {
                tr_with("Your {period} in Rufin", &args)
            } else {
                tr_with("Your {period}", &args)
            });
        }
        for change in &self.changes {
            change.set_visible(settings.show_comparison && !change.text().is_empty());
        }
        for (section, visible) in self.sections.iter().zip([
            settings.tracks,
            settings.artists,
            settings.albums,
            settings.genres,
        ]) {
            section.set_visible(visible);
        }
        self.refresh_background();
    }

    pub(super) fn refresh_playback(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let current = route_current_track(shell.player_ui.selected_playback().as_deref());
        self.collections.refresh_playback(current);
    }

    fn refresh_background(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let settings = shell.settings.current.borrow().activity_overview.clone();
        self.background.set_visible(settings.dynamic_background);
        if settings.dynamic_background {
            self.background.update(
                self.bg_tile.drag_paintable_source().paintable(),
                settings.background_image,
            );
        }
    }

    fn export_png(self: &Rc<Self>) {
        self.transition.skip();
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let Some(period) = self.selected_period() else {
            return;
        };
        self.export_dialog
            .set_initial_name(Some(&format!("rufin-{}.png", period_key(period))));
        let view = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(file) = view
                .export_dialog
                .save_future(Some(&shell.chrome.window))
                .await
            else {
                return;
            };
            let Some(renderer) = shell.chrome.window.renderer() else {
                return;
            };
            let snapshot = gtk::Snapshot::new();
            let (width, height) = (view.report.width() as f32, view.report.height() as f32);
            let scale = 1536.0 / width.max(1.0);
            snapshot.scale(scale, scale);
            gtk::WidgetPaintable::new(Some(&view.report)).snapshot(
                &snapshot,
                f64::from(width),
                f64::from(height),
            );
            let Some(node) = snapshot.to_node() else {
                return;
            };
            let texture = renderer.render_texture(
                &node,
                Some(&graphene::Rect::new(
                    0.0,
                    0.0,
                    width * scale,
                    height * scale,
                )),
            );
            let bytes = texture.save_to_png_bytes();
            if let Err((_, error)) = file
                .replace_contents_future(
                    bytes,
                    None,
                    false,
                    gio::FileCreateFlags::REPLACE_DESTINATION,
                )
                .await
            {
                shell
                    .control_feedback
                    .show_feedback_toast(error.to_string());
            }
        });
    }
}

fn parse_month(month: &str) -> Option<(i32, u8)> {
    let (year, month) = month.split_once('-')?;
    Some((year.parse().ok()?, month.parse().ok()?))
}

fn period_key(period: CalendarActivityPeriod) -> String {
    match period {
        CalendarActivityPeriod::Month { year, month } => format!("{year:04}-{month:02}"),
        CalendarActivityPeriod::Year(year) => year.to_string(),
        CalendarActivityPeriod::Lifetime => String::new(),
    }
}

fn period_label(period: CalendarActivityPeriod) -> String {
    match period {
        CalendarActivityPeriod::Month { year, month } => {
            glib::DateTime::from_local(year, i32::from(month), 1, 12, 0, 0.0)
                .and_then(|date| date.format("%B %Y"))
                .map(|text| text.to_string())
                .unwrap_or_else(|_| period_key(period))
        }
        _ => period_key(period),
    }
}

fn percentage_change(current: i64, previous: i64) -> String {
    if previous == 0 {
        return String::new();
    }
    format!(
        "({:+.0}%)",
        (current as f64 - previous as f64) * 100.0 / previous as f64
    )
}
