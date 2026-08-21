use std::rc::Rc;
use std::time::Duration;

use crate::layout::{large_popup_content_height, large_popup_content_width};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::runtime::{ReleaseHistory, ReleaseNote, ReleaseUpdate, ReleaseUpdateHandle};
use crate::shell::Shell;
use adw::prelude::*;
use gtk::glib;
use localization::{tr, trn_with};
use tracing::warn;

const RELEASE_NOTES_POPUP_WIDTH: i32 = 700;
const RELEASE_NOTES_POPUP_HEIGHT: i32 = 640;
const RELEASE_TOAST_TITLE: &str = "✨ New release is available!";
const RELEASE_CHECK_POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CivilDate {
    year: i32,
    month: u32,
    day: u32,
}

pub(crate) fn check_for_release_update(shell: &Rc<Shell>) {
    shell.products.release_updates.check_and_update();
}

pub(crate) fn schedule_periodic_release_checks(shell: &Rc<Shell>) {
    let release_updates = shell.products.release_updates.clone();
    glib::timeout_add_local(RELEASE_CHECK_POLL_INTERVAL, move || {
        release_updates.check();
        glib::ControlFlow::Continue
    });
}

fn current_civil_date() -> Option<CivilDate> {
    let now = glib::DateTime::now_local().ok()?;
    Some(CivilDate {
        year: now.year(),
        month: now.month() as u32,
        day: now.day_of_month() as u32,
    })
}

fn parse_civil_date(text: &str) -> Option<CivilDate> {
    let mut parts = text.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(CivilDate { year, month, day })
}

fn civil_days(date: CivilDate) -> i32 {
    let year = date.year - i32::from(date.month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = date.month as i32;
    let day = date.day as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn release_relative_date_for(date: &str, today: CivilDate) -> String {
    let Some(release_date) = parse_civil_date(date) else {
        return date.to_string();
    };
    let days = civil_days(today).saturating_sub(civil_days(release_date));
    if days < 0 {
        return date.to_string();
    }
    match days {
        0 => tr("today"),
        1 => tr("yesterday"),
        2..=13 => {
            let count = days as u64;
            trn_with(
                "{count} day ago",
                "{count} days ago",
                count,
                &[("count", &count.to_string())],
            )
        }
        14..=59 => {
            let count = (days / 7) as u64;
            trn_with(
                "{count} week ago",
                "{count} weeks ago",
                count,
                &[("count", &count.to_string())],
            )
        }
        60..=729 => {
            let count = (days / 30) as u64;
            trn_with(
                "{count} month ago",
                "{count} months ago",
                count,
                &[("count", &count.to_string())],
            )
        }
        _ => {
            let count = (days / 365) as u64;
            trn_with(
                "{count} year ago",
                "{count} years ago",
                count,
                &[("count", &count.to_string())],
            )
        }
    }
}

fn release_relative_date(date: &str) -> String {
    current_civil_date()
        .map(|today| release_relative_date_for(date, today))
        .unwrap_or_else(|| date.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleaseRowStatus {
    Installed,
    Update,
    None,
}

fn release_row_status(history: &ReleaseHistory, note: &ReleaseNote) -> ReleaseRowStatus {
    if note.version == history.installed_version {
        return ReleaseRowStatus::Installed;
    }
    if history.available_version.as_deref() == Some(note.version.as_str()) {
        return ReleaseRowStatus::Update;
    }
    ReleaseRowStatus::None
}

fn release_note_row(
    window: &gtk::ApplicationWindow,
    note: &ReleaseNote,
    history: &ReleaseHistory,
    updating_version: Option<&str>,
    release_updates: &ReleaseUpdateHandle,
) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 8);
    row.add_css_class("release-note-row");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    header.set_hexpand(true);
    let version_label = format!("v{}", note.version);
    let version = gtk::Button::new();
    version.add_css_class("flat");
    version.add_css_class("release-note-version");
    version.set_cursor_from_name(Some("pointer"));
    version.set_tooltip_text(Some(&tr("Open release notes")));
    let version_content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let version_text = gtk::Label::new(Some(&version_label));
    let version_icon = gtk::Image::from_icon_name("rufin-external-link-symbolic");
    version_icon.set_pixel_size(12);
    version_content.append(&version_text);
    version_content.append(&version_icon);
    version.set_child(Some(&version_content));
    let url = format!(
        "https://github.com/screwys/Rufin/releases/tag/v{}",
        note.version
    );
    let window = window.downgrade();
    version.connect_clicked(move |_| {
        let Some(window) = window.upgrade() else {
            return;
        };
        let launcher = gtk::UriLauncher::new(&url);
        gtk::glib::spawn_future_local(async move {
            if let Err(error) = launcher.launch_future(Some(&window)).await {
                warn!(%error, "failed to open release notes link");
            }
        });
    });
    let date = gtk::Label::new(Some(&release_relative_date(&note.date)));
    date.add_css_class("release-note-date");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&version);
    match release_row_status(history, note) {
        ReleaseRowStatus::Installed => {
            let installed = gtk::Label::new(Some(&tr("Installed")));
            installed.add_css_class("release-note-installed");
            header.append(&installed);
        }
        ReleaseRowStatus::Update => {
            let update = gtk::Button::with_label(&tr("Update"));
            update.add_css_class("release-note-installed");
            update.add_css_class("release-note-update");
            update.set_cursor_from_name(Some("pointer"));
            update.set_sensitive(updating_version != Some(note.version.as_str()));
            let version = note.version.clone();
            let release_updates = release_updates.clone();
            update.connect_clicked(move |_| {
                release_updates.update(version.clone());
            });
            header.append(&update);
        }
        ReleaseRowStatus::None => {}
    }
    header.append(&spacer);
    header.append(&date);
    row.append(&header);

    if let Some(summary) = note.summary.as_ref() {
        let body = gtk::Label::new(Some(summary));
        body.add_css_class("release-note-summary");
        body.set_wrap(true);
        body.set_xalign(0.0);
        row.append(&body);
    }

    let items = gtk::Box::new(gtk::Orientation::Vertical, 4);
    items.add_css_class("release-note-items");
    for item in &note.items {
        let bullet = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bullet.add_css_class("release-note-bullet");
        let marker = gtk::Label::new(Some("•"));
        marker.add_css_class("release-note-marker");
        marker.set_valign(gtk::Align::Start);
        let text = gtk::Label::new(Some(item));
        text.add_css_class("release-note-item");
        text.set_wrap(true);
        text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.set_xalign(0.0);
        text.set_hexpand(true);
        bullet.append(&marker);
        bullet.append(&text);
        items.append(&bullet);
    }
    row.append(&items);

    row.upcast()
}

fn present_release_notes_dialog(
    window: &gtk::ApplicationWindow,
    history: &ReleaseHistory,
    updating_version: Option<&str>,
    release_updates: &ReleaseUpdateHandle,
) -> gtk::glib::WeakRef<gtk::Box> {
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(&tr("Version History"), "")));
    toolbar.add_top_bar(&header);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("release-notes-list");
    populate_release_notes_list(window, &list, history, updating_version, release_updates);

    let popup_width = large_popup_content_width(RELEASE_NOTES_POPUP_WIDTH);
    let popup_height = large_popup_content_height(window.height(), RELEASE_NOTES_POPUP_HEIGHT);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_margin_top(12);
    scroller.set_margin_bottom(12);
    scroller.set_margin_start(18);
    scroller.set_margin_end(18);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&list));
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(tr("Version History"))
        .content_width(popup_width)
        .content_height(popup_height)
        .child(&toolbar)
        .build();
    let list = list.downgrade();
    present_light_dismiss_dialog(&dialog, window);
    list
}

fn populate_release_notes_list(
    window: &gtk::ApplicationWindow,
    list: &gtk::Box,
    history: &ReleaseHistory,
    updating_version: Option<&str>,
    release_updates: &ReleaseUpdateHandle,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    for note in history.notes.iter() {
        list.append(&release_note_row(
            window,
            note,
            history,
            updating_version,
            release_updates,
        ));
    }
}

fn refresh_open_release_notes(shell: &Shell) {
    let Some(list) = shell
        .preferences
        .release_history_list
        .borrow()
        .as_ref()
        .and_then(gtk::glib::WeakRef::upgrade)
    else {
        return;
    };
    let history = shell.preferences.release_history.borrow().clone();
    let updating_version = shell.preferences.release_updating.borrow().clone();
    populate_release_notes_list(
        &shell.chrome.window,
        &list,
        &history,
        updating_version.as_deref(),
        &shell.products.release_updates,
    );
}

pub(crate) fn apply_release_update(shell: &Rc<Shell>, update: ReleaseUpdate) {
    match update {
        ReleaseUpdate::Refreshed {
            history,
            notification_version,
        } => {
            let update_available = history.available_version.is_some();
            let changed = {
                let mut current = shell.preferences.release_history.borrow_mut();
                if *current == history {
                    false
                } else {
                    *current = history;
                    true
                }
            };
            if changed {
                refresh_open_release_notes(shell);
            }
            if !update_available {
                dismiss_release_notification(shell);
            }
            let Some(version) = notification_version else {
                return;
            };
            dismiss_release_notification(shell);
            let toast = adw::Toast::new(&tr(RELEASE_TOAST_TITLE));
            toast.set_timeout(0);
            toast.set_button_label(Some(&tr("View")));
            toast.set_action_name(Some("win.show-release-notes"));
            shell.chrome.toast_overlay.add_toast(toast.clone());
            *shell.preferences.release_notification_toast.borrow_mut() = Some(toast);
            if let Err(error) = shell.products.release_updates.mark_seen(version) {
                warn!(%error, "failed to record the shown release notification");
            }
        }
        ReleaseUpdate::Updating { version } => {
            dismiss_release_notification(shell);
            *shell.preferences.release_updating.borrow_mut() = Some(version);
            refresh_open_release_notes(shell);
        }
        ReleaseUpdate::Updated { version, .. } => {
            dismiss_release_notification(shell);
            clear_updating_version(shell, &version);
            {
                let mut history = shell.preferences.release_history.borrow_mut();
                history.installed_version = version.clone();
                history.available_version = None;
            }
            refresh_open_release_notes(shell);
        }
        ReleaseUpdate::Failed { version, error } => {
            clear_updating_version(shell, &version);
            refresh_open_release_notes(shell);
            warn!(%version, %error, "Rufin update failed");
        }
        ReleaseUpdate::Restarting { version } => {
            dismiss_release_notification(shell);
            *shell.preferences.release_updating.borrow_mut() = Some(version);
            shell.request_quit("update restart");
        }
    }
}

pub(crate) fn dismiss_release_notification(shell: &Shell) {
    if let Some(toast) = shell
        .preferences
        .release_notification_toast
        .borrow_mut()
        .take()
    {
        toast.dismiss();
    }
}

fn clear_updating_version(shell: &Shell, version: &str) {
    let mut updating = shell.preferences.release_updating.borrow_mut();
    if updating.as_deref() == Some(version) {
        updating.take();
    }
}

impl Shell {
    pub(crate) fn present_release_notes(&self) {
        let history = self.preferences.release_history.borrow().clone();
        let updating_version = self.preferences.release_updating.borrow().clone();
        let list = present_release_notes_dialog(
            &self.chrome.window,
            &history,
            updating_version.as_deref(),
            &self.products.release_updates,
        );
        self.preferences.release_history_list.replace(Some(list));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::runtime::{ReleaseHistory, ReleaseNote};

    use super::{CivilDate, ReleaseRowStatus, release_relative_date_for, release_row_status};

    #[test]
    fn release_dates_use_relative_labels_without_singular_units() {
        let today = CivilDate {
            year: 2026,
            month: 6,
            day: 19,
        };

        assert_eq!(release_relative_date_for("2026-06-19", today), "today");
        assert_eq!(release_relative_date_for("2026-06-18", today), "yesterday");
        assert_eq!(release_relative_date_for("2026-06-10", today), "9 days ago");
        assert_eq!(
            release_relative_date_for("2026-05-01", today),
            "7 weeks ago"
        );
        assert_eq!(
            release_relative_date_for("2025-09-19", today),
            "9 months ago"
        );
        assert_eq!(
            release_relative_date_for("2025-06-19", today),
            "12 months ago"
        );
        assert_eq!(
            release_relative_date_for("2023-06-19", today),
            "3 years ago"
        );
        assert_eq!(release_relative_date_for("not-a-date", today), "not-a-date");
    }

    #[test]
    fn only_the_latest_newer_release_gets_an_update_action() {
        let notes: Arc<[ReleaseNote]> = vec![
            ReleaseNote {
                version: "2.0.0".to_string(),
                date: String::new(),
                summary: None,
                items: Vec::new(),
            },
            ReleaseNote {
                version: "1.0.0".to_string(),
                date: String::new(),
                summary: None,
                items: Vec::new(),
            },
        ]
        .into();
        let history = ReleaseHistory {
            notes: Arc::clone(&notes),
            installed_version: "1.0.0".to_string(),
            available_version: Some("2.0.0".to_string()),
            automatic_updates_supported: true,
        };

        assert_eq!(
            release_row_status(&history, &notes[0]),
            ReleaseRowStatus::Update
        );
        assert_eq!(
            release_row_status(&history, &notes[1]),
            ReleaseRowStatus::Installed
        );
    }
}
