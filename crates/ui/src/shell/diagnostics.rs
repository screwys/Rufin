use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gio::prelude::FileExtManual;
use gtk::{gio, glib};
use localization::tr;
use tracing::warn;

use crate::layout::{large_popup_content_height, large_popup_content_width};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;

use super::Shell;

const DIAGNOSTICS_DIALOG_WIDTH: i32 = 680;
const DIAGNOSTICS_DIALOG_HEIGHT: i32 = 560;
const LOG_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

pub(super) fn present_diagnostics(shell: &Rc<Shell>) {
    let resource = crate::ui_resource::DIAGNOSTICS_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    let dialog: adw::Dialog = crate::ui_resource::object(&builder, resource, "diagnostics_dialog");
    dialog.set_content_width(large_popup_content_width(DIAGNOSTICS_DIALOG_WIDTH));
    dialog.set_content_height(large_popup_content_height(
        shell.chrome.window.height(),
        DIAGNOSTICS_DIALOG_HEIGHT,
    ));
    let debug: adw::SwitchRow = crate::ui_resource::object(&builder, resource, "debug");
    debug.set_active(shell.diagnostics.debug_enabled());

    let log: gtk::TextView = crate::ui_resource::object(&builder, resource, "log");
    log.update_property(&[gtk::accessible::Property::Label(&tr("Diagnostic log"))]);
    let scroller: gtk::ScrolledWindow =
        crate::ui_resource::object(&builder, resource, "log_scroller");
    let copy: gtk::Button = crate::ui_resource::object(&builder, resource, "copy");
    let save: gtk::Button = crate::ui_resource::object(&builder, resource, "save");
    let status: gtk::Label = crate::ui_resource::object(&builder, resource, "status");
    drop(builder);

    let changing_debug = Rc::new(Cell::new(false));
    let diagnostics = shell.diagnostics.clone();
    let debug_status = status.clone();
    let changing_debug_for_notify = Rc::clone(&changing_debug);
    debug.connect_active_notify(move |row| {
        if changing_debug_for_notify.get() {
            return;
        }
        let enabled = row.is_active();
        if let Err(error) = diagnostics.set_debug_enabled(enabled) {
            changing_debug_for_notify.set(true);
            row.set_active(!enabled);
            changing_debug_for_notify.set(false);
            warn!(%error, "could not change debug logging");
            debug_status.add_css_class("error");
            debug_status.set_text(&error);
            debug_status.set_visible(true);
        }
    });

    let initial = shell.diagnostics.snapshot();
    log.buffer().set_text(&initial);
    scroll_to_log_end(&scroller);
    let last_revision = Rc::new(Cell::new(shell.diagnostics.revision()));
    let weak_dialog = dialog.downgrade();
    let diagnostics = shell.diagnostics.clone();
    let refresh_log = log.clone();
    let refresh_scroller = scroller.clone();
    let refresh_revision = Rc::clone(&last_revision);
    glib::timeout_add_local(LOG_REFRESH_INTERVAL, move || {
        if weak_dialog.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        let revision = diagnostics.revision();
        if refresh_revision.replace(revision) != revision {
            let adjustment = refresh_scroller.vadjustment();
            let follows_tail =
                adjustment.value() + adjustment.page_size() >= adjustment.upper() - 24.0;
            refresh_log.buffer().set_text(&diagnostics.snapshot());
            if follows_tail {
                scroll_to_log_end(&refresh_scroller);
            }
        }
        glib::ControlFlow::Continue
    });

    let copy_buffer = log.buffer();
    let copy_status = status.clone();
    copy.connect_clicked(move |button| {
        let buffer = copy_buffer.clone();
        let contents = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        button.display().clipboard().set_text(&contents);
        copy_status.remove_css_class("error");
        copy_status.set_text(&tr("Logs are copied"));
    });

    let save_window = shell.chrome.window.clone();
    let save_buffer = log.buffer();
    let save_status = status.clone();
    save.connect_clicked(move |_| {
        let window = save_window.clone();
        let buffer = save_buffer.clone();
        let status = save_status.clone();
        glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title(tr("Save Diagnostic Log"))
                .initial_name("rufin-debug.log")
                .build();
            let Ok(file) = chooser.save_future(Some(&window)).await else {
                return;
            };
            let contents = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .as_bytes()
                .to_vec();
            match file
                .replace_contents_future(
                    contents,
                    None,
                    false,
                    gio::FileCreateFlags::REPLACE_DESTINATION,
                )
                .await
            {
                Ok(_) => {
                    status.remove_css_class("error");
                    status.set_text(&tr("Diagnostic log saved"));
                    status.set_visible(true);
                }
                Err((_, error)) => {
                    warn!(%error, "could not save diagnostic log");
                    status.add_css_class("error");
                    status.set_text(&format!("{}: {error}", tr("Could not save diagnostic log")));
                    status.set_visible(true);
                }
            }
        });
    });

    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

fn scroll_to_log_end(scroller: &gtk::ScrolledWindow) {
    let adjustment = scroller.vadjustment();
    adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
}
