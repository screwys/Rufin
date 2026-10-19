use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use gtk_player::outputs::casting_network_dropdown;
use localization::tr;
use secrets::SecretStorageMode;

use crate::Preferences;

pub(super) fn page(shell: &Rc<Preferences>, dialog: &adw::Dialog) -> adw::PreferencesPage {
    let resource = crate::ui_resource::PRIVACY_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        privacy_page: adw::PreferencesPage,
        private_row: adw::SwitchRow,
        secret_storage_row: adw::ComboRow,
        cast_proxy_row: adw::SwitchRow,
        cast_network_row: adw::ActionRow,
    });
    let settings = shell.settings.current.borrow().clone();
    private_row.set_active(settings.application.private_mode);
    let private_shell = Rc::clone(shell);
    private_row.connect_active_notify(move |row| {
        private_shell.set_private_mode(row.is_active());
    });
    let secret_storage_titles = [tr("Legacy"), tr("System keyring")];
    let secret_storage_refs = secret_storage_titles
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    secret_storage_row.set_model(Some(&gtk::StringList::new(&secret_storage_refs)));
    shell
        .state
        .secret_storage_row
        .set(Some(&secret_storage_row));
    secret_storage_row.set_selected(secret_storage_mode_index(
        settings.application.secret_storage_mode,
    ));
    let secret_storage_shell = Rc::clone(shell);
    let secret_storage_guard = Rc::new(Cell::new(false));
    let secret_storage_guard_for_row = Rc::clone(&secret_storage_guard);
    let preferences_dialog_for_secret_storage = dialog.downgrade();
    secret_storage_row.connect_selected_notify(move |row| {
        if secret_storage_guard_for_row.get() {
            return;
        }
        let mode = secret_storage_mode_from_index(row.selected());
        let previous_mode = secret_storage_shell
            .settings
            .current
            .borrow()
            .application.secret_storage_mode;
        if mode == previous_mode {
            return;
        }

        let confirm = adw::AlertDialog::builder()
            .heading(tr("Change Secret Storage"))
            .body(tr(
                "This will log you out and clear your secrets including API ones. Your app cache is not touched.",
            ))
            .build();
        let cancel = tr("Cancel");
        let change = tr("Change");
        confirm.add_responses(&[("cancel", cancel.as_str()), ("change", change.as_str())]);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        confirm.set_response_appearance("change", adw::ResponseAppearance::Destructive);
        let row = row.clone();
        let shell = Rc::clone(&secret_storage_shell);
        let guard = Rc::clone(&secret_storage_guard_for_row);
        let preferences_dialog = preferences_dialog_for_secret_storage.clone();
        let window = shell.window.clone();
        confirm.choose(
            Some(&window),
            None::<&gtk::gio::Cancellable>,
            move |response| {
                if response.as_str() != "change" {
                    guard.set(true);
                    row.set_selected(secret_storage_mode_index(previous_mode));
                    guard.set(false);
                    return;
                }
                gtk::glib::spawn_future_local(async move {
                    if shell.set_secret_storage_mode(mode).await {
                        if let Some(preferences_dialog) = preferences_dialog.upgrade() {
                            preferences_dialog.close();
                        }
                        return;
                    }
                    guard.set(true);
                    row.set_selected(secret_storage_mode_index(previous_mode));
                    guard.set(false);
                });
            },
        );
    });
    cast_proxy_row.set_active(settings.application.cast_proxy_enabled);
    let cast_proxy_shell = Rc::clone(shell);
    cast_proxy_row.connect_active_notify(move |row| {
        cast_proxy_shell.settings.set_app_setting(
            "cast proxy setting",
            row.is_active(),
            |settings| &mut settings.application.cast_proxy_enabled,
        );
    });
    let cast_network_dropdown = casting_network_dropdown(&shell.player_ui, 220);
    cast_network_row.add_suffix(&cast_network_dropdown);
    cast_network_row.set_activatable_widget(Some(&cast_network_dropdown));

    privacy_page
}

fn secret_storage_mode_index(mode: SecretStorageMode) -> u32 {
    match mode {
        SecretStorageMode::ConfigFile => 0,
        SecretStorageMode::SystemKeyring => 1,
    }
}
fn secret_storage_mode_from_index(index: u32) -> SecretStorageMode {
    match index {
        1 => SecretStorageMode::SystemKeyring,
        _ => SecretStorageMode::ConfigFile,
    }
}
