//! The bottom bar's sleep timer: its button, with the time left while it runs, and its popover.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::glib;
use localization::{tr, tr_with};
use playback::{SleepTimerChoice, SleepTimerView};
use rufin_core::playback::{
    SLEEP_TIMER_MINUTES, SLEEP_TIMER_TRACKS, SleepTimerRequest, sleep_timer_text,
};

use crate::widgets::localization::{bind_widget_accessible_label, bind_widget_tooltip};

/// What the popover holds before a choice: 30 minutes, or the end of this track.
const DEFAULT_MINUTES: u32 = 30;
const DEFAULT_TRACKS: u32 = 1;

pub struct SleepTimerControl {
    button: gtk::MenuButton,
    label: gtk::Label,
    title: gtk::Label,
    running: gtk::Box,
    countdown: gtk::Label,
    caption: gtk::Label,
    extend: gtk::Button,
    mode: adw::ToggleGroup,
    presets: gtk::Grid,
    /// The preset buttons and their values; the one matching the value is highlighted.
    preset_buttons: RefCell<Vec<(u32, gtk::Button)>>,
    custom: adw::SpinRow,
    finish_track: adw::SwitchRow,
    timer: Cell<Option<SleepTimerView>>,
    /// The room the bar leaves beside its actions, for the time left beside the moon.
    spare: Cell<i32>,
    updating: Cell<bool>,
    tick: RefCell<Option<glib::SourceId>>,
    request: RefCell<Option<Box<dyn Fn(SleepTimerRequest)>>>,
}

fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn counts_tracks(mode: &adw::ToggleGroup) -> bool {
    mode.active_name().as_deref() == Some("tracks")
}

impl SleepTimerControl {
    pub fn new(button: &gtk::MenuButton) -> Rc<Self> {
        let resource = "player/sleep_timer.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            popover: gtk::Popover,
            title: gtk::Label,
            running: gtk::Box,
            countdown: gtk::Label,
            caption: gtk::Label,
            extend: gtk::Button,
            off: gtk::Button,
            mode: adw::ToggleGroup,
            presets: gtk::Grid,
            custom: adw::SpinRow,
            start: gtk::Button,
            finish_track: adw::SwitchRow,
        });
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.set_halign(gtk::Align::Center);
        content.append(&gtk::Image::from_icon_name("rufin-sleep-timer-symbolic"));
        let label = gtk::Label::new(None);
        label.add_css_class("sleep-timer-button-label");
        label.add_css_class("numeric");
        label.set_visible(false);
        content.append(&label);
        button.set_child(Some(&content));
        button.set_popover(Some(&popover));
        bind_widget_tooltip(button, "Sleep timer");
        bind_widget_accessible_label(button, "Sleep timer");

        let control = Rc::new(Self {
            button: button.clone(),
            label,
            title,
            running,
            countdown,
            caption,
            extend: extend.clone(),
            mode: mode.clone(),
            presets,
            preset_buttons: RefCell::new(Vec::new()),
            custom: custom.clone(),
            finish_track: finish_track.clone(),
            timer: Cell::new(None),
            spare: Cell::new(0),
            updating: Cell::new(false),
            tick: RefCell::new(None),
            request: RefCell::new(None),
        });
        let weak = Rc::downgrade(&control);
        let on = move |apply: fn(&Self)| {
            let weak = weak.clone();
            move || {
                if let Some(control) = weak.upgrade() {
                    apply(&control);
                }
            }
        };
        let extend_action = on(|control| control.send(SleepTimerRequest::Extend));
        extend.connect_clicked(move |_| extend_action());
        let off_action = on(|control| {
            control.button.popdown();
            control.send(SleepTimerRequest::Off);
        });
        off.connect_clicked(move |_| off_action());
        let start_action = on(Self::start_custom);
        start.connect_clicked(move |_| start_action());
        let value_action = on(Self::highlight_preset);
        custom.connect_value_notify(move |_| value_action());
        let mode_action = on(Self::show_mode);
        mode.connect_active_name_notify(move |_| mode_action());
        let finish_action = on(Self::finish_track_changed);
        finish_track.connect_active_notify(move |_| finish_action());
        // Opening shows the running timer's kind.
        let opened = on(Self::show_timer_kind);
        popover.connect_show(move |_| opened());
        control.show_mode();
        control.update(None);
        control
    }

    pub fn button(&self) -> &gtk::MenuButton {
        &self.button
    }

    pub fn connect_request(&self, request: impl Fn(SleepTimerRequest) + 'static) {
        *self.request.borrow_mut() = Some(Box::new(request));
    }

    fn send(&self, request: SleepTimerRequest) {
        if let Some(send) = self.request.borrow().as_ref() {
            send(request);
        }
    }

    fn start(&self, choice: SleepTimerChoice) {
        self.button.popdown();
        self.send(SleepTimerRequest::Start {
            choice,
            finish_track: matches!(choice, SleepTimerChoice::Minutes { .. })
                && self.finish_track.is_active(),
        });
    }

    fn start_custom(&self) {
        let value = self.custom.value().round().max(1.0) as u32;
        self.start(if counts_tracks(&self.mode) {
            SleepTimerChoice::Tracks { tracks: value }
        } else {
            SleepTimerChoice::Minutes { minutes: value }
        });
    }

    fn finish_track_changed(&self) {
        if self.updating.get() {
            return;
        }
        if let Some(SleepTimerView {
            choice: SleepTimerChoice::Minutes { .. },
            ..
        }) = self.timer.get()
        {
            self.send(SleepTimerRequest::FinishTrack {
                enabled: self.finish_track.is_active(),
            });
        }
    }

    /// The presets and the custom value of the chosen kind.
    fn show_mode(&self) {
        let tracks = counts_tracks(&self.mode);
        while let Some(child) = self.presets.first_child() {
            self.presets.remove(&child);
        }
        self.preset_buttons.borrow_mut().clear();
        let mut column = 0;
        let mut row = 0;
        let choices: Vec<(SleepTimerChoice, String)> = if tracks {
            SLEEP_TIMER_TRACKS
                .iter()
                .map(|&tracks| {
                    let label = if tracks == 1 {
                        tr("End of this track")
                    } else {
                        tracks.to_string()
                    };
                    (SleepTimerChoice::Tracks { tracks }, label)
                })
                .collect()
        } else {
            SLEEP_TIMER_MINUTES
                .iter()
                .map(|&minutes| {
                    (
                        SleepTimerChoice::Minutes { minutes },
                        tr_with("{minutes} min", &[("minutes", &minutes.to_string())]),
                    )
                })
                .collect()
        };
        for (choice, label) in choices {
            let width = if choice == (SleepTimerChoice::Tracks { tracks: 1 }) {
                2
            } else {
                1
            };
            if column + width > 4 {
                column = 0;
                row += 1;
            }
            let value = match choice {
                SleepTimerChoice::Minutes { minutes } => minutes,
                SleepTimerChoice::Tracks { tracks } => tracks,
            };
            let button = gtk::Button::with_label(&label);
            // A preset fills the value; Start starts it.
            let custom = self.custom.downgrade();
            button.connect_clicked(move |_| {
                if let Some(custom) = custom.upgrade() {
                    custom.set_value(f64::from(value));
                }
            });
            self.presets.attach(&button, column, row, width, 1);
            self.preset_buttons.borrow_mut().push((value, button));
            column += width;
        }
        // The running timer's value, or the default of its kind.
        let value = match self.timer.get().map(|timer| timer.choice) {
            Some(SleepTimerChoice::Minutes { minutes }) if !tracks => minutes,
            Some(SleepTimerChoice::Tracks { tracks: count }) if tracks => count,
            _ if tracks => DEFAULT_TRACKS,
            _ => DEFAULT_MINUTES,
        };
        self.updating.set(true);
        self.custom
            .adjustment()
            .set_upper(if tracks { 100.0 } else { 240.0 });
        self.custom.set_value(f64::from(value));
        self.custom
            .set_title(&tr(if tracks { "Tracks" } else { "Minutes" }));
        self.finish_track.set_visible(!tracks);
        self.updating.set(false);
        self.highlight_preset();
    }

    fn highlight_preset(&self) {
        let value = self.custom.value().round() as u32;
        for (preset, button) in self.preset_buttons.borrow().iter() {
            if *preset == value {
                button.add_css_class("suggested-action");
            } else {
                button.remove_css_class("suggested-action");
            }
        }
    }

    fn show_timer_kind(&self) {
        if let Some(timer) = self.timer.get() {
            let kind = match timer.choice {
                SleepTimerChoice::Minutes { .. } => "time",
                SleepTimerChoice::Tracks { .. } => "tracks",
            };
            if self.mode.active_name().as_deref() != Some(kind) {
                // The mode's notify shows its presets.
                self.mode.set_active_name(Some(kind));
                return;
            }
        }
        self.show_mode();
    }

    pub fn set_spare_width(&self, spare: i32) {
        self.spare.set(spare);
        self.refresh();
    }

    pub fn update(self: &Rc<Self>, timer: Option<SleepTimerView>) {
        let changed = self.timer.get().map(|timer| timer.choice) != timer.map(|timer| timer.choice);
        self.timer.set(timer);
        self.running.set_visible(timer.is_some());
        self.title.set_visible(timer.is_none());
        self.updating.set(true);
        if let Some(timer) = timer {
            self.finish_track.set_active(timer.finish_track);
        }
        self.updating.set(false);
        if changed {
            self.show_timer_kind();
        }
        let ticking = timer.is_some_and(|timer| timer.ends_at_unix_millis.is_some());
        let mut tick = self.tick.borrow_mut();
        if ticking && tick.is_none() {
            let weak = Rc::downgrade(self);
            *tick = Some(glib::timeout_add_local(
                Duration::from_secs(1),
                move || match weak.upgrade() {
                    Some(control) => {
                        control.refresh();
                        glib::ControlFlow::Continue
                    }
                    None => glib::ControlFlow::Break,
                },
            ));
        } else if !ticking && let Some(source) = tick.take() {
            source.remove();
        }
        drop(tick);
        self.refresh();
    }

    fn refresh(&self) {
        let Some(timer) = self.timer.get() else {
            self.button.remove_css_class("sleep-timer-active");
            self.button.remove_css_class("sleep-timer-wide");
            self.label.set_visible(false);
            bind_widget_tooltip(&self.button, "Sleep timer");
            return;
        };
        let text = sleep_timer_text(timer, now_unix_millis());
        self.button.add_css_class("sleep-timer-active");
        // The pill adds the text, its gap and its padding to the round button.
        let (text_width, _) = self
            .label
            .create_pango_layout(Some(&text.short))
            .pixel_size();
        let wide = !text.short.is_empty() && self.spare.get() >= text_width + 6 + 18;
        if wide {
            self.button.add_css_class("sleep-timer-wide");
        } else {
            self.button.remove_css_class("sleep-timer-wide");
        }
        self.label.set_text(&text.short);
        self.label.set_visible(wide);
        self.button
            .set_tooltip_text(Some(&format!("{}\n{}", tr("Sleep timer"), text.caption)));
        self.countdown.set_text(&text.countdown);
        self.caption.set_text(&text.caption);
        self.caption.set_visible(!text.caption.is_empty());
        self.extend.set_label(&text.extend);
    }
}

impl Drop for SleepTimerControl {
    fn drop(&mut self) {
        if let Some(source) = self.tick.take() {
            source.remove();
        }
    }
}
