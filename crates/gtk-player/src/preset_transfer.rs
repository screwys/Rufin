use std::future::Future;

use adw::prelude::*;
use gtk::{gio, glib};

pub(crate) fn import<F: Future<Output = Result<(), String>> + 'static>(
    root: &impl IsA<gtk::Widget>,
    allow_apo: bool,
    apply: impl Fn(String, bool) -> F + 'static,
) {
    let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        import_dialog: adw::Dialog, import_text: gtk::TextView,
        import_formats: gtk::Box, import_apo: gtk::ToggleButton,
        import_accept: gtk::Button, import_error: gtk::Label,
        choose_file: gtk::Button, paste: gtk::Button, file_open: gtk::FileDialog,
    });
    import_formats.set_visible(allow_apo);
    import_dialog.set_focus(Some(&import_text));
    let buffer = import_text.buffer();
    import_accept.set_sensitive(false);
    let accept = import_accept.downgrade();
    let error = import_error.downgrade();
    buffer.connect_changed(move |buffer| {
        if let Some(accept) = accept.upgrade() {
            accept.set_sensitive(
                !buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .trim()
                    .is_empty(),
            );
        }
        if let Some(error) = error.upgrade() {
            error.set_visible(false);
        }
    });
    let text = import_text.downgrade();
    let format = import_apo.downgrade();
    let set_text = std::rc::Rc::new(move |contents: &str| {
        if allow_apo && let Some(format) = format.upgrade() {
            format.set_active(!contents.trim_start().starts_with('{'));
        }
        if let Some(view) = text.upgrade() {
            view.buffer().set_text(contents);
        }
    });
    let pasted = set_text.clone();
    paste.connect_clicked(move |button| {
        let clipboard = button.clipboard();
        let set_text = pasted.clone();
        glib::spawn_future_local(async move {
            if let Ok(Some(text)) = clipboard.read_text_future().await {
                set_text(&text);
            }
        });
    });
    let error = import_error.downgrade();
    choose_file.connect_clicked(move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        let chooser = file_open.clone();
        let set_text = set_text.clone();
        let error = error.clone();
        glib::spawn_future_local(async move {
            let Ok(file) = chooser.open_future(window.as_ref()).await else {
                return;
            };
            let result = file
                .load_contents_future()
                .await
                .map_err(|error| error.to_string())
                .and_then(|(bytes, _)| {
                    String::from_utf8(bytes.to_vec()).map_err(|error| error.to_string())
                });
            match result {
                Ok(text) => set_text(&text),
                Err(message) => show_error(&error, &message),
            }
        });
    });
    let dialog = import_dialog.downgrade();
    import_accept.connect_clicked(move |button| {
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        let result = apply(text, import_apo.is_active());
        button.set_sensitive(false);
        import_error.set_visible(false);
        let button = button.downgrade();
        let dialog = dialog.clone();
        let error = import_error.downgrade();
        glib::spawn_future_local(async move {
            match result.await {
                Ok(()) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                }
                Err(message) => {
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(true);
                    }
                    show_error(&error, &message);
                }
            }
        });
    });
    gtk_widgets::popup::present_light_dismiss_dialog(&import_dialog, root);
}

pub(crate) fn export(
    root: &impl IsA<gtk::Widget>,
    filenames: &'static [&'static str],
    preview: impl Fn(bool) -> Result<String, String> + 'static,
) {
    let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        export_dialog: adw::Dialog, export_formats: gtk::Box, export_apo: gtk::ToggleButton,
        export_text: gtk::TextView, export_error: gtk::Label, file_save: gtk::FileDialog,
        export_accept: gtk::Button, export_save: gtk::Button, export_copy_icon: gtk::Image,
    });
    export_formats.set_visible(filenames.len() > 1);
    let buffer = export_text.buffer();
    let preview_buffer = buffer.clone();
    let copy = export_accept.downgrade();
    let save = export_save.downgrade();
    let error = export_error.downgrade();
    let update = move |format: &gtk::ToggleButton| {
        let result = preview(format.is_active());
        if let Some(copy) = copy.upgrade() {
            copy.set_sensitive(result.is_ok());
        }
        if let Some(save) = save.upgrade() {
            save.set_sensitive(result.is_ok());
        }
        match result {
            Ok(text) => {
                preview_buffer.set_text(&text);
                if let Some(error) = error.upgrade() {
                    error.set_visible(false);
                }
            }
            Err(message) => {
                preview_buffer.set_text("");
                show_error(&error, &message);
            }
        }
    };
    update(&export_apo);
    export_apo.connect_toggled(update);
    let copy_buffer = buffer.clone();
    let copy = gtk_widgets::clipboard::copy_action(export_accept.upcast_ref(), &export_copy_icon);
    export_accept.connect_clicked(move |_| {
        copy(&copy_buffer.text(&copy_buffer.start_iter(), &copy_buffer.end_iter(), false));
    });
    let dialog = export_dialog.downgrade();
    let error = export_error.downgrade();
    export_save.connect_clicked(move |button| {
        let text = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        file_save.set_initial_name(Some(filenames[usize::from(export_apo.is_active())]));
        let window = button.root().and_downcast::<gtk::Window>();
        let chooser = file_save.clone();
        let dialog = dialog.clone();
        let error = error.clone();
        glib::spawn_future_local(async move {
            let Ok(file) = chooser.save_future(window.as_ref()).await else {
                return;
            };
            match file
                .replace_contents_future(
                    text.into_bytes(),
                    None,
                    false,
                    gio::FileCreateFlags::REPLACE_DESTINATION,
                )
                .await
            {
                Ok(_) => {
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                }
                Err((_, message)) => show_error(&error, &message.to_string()),
            }
        });
    });
    gtk_widgets::popup::present_light_dismiss_dialog(&export_dialog, root);
}

fn show_error(error: &glib::WeakRef<gtk::Label>, message: &str) {
    if let Some(label) = error.upgrade() {
        label.set_text(message);
        label.set_visible(true);
    }
}
