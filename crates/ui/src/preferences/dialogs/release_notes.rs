use std::rc::Rc;
use std::time::Duration;

use crate::layout::{large_popup_content_height, large_popup_content_width};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::runtime::{ReleaseHistory, ReleaseNote, ReleaseUpdate, ReleaseUpdateHandle};
use crate::shell::Shell;
use adw::prelude::*;
use gtk::glib;
use localization::{tr, trn_with};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use tracing::warn;

const RELEASE_NOTES_POPUP_WIDTH: i32 = 700;
const RELEASE_NOTES_POPUP_HEIGHT: i32 = 640;
const RELEASE_TOAST_TITLE: &str = "✨ New release is available!";
const RELEASE_CHECK_POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleaseBlockKind {
    Heading,
    Paragraph,
    Bullet,
    Quote,
    Code,
    Divider,
}

#[derive(Debug, Eq, PartialEq)]
struct ReleaseBlock {
    kind: ReleaseBlockKind,
    markup: String,
}

fn finish_release_block(blocks: &mut Vec<ReleaseBlock>, current: &mut Option<ReleaseBlock>) {
    if let Some(block) = current
        .take()
        .filter(|block| !block.markup.trim().is_empty())
    {
        blocks.push(block);
    }
}

fn begin_release_block(
    blocks: &mut Vec<ReleaseBlock>,
    current: &mut Option<ReleaseBlock>,
    kind: ReleaseBlockKind,
) {
    finish_release_block(blocks, current);
    *current = Some(ReleaseBlock {
        kind,
        markup: String::new(),
    });
}

fn append_escaped(output: &mut String, text: &str) {
    output.push_str(&glib::markup_escape_text(text));
}

fn append_github_text(output: &mut String, text: &str) {
    let chars = text.chars().collect::<Vec<_>>();
    let mut plain = String::new();
    let mut index = 0;
    while index < chars.len() {
        let boundary = index == 0 || !chars[index - 1].is_ascii_alphanumeric();
        if chars[index] == '#' && boundary && chars.get(index + 1).is_some_and(char::is_ascii_digit)
        {
            append_escaped(output, &plain);
            plain.clear();
            let start = index;
            index += 1;
            while chars.get(index).is_some_and(char::is_ascii_digit) {
                index += 1;
            }
            let number = chars[start + 1..index].iter().collect::<String>();
            output.push_str(&format!(
                "<a href=\"https://github.com/screwys/Rufin/issues/{number}\">#{number}</a>"
            ));
            continue;
        }
        if chars[index] == '@'
            && boundary
            && chars
                .get(index + 1)
                .is_some_and(|ch| ch.is_ascii_alphanumeric())
        {
            append_escaped(output, &plain);
            plain.clear();
            let start = index + 1;
            index += 2;
            while chars
                .get(index)
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            {
                index += 1;
            }
            let login = chars[start..index].iter().collect::<String>();
            output.push_str(&format!(
                "<a href=\"https://github.com/{login}\">@{login}</a>"
            ));
            continue;
        }
        plain.push(chars[index]);
        index += 1;
    }
    append_escaped(output, &plain);
}

fn release_markdown_blocks(body: &str) -> Vec<ReleaseBlock> {
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES;
    let mut blocks = Vec::new();
    let mut current = None;
    let mut explicit_link_depth = 0_u32;

    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                begin_release_block(&mut blocks, &mut current, ReleaseBlockKind::Heading)
            }
            Event::Start(Tag::Paragraph) if current.is_none() => {
                begin_release_block(&mut blocks, &mut current, ReleaseBlockKind::Paragraph)
            }
            Event::Start(Tag::Item) => {
                begin_release_block(&mut blocks, &mut current, ReleaseBlockKind::Bullet)
            }
            Event::Start(Tag::BlockQuote(_)) => {
                begin_release_block(&mut blocks, &mut current, ReleaseBlockKind::Quote)
            }
            Event::Start(Tag::CodeBlock(_)) => {
                begin_release_block(&mut blocks, &mut current, ReleaseBlockKind::Code)
            }
            Event::Start(Tag::Strong) => {
                current
                    .get_or_insert_with(|| ReleaseBlock {
                        kind: ReleaseBlockKind::Paragraph,
                        markup: String::new(),
                    })
                    .markup
                    .push_str("<b>");
            }
            Event::End(TagEnd::Strong) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push_str("</b>");
                }
            }
            Event::Start(Tag::Emphasis) => {
                current
                    .get_or_insert_with(|| ReleaseBlock {
                        kind: ReleaseBlockKind::Paragraph,
                        markup: String::new(),
                    })
                    .markup
                    .push_str("<i>");
            }
            Event::End(TagEnd::Emphasis) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push_str("</i>");
                }
            }
            Event::Start(Tag::Strikethrough) => {
                current
                    .get_or_insert_with(|| ReleaseBlock {
                        kind: ReleaseBlockKind::Paragraph,
                        markup: String::new(),
                    })
                    .markup
                    .push_str("<s>");
            }
            Event::End(TagEnd::Strikethrough) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push_str("</s>");
                }
            }
            Event::Start(Tag::Link { dest_url, .. })
            | Event::Start(Tag::Image { dest_url, .. }) => {
                let block = current.get_or_insert_with(|| ReleaseBlock {
                    kind: ReleaseBlockKind::Paragraph,
                    markup: String::new(),
                });
                block.markup.push_str("<a href=\"");
                append_escaped(&mut block.markup, &dest_url);
                block.markup.push_str("\">");
                explicit_link_depth += 1;
            }
            Event::End(TagEnd::Link) | Event::End(TagEnd::Image) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push_str("</a>");
                }
                explicit_link_depth = explicit_link_depth.saturating_sub(1);
            }
            Event::Text(text) => {
                let block = current.get_or_insert_with(|| ReleaseBlock {
                    kind: ReleaseBlockKind::Paragraph,
                    markup: String::new(),
                });
                if explicit_link_depth == 0 && block.kind != ReleaseBlockKind::Code {
                    append_github_text(&mut block.markup, &text);
                } else {
                    append_escaped(&mut block.markup, &text);
                }
            }
            Event::Code(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                let block = current.get_or_insert_with(|| ReleaseBlock {
                    kind: ReleaseBlockKind::Paragraph,
                    markup: String::new(),
                });
                block.markup.push_str("<tt>");
                append_escaped(&mut block.markup, &text);
                block.markup.push_str("</tt>");
            }
            Event::SoftBreak => {
                if let Some(block) = current.as_mut() {
                    block.markup.push(' ');
                }
            }
            Event::HardBreak => {
                if let Some(block) = current.as_mut() {
                    block.markup.push('\n');
                }
            }
            Event::TaskListMarker(checked) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push_str(if checked { "☑ " } else { "☐ " });
                }
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let block = current.get_or_insert_with(|| ReleaseBlock {
                    kind: ReleaseBlockKind::Paragraph,
                    markup: String::new(),
                });
                append_escaped(&mut block.markup, &html);
            }
            Event::FootnoteReference(reference) => {
                let block = current.get_or_insert_with(|| ReleaseBlock {
                    kind: ReleaseBlockKind::Paragraph,
                    markup: String::new(),
                });
                append_escaped(&mut block.markup, &format!("[{reference}]"));
            }
            Event::Rule => {
                finish_release_block(&mut blocks, &mut current);
                blocks.push(ReleaseBlock {
                    kind: ReleaseBlockKind::Divider,
                    markup: String::new(),
                });
            }
            Event::End(TagEnd::Heading(_))
            | Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Item)
            | Event::End(TagEnd::BlockQuote(_))
            | Event::End(TagEnd::CodeBlock)
            | Event::End(TagEnd::Table) => {
                finish_release_block(&mut blocks, &mut current);
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push_str("    ");
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(block) = current.as_mut() {
                    block.markup.push('\n');
                }
            }
            Event::Start(_) | Event::End(_) => {}
        }
    }
    finish_release_block(&mut blocks, &mut current);
    blocks
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
    let url = note.url.clone();
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

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.add_css_class("release-note-body");
    for block in release_markdown_blocks(&note.body) {
        if block.kind == ReleaseBlockKind::Divider {
            body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            continue;
        }
        let text = gtk::Label::new(None);
        text.set_markup(&block.markup);
        text.set_wrap(true);
        text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        text.set_xalign(0.0);
        text.set_hexpand(true);
        match block.kind {
            ReleaseBlockKind::Heading => text.add_css_class("release-note-heading"),
            ReleaseBlockKind::Quote => text.add_css_class("release-note-quote"),
            ReleaseBlockKind::Code => text.add_css_class("release-note-code"),
            ReleaseBlockKind::Paragraph | ReleaseBlockKind::Bullet => {
                text.add_css_class("release-note-item");
            }
            ReleaseBlockKind::Divider => unreachable!("dividers do not create labels"),
        }
        if block.kind == ReleaseBlockKind::Bullet {
            let bullet = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            bullet.add_css_class("release-note-bullet");
            let marker = gtk::Label::new(Some("•"));
            marker.add_css_class("release-note-marker");
            marker.set_valign(gtk::Align::Start);
            bullet.append(&marker);
            bullet.append(&text);
            body.append(&bullet);
        } else {
            body.append(&text);
        }
    }
    row.append(&body);

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

    let view = gtk::Box::new(gtk::Orientation::Vertical, 0);
    populate_release_notes_view(window, &view, history, updating_version, release_updates);

    let popup_width = large_popup_content_width(RELEASE_NOTES_POPUP_WIDTH);
    let popup_height = large_popup_content_height(window.height(), RELEASE_NOTES_POPUP_HEIGHT);
    toolbar.set_content(Some(&view));

    let dialog = adw::Dialog::builder()
        .title(tr("Version History"))
        .content_width(popup_width)
        .content_height(popup_height)
        .child(&toolbar)
        .build();
    let view = view.downgrade();
    present_light_dismiss_dialog(&dialog, window);
    view
}

fn selected_release_version(view: &gtk::Box) -> Option<String> {
    view.first_child()
        .and_then(|child| child.downcast::<gtk::Box>().ok())
        .and_then(|selector| selector.last_child())
        .and_then(|child| child.downcast::<gtk::DropDown>().ok())
        .and_then(|dropdown| dropdown.selected_item())
        .and_then(|item| item.downcast::<gtk::StringObject>().ok())
        .and_then(|item| {
            item.string()
                .strip_prefix('v')
                .and_then(|label| label.split_once("  ·  "))
                .map(|(version, _)| version.to_string())
        })
}

fn populate_release_notes_view(
    window: &gtk::ApplicationWindow,
    view: &gtk::Box,
    history: &ReleaseHistory,
    updating_version: Option<&str>,
    release_updates: &ReleaseUpdateHandle,
) {
    let selected_version = selected_release_version(view);
    let selected = selected_version
        .and_then(|version| {
            history
                .notes
                .iter()
                .position(|note| note.version == version)
        })
        .unwrap_or_default() as u32;
    while let Some(child) = view.first_child() {
        view.remove(&child);
    }

    let selector_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    selector_row.add_css_class("release-note-selector");
    selector_row.set_margin_top(12);
    selector_row.set_margin_start(18);
    selector_row.set_margin_end(18);
    let titles = history
        .notes
        .iter()
        .map(|note| {
            format!(
                "v{}  ·  {}",
                note.version,
                release_relative_date(&note.date)
            )
        })
        .collect::<Vec<_>>();
    let title_refs = titles.iter().map(String::as_str).collect::<Vec<_>>();
    let selector = gtk::DropDown::from_strings(&title_refs);
    selector.set_hexpand(true);
    selector.set_sensitive(!history.notes.is_empty());
    selector.set_selected(selected);
    selector_row.append(&selector);
    view.append(&selector_row);

    let release = gtk::Box::new(gtk::Orientation::Vertical, 0);
    release.add_css_class("release-notes-list");
    if let Some(note) = history.notes.get(selected as usize) {
        release.append(&release_note_row(
            window,
            note,
            history,
            updating_version,
            release_updates,
        ));
    }

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_margin_top(12);
    scroller.set_margin_bottom(12);
    scroller.set_margin_start(18);
    scroller.set_margin_end(18);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&release));
    view.append(&scroller);

    let window = window.clone();
    let history = history.clone();
    let updating_version = updating_version.map(str::to_string);
    let release_updates = release_updates.clone();
    selector.connect_selected_notify(move |selector| {
        while let Some(child) = release.first_child() {
            release.remove(&child);
        }
        if let Some(note) = history.notes.get(selector.selected() as usize) {
            release.append(&release_note_row(
                &window,
                note,
                &history,
                updating_version.as_deref(),
                &release_updates,
            ));
        }
    });
}

fn refresh_open_release_notes(shell: &Shell) {
    let Some(view) = shell
        .preferences
        .release_history_view
        .borrow()
        .as_ref()
        .and_then(gtk::glib::WeakRef::upgrade)
    else {
        return;
    };
    let history = shell.preferences.release_history.borrow().clone();
    let updating_version = shell.preferences.release_updating.borrow().clone();
    populate_release_notes_view(
        &shell.chrome.window,
        &view,
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
        let view = present_release_notes_dialog(
            &self.chrome.window,
            &history,
            updating_version.as_deref(),
            &self.products.release_updates,
        );
        self.preferences.release_history_view.replace(Some(view));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::runtime::{ReleaseHistory, ReleaseNote};

    use super::{
        CivilDate, ReleaseBlockKind, ReleaseRowStatus, release_markdown_blocks,
        release_relative_date_for, release_row_status,
    };

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
                url: "https://github.com/screwys/Rufin/releases/tag/v2.0.0".to_string(),
                body: String::new(),
            },
            ReleaseNote {
                version: "1.0.0".to_string(),
                date: String::new(),
                url: "https://github.com/screwys/Rufin/releases/tag/v1.0.0".to_string(),
                body: String::new(),
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

    #[test]
    fn github_changelog_references_and_contributors_become_links() {
        let blocks = release_markdown_blocks(
            "## Changelog\n\n- Fix playback by @someone in #744\n\n## New Contributors\n\n- @newcomer made their first contribution in #745\n\n**Full Changelog:** [v1...v2](https://github.com/screwys/Rufin/compare/v1...v2)",
        );

        assert_eq!(blocks[0].kind, ReleaseBlockKind::Heading);
        assert!(blocks[0].markup.contains("Changelog"));
        assert_eq!(blocks[1].kind, ReleaseBlockKind::Bullet);
        assert!(blocks[1].markup.contains("https://github.com/someone"));
        assert!(
            blocks[1]
                .markup
                .contains("https://github.com/screwys/Rufin/issues/744")
        );
        assert_eq!(blocks[2].kind, ReleaseBlockKind::Heading);
        assert_eq!(blocks[3].kind, ReleaseBlockKind::Bullet);
        assert!(blocks[3].markup.contains("https://github.com/newcomer"));
        assert!(
            blocks[4]
                .markup
                .contains("https://github.com/screwys/Rufin/compare/v1...v2")
        );
    }
}
