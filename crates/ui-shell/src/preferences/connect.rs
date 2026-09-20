use crate::shell::Shell;
use adw::prelude::*;
use rufin_core::connect::{Action, Control, Encoding, Status, portable::Destination};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use ui_shared::popup::present_light_dismiss_dialog;

pub(crate) fn install(shell: &Rc<Shell>) {
    let mut updates = shell.products.connect.subscribe();
    let weak = Rc::downgrade(shell);
    let pairing = gtk::glib::spawn_future_local(async move {
        let mut previous = None;
        loop {
            let session = updates
                .borrow_and_update()
                .pairing
                .as_ref()
                .map(|pairing| pairing.session.clone());
            if session.is_some() && session != previous {
                let Some(shell) = weak.upgrade() else { return };
                present(&shell);
            }
            previous = session;
            if updates.changed().await.is_err() {
                return;
            }
        }
    });
    shell
        .chrome
        .window
        .connect_destroy(move |_| pairing.abort());

    let inactive = Rc::new(std::cell::Cell::new(None::<std::time::Instant>));
    let offered = Rc::new(std::cell::Cell::new(false));
    let weak = Rc::downgrade(shell);
    shell.chrome.window.connect_is_active_notify(move |window| {
        if !window.is_active() {
            inactive.set(Some(std::time::Instant::now()));
            return;
        }
        if offered.get() {
            return;
        }
        if inactive
            .take()
            .is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(300))
        {
            return;
        }
        let Some(shell) = weak.upgrade() else { return };
        let owner = shell.products.connect.clone();
        let result = shell
            .products
            .runtime
            .spawn(async move { owner.continuation_offer().await });
        let weak = Rc::downgrade(&shell);
        let offered = offered.clone();
        gtk::glib::spawn_future_local(async move {
            let Ok(Ok(Some(device))) = result.await else {
                return;
            };
            let Some(shell) = weak.upgrade() else { return };
            if offered.replace(true) {
                return;
            }
            let toast = adw::Toast::builder()
                .title(localization::tr_with(
                    "Continue from where {device} left off?",
                    &[("device", &device.name)],
                ))
                .button_label(localization::tr("Continue"))
                .timeout(0)
                .build();
            let weak = Rc::downgrade(&shell);
            toast.connect_button_clicked(move |_| {
                if let Some(shell) = weak.upgrade() {
                    run(
                        &shell,
                        Action::Continue {
                            peer: device.id.clone(),
                        },
                    );
                }
            });
            shell.chrome.toast_overlay.add_toast(toast);
        });
    });
}

fn show_feedback(shell: &Shell, message: String) {
    shell
        .preferences
        .connect_feedback
        .borrow()
        .as_ref()
        .unwrap_or(&shell.control_feedback)
        .show_feedback_toast(message);
}

fn show_connected(shell: &Rc<Shell>, connected: &str) {
    if !shell.products.connect.status().receiving_collection {
        shell
            .control_feedback
            .show_feedback_toast(connected.to_owned());
        return;
    }
    let toast = adw::Toast::builder()
        .title(shell.products.connect.status().profile_status)
        .button_label(localization::tr("Open Rufin Connect"))
        .timeout(0)
        .build();
    let weak = Rc::downgrade(shell);
    toast.connect_button_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            present(&shell);
        }
    });
    shell.chrome.toast_overlay.add_toast(toast.clone());
    let mut updates = shell.products.connect.subscribe();
    gtk::glib::spawn_future_local(async move {
        loop {
            let state = updates.borrow_and_update().clone();
            toast.set_title(&state.profile_status);
            if !state.receiving_collection {
                toast.set_timeout(5);
                return;
            }
            if updates.changed().await.is_err() {
                toast.dismiss();
                return;
            }
        }
    });
}

fn flash_success(button: &gtk::Button, original: &'static str) {
    button.set_icon_name("rufin-object-select-symbolic");
    let button = button.downgrade();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        if let Some(button) = button.upgrade() {
            button.set_icon_name(original);
        }
    });
}

fn run(shell: &Rc<Shell>, action: Action) {
    run_with_feedback(shell, action, None);
}

fn run_with_feedback(shell: &Rc<Shell>, action: Action, button: Option<&gtk::Button>) {
    let button = button.map(|button| button.downgrade());
    let testing_connection = matches!(action, Action::TestConnection { .. });
    let pending_import =
        matches!(action, Action::Import { replace: false, .. }).then(|| action.clone());
    let owner = shell.products.connect.clone();
    let result = shell
        .products
        .runtime
        .spawn(async move { owner.execute(action).await });
    let weak = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let result = result
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
        if result.is_ok()
            && let Some(button) = button.and_then(|button| button.upgrade())
        {
            flash_success(&button, "rufin-view-refresh-symbolic");
        }
        if let Err(error) = result
            && let Some(shell) = weak.upgrade()
        {
            if let Some(mut action) = pending_import
                && error
                    == "Connecting an existing profile will remove all data in this device, do you really want to continue?"
            {
                let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
                let builder = ui_shared::ui_resource::builder(resource);
                let dialog: adw::AlertDialog =
                    ui_shared::ui_resource::object(&builder, resource, "replace_confirmation");
                let parent = shell
                    .preferences
                    .connect_dialog
                    .upgrade()
                    .map(|dialog| dialog.upcast::<gtk::Widget>())
                    .unwrap_or_else(|| shell.chrome.window.clone().upcast());
                if dialog.choose_future(Some(&parent)).await == "join" {
                    if let Action::Import { replace, .. } = &mut action {
                        *replace = true;
                    }
                    run(&shell, action);
                }
                return;
            }
            show_feedback(
                &shell,
                if testing_connection {
                    localization::tr("Connection test has failed")
                } else {
                    error
                },
            );
        }
    });
}

pub(crate) fn bind_controller(
    shell: &Rc<Shell>,
    popover: &gtk::Popover,
    builder: &gtk::Builder,
    resource: &str,
) {
    ui_shared::objects!(builder, resource, {
        connect_enabled: adw::SwitchRow, open_connect: gtk::Button,
        continue_connect: gtk::Button,
    });
    connect_enabled.set_active(shell.products.connect.status().settings.enabled);
    let weak = Rc::downgrade(&shell);
    let popup = popover.downgrade();
    connect_enabled.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            let enabled = row.is_active();
            if shell.products.connect.status().settings.enabled != enabled {
                run(&shell, Action::Enable { enabled });
                if enabled {
                    if let Some(popup) = popup.upgrade() {
                        popup.popdown();
                    }
                    present(&shell);
                }
            }
        }
    });
    let weak = Rc::downgrade(&shell);
    let popup = popover.downgrade();
    open_connect.connect_clicked(move |_| {
        if let Some(popup) = popup.upgrade() {
            popup.popdown();
        }
        if let Some(shell) = weak.upgrade() {
            present(&shell);
        }
    });
    let offered_device = Rc::new(RefCell::new(None::<rufin_core::connect::Device>));
    let offered = offered_device.clone();
    let weak = Rc::downgrade(shell);
    let popup = popover.downgrade();
    continue_connect.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade()
            && let Some(device) = offered.borrow().as_ref()
        {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
            run(
                &shell,
                Action::Continue {
                    peer: device.id.clone(),
                },
            );
        }
    });
    let updates = shell.products.connect.subscribe();
    let row = connect_enabled.downgrade();
    let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let running = task.clone();
    let weak = Rc::downgrade(shell);
    let continue_button = continue_connect.downgrade();
    popover.connect_map(move |_| {
        let mut updates = updates.clone();
        let row = row.clone();
        if let Some(row) = row.upgrade() {
            row.set_active(updates.borrow().settings.enabled);
        }
        let button = continue_button.clone();
        let offered = offered_device.clone();
        let Some(shell) = weak.upgrade() else { return };
        if let Some(button) = button.upgrade() {
            button.set_visible(false);
        }
        let owner = shell.products.connect.clone();
        let offer = shell
            .products
            .runtime
            .spawn(async move { owner.continuation_offer().await });
        *running.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            let device = offer.await.ok().and_then(Result::ok).flatten();
            if let Some(button) = button.upgrade() {
                button.set_visible(device.is_some());
                if let Some(device) = &device {
                    let label = localization::tr_with(
                        "Continue from where {device} left off",
                        &[("device", &device.name)],
                    );
                    button.set_tooltip_text(Some(&label));
                    button.update_property(&[gtk::accessible::Property::Label(&label)]);
                }
            }
            *offered.borrow_mut() = device;
            loop {
                let enabled = updates.borrow_and_update().settings.enabled;
                let Some(row) = row.upgrade() else { return };
                row.set_active(enabled);
                if !enabled && let Some(button) = button.upgrade() {
                    button.set_visible(false);
                }
                drop(row);
                if updates.changed().await.is_err() {
                    return;
                }
            }
        }));
    });
    popover.connect_unmap(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
    });
}

fn present(shell: &Rc<Shell>) {
    if let Some(dialog) = shell.preferences.connect_dialog.upgrade() {
        present_light_dismiss_dialog(&dialog, &shell.chrome.window);
        return;
    }
    let resource = crate::ui_resource::CONNECT_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        connect_dialog: adw::Dialog, page_title: adw::WindowTitle,
        choose_join: gtk::ToggleButton, choose_invite: gtk::ToggleButton,
        connection_choices: gtk::Box, finish_setup: gtk::Button,
        join_method: adw::ComboRow, pairing: adw::PreferencesGroup,
        join_section: gtk::Box, invite_section: adw::ActionRow, file_section: gtk::Box,
        verify_section: gtk::Box, connecting_section: gtk::Box, devices_section: gtk::Box,
        media_section: gtk::Box, network_section: gtk::Box, approval_status: gtk::Label,
        membership_status_text: gtk::Label, connecting_status: gtk::Label,
        feedback_label: gtk::Label, connection_error: adw::Banner,
        device_name: adw::EntryRow, media_status: gtk::Label,
        devices: adw::PreferencesGroup, nearby_devices: adw::PreferencesGroup,
        pair_name: gtk::Label, emoji: gtk::Label,
        peer: adw::EntryRow, nearby: adw::SwitchRow, relay: adw::EntryRow,
        relay_enabled: gtk::Switch,
        folder_title: gtk::Label, file_title: gtk::Label, storage_chooser: gtk::FileDialog,
        local_storage_help: gtk::Label, storage_help: gtk::Image, folders_section: gtk::Box,
        encoding: adw::ComboRow, storage: adw::ComboRow,
        choose_connection: gtk::Button,
        file: adw::EntryRow,
        folders: adw::PreferencesGroup,
        create: gtk::Button, copy_invitation: gtk::Button, join: gtk::Button,
        approve: gtk::Button, reject: gtk::Button, refresh: gtk::Button,
        cancel_pairing: gtk::Button,
        import: gtk::Button, save_storage: gtk::Button, browse_file: gtk::Button,

        storage_applying_text: gtk::Label, storage_applied_text: gtk::Label,
        leave: gtk::Button, file_chooser: gtk::FileDialog,
        folder_chooser: gtk::FileDialog,
    });
    *shell.preferences.connect_feedback.borrow_mut() = Some(
        ui_shared::feedback::ControlFeedbackState::new(&feedback_label, shell.settings.clone()),
    );
    let weak = Rc::downgrade(shell);
    connect_dialog.connect_closed(move |_| {
        if let Some(shell) = weak.upgrade() {
            shell.preferences.connect_dialog.set(None::<&adw::Dialog>);
            shell.preferences.connect_feedback.borrow_mut().take();
        }
    });
    shell.preferences.connect_dialog.set(Some(&connect_dialog));
    let initial = shell.products.connect.status();
    device_name.set_text(&initial.settings.name);
    nearby.set_active(initial.settings.nearby);
    relay.set_text(
        initial
            .settings
            .relay
            .as_deref()
            .unwrap_or(if initial.settings.public_relay {
                "https://euc1-1.relay.n0.iroh.link/"
            } else {
                ""
            }),
    );
    relay_enabled.set_active(initial.settings.relay.is_some() || initial.settings.public_relay);
    relay.set_visible(relay_enabled.is_active());
    let updating_network = Rc::new(Cell::new(false));
    encoding.set_selected(u32::from(initial.settings.encoding == Encoding::Mp3));
    let accounts = Rc::new(RefCell::new(shell.products.source.file_integrations()));
    let storage_draft = Rc::new(RefCell::new(StorageDraft::default()));
    let reload_accounts: Rc<dyn Fn() -> bool> = {
        let weak = Rc::downgrade(shell);
        let accounts = accounts.clone();
        Rc::new(move || {
            let Some(shell) = weak.upgrade() else {
                return false;
            };
            let available = shell.products.source.file_integrations();
            if *accounts.borrow() == available {
                return false;
            }
            *accounts.borrow_mut() = available;
            true
        })
    };
    show_destination(
        &storage,
        &file,
        &storage_draft,
        &accounts.borrow(),
        &initial,
    );
    let applying_storage = Rc::new(Cell::new(false));
    let apply_label = save_storage.label().unwrap_or_default();
    let update_storage: Rc<dyn Fn()> = {
        let applying = applying_storage.clone();
        let label = apply_label.clone();
        let weak = Rc::downgrade(shell);
        let storage = storage.downgrade();
        let file = file.downgrade();
        let apply = save_storage.downgrade();
        let group = storage_help.downgrade();
        let description = storage_help.tooltip_text();
        let local_description = local_storage_help.text();
        let importing = import.downgrade();
        let storage_draft = storage_draft.clone();
        Rc::new(move || {
            let (Some(shell), Some(storage), Some(file), Some(apply), Some(group)) = (
                weak.upgrade(),
                storage.upgrade(),
                file.upgrade(),
                apply.upgrade(),
                group.upgrade(),
            ) else {
                return;
            };
            let remote = storage.selected() > 0;
            group.set_tooltip_text(
                if importing
                    .upgrade()
                    .is_some_and(|button| button.is_visible())
                {
                    None
                } else if remote {
                    description.as_deref()
                } else {
                    Some(&local_description)
                },
            );
            let source = storage_draft.borrow().source.clone();
            let path = file.text().to_string();
            let draft = match source {
                Some(source_id) => Destination::Remote {
                    source_id,
                    path: path.clone(),
                },
                None => Destination::Local {
                    path: path.clone().into(),
                },
            };
            let changed = Some(draft) != shell.products.connect.status().settings.destination;
            if changed && !applying.get() {
                apply.set_label(&label);
            }
            apply.set_sensitive(
                !applying.get()
                    && storage.selected() != gtk::INVALID_LIST_POSITION
                    && (!remote || storage_draft.borrow().source.is_some())
                    && !path.is_empty()
                    && changed,
            );
        })
    };
    let changed = update_storage.clone();
    file.connect_changed(move |_| changed());
    update_storage();
    let update_view: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(shell);
        let choose_join = choose_join.downgrade();
        let connection_choices = connection_choices.downgrade();
        let finish_setup = finish_setup.downgrade();
        let join_method = join_method.downgrade();
        let join_section = join_section.downgrade();
        let invite_section = invite_section.downgrade();
        let file_section = file_section.downgrade();
        let verify_section = verify_section.downgrade();
        let connecting_section = connecting_section.downgrade();
        let devices_section = devices_section.downgrade();
        let network_section = network_section.downgrade();
        let media_section = media_section.downgrade();
        let leave = leave.downgrade();
        let pairing = pairing.downgrade();
        let nearby_devices = nearby_devices.downgrade();
        let import = import.downgrade();
        let save_storage = save_storage.downgrade();
        let approve = approve.downgrade();
        let storage_changed = update_storage.clone();
        let file = file.downgrade();
        let browse_file = browse_file.downgrade();
        let folder_title = folder_title.text();
        let file_title = file_title.text();
        let waiting_for_approval = approval_status.text();
        let connecting_devices = membership_status_text.text();
        let approval_status = approval_status.downgrade();
        let title = page_title.downgrade();
        Rc::new(move || {
            let (Some(shell), Some(choice), Some(method)) =
                (weak.upgrade(), choose_join.upgrade(), join_method.upgrade())
            else {
                return;
            };
            let state = shell.products.connect.status();
            let busy = state.pairing.is_some() || state.connecting;
            let established = state.settings.established;
            let setup = state.settings.setup_pending;
            let joining = choice.is_active() && !established;
            let importing = joining && method.selected() == 1;
            if let Some(title) = title.upgrade() {
                title.set_subtitle(if (joining || setup) && !busy {
                    &state.profile_status
                } else {
                    ""
                });
            }
            if let Some(choices) = connection_choices.upgrade() {
                choices.set_visible(!established && !busy);
            }
            if let Some(button) = finish_setup.upgrade() {
                button.set_visible(setup && !busy);
            }
            if let Some(invite) = invite_section.upgrade() {
                invite.set_visible(!joining && !busy && !setup);
            }
            let profile_controls = established && !busy;
            for (section, visible) in [
                (&join_section, joining && !busy),
                (&file_section, (profile_controls || importing) && !busy),
                (&verify_section, state.pairing.is_some()),
                (&connecting_section, state.connecting),
                (
                    &network_section,
                    !busy && (profile_controls || (joining && !importing)),
                ),
                (&media_section, profile_controls),
                (
                    &devices_section,
                    !busy && !setup && state.devices.iter().any(|device| device.enrolled),
                ),
            ] {
                if let Some(section) = section.upgrade() {
                    section.set_visible(visible);
                }
            }
            if let Some(group) = pairing.upgrade() {
                group.set_visible(!importing);
            }
            if let Some(group) = nearby_devices.upgrade() {
                group.set_visible(
                    joining
                        && !busy
                        && !importing
                        && state.devices.iter().any(|device| !device.enrolled),
                );
            }
            if let Some(button) = leave.upgrade() {
                button.set_visible(profile_controls);
            }
            if let Some(file) = file.upgrade() {
                file.set_title(if importing {
                    &file_title
                } else {
                    &folder_title
                });
            }
            if let Some(browse) = browse_file.upgrade() {
                browse.set_tooltip_text(Some(if importing {
                    &file_title
                } else {
                    &folder_title
                }));
            }
            if let Some(button) = import.upgrade() {
                button.set_visible(importing);
            }
            if let Some(button) = save_storage.upgrade() {
                button.set_visible(!importing);
            }
            storage_changed();
            let approved = state
                .pairing
                .as_ref()
                .is_some_and(|pairing| pairing.approved);
            if let Some(button) = approve.upgrade() {
                button.set_visible(!approved);
            }
            if let Some(label) = approval_status.upgrade() {
                label.set_visible(approved);
                label.set_text(
                    if state
                        .pairing
                        .as_ref()
                        .is_some_and(|pairing| pairing.verified)
                    {
                        &connecting_devices
                    } else {
                        &waiting_for_approval
                    },
                );
            }
        })
    };
    let select_method: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(shell);
        let choice = choose_join.downgrade();
        let method = join_method.downgrade();
        let file = file.downgrade();
        let storage = storage.downgrade();
        let storage_draft = storage_draft.clone();
        let accounts = accounts.clone();
        let update_view = update_view.clone();
        Rc::new(move || {
            let (Some(shell), Some(choice), Some(method), Some(file), Some(storage)) = (
                weak.upgrade(),
                choice.upgrade(),
                method.upgrade(),
                file.upgrade(),
                storage.upgrade(),
            ) else {
                return;
            };
            if choice.is_active() && method.selected() == 1 {
                file.set_text("");
            } else {
                show_destination(
                    &storage,
                    &file,
                    &storage_draft,
                    &accounts.borrow(),
                    &shell.products.connect.status(),
                );
            }
            update_view();
        })
    };
    let select = select_method.clone();
    let weak = Rc::downgrade(shell);
    choose_join.connect_toggled(move |button| {
        select();
        if button.is_active()
            && let Some(shell) = weak.upgrade()
        {
            run(&shell, Action::Discover);
        }
    });
    let select = select_method.clone();
    let weak = Rc::downgrade(shell);
    choose_invite.connect_toggled(move |button| {
        if button.is_active() {
            select();
            if let Some(shell) = weak.upgrade()
                && !shell.products.connect.status().settings.enabled
            {
                run(&shell, Action::Enable { enabled: true });
            }
        }
    });
    join_method.connect_selected_notify(move |_| select_method());
    update_view();
    browse_file.set_visible(storage.selected() == 0);
    let browse = browse_file.downgrade();
    let file_weak = file.downgrade();
    let weak = Rc::downgrade(shell);
    let storage_accounts = accounts.clone();
    let draft = storage_draft.clone();
    let parent = connect_dialog.downgrade();
    let storage_changed = update_storage.clone();
    let choose = choose_connection.downgrade();
    choose_connection.set_visible(storage.selected() > 0);
    let importing = import.downgrade();
    let select_connection: Rc<dyn Fn(&adw::ComboRow, bool)> = Rc::new(move |row, choose_again| {
        if let Some(browse) = browse.upgrade() {
            browse.set_visible(row.selected() == 0);
        }
        if let Some(choose) = choose.upgrade() {
            choose.set_visible(row.selected() > 0);
        }
        if draft.borrow().updating || (!choose_again && row.selected() == draft.borrow().selected) {
            return;
        }
        let selected = row.selected();
        let importing = importing
            .upgrade()
            .is_some_and(|button| button.is_visible());
        let (Some(file), Some(shell), Some(parent)) =
            (file_weak.upgrade(), weak.upgrade(), parent.upgrade())
        else {
            return;
        };
        if selected == 0 {
            draft.borrow_mut().source = None;
            draft.borrow_mut().selected = 0;
            row.set_subtitle("");
            file.set_text(&if importing {
                String::new()
            } else {
                shell.products.connect.status().storage_path(None)
            });
            storage_changed();
            return;
        }
        let previous = draft.borrow().selected;
        draft.borrow_mut().updating = true;
        row.set_selected(previous);
        draft.borrow_mut().updating = false;
        let kind = if selected == 2 { "smb" } else { "webdav" };
        let draft = draft.clone();
        let accounts = storage_accounts.clone();
        let file = file.downgrade();
        let weak = Rc::downgrade(&shell);
        let row = row.downgrade();
        let changed = storage_changed.clone();
        storage_changed();
        super::source::login::choose_integration(
            &shell,
            &parent,
            kind,
            Rc::new(move |id| {
                let (Some(shell), Some(file), Some(row)) =
                    (weak.upgrade(), file.upgrade(), row.upgrade())
                else {
                    return;
                };
                *accounts.borrow_mut() = shell.products.source.file_integrations();
                {
                    let mut draft = draft.borrow_mut();
                    draft.source = Some(id.clone());
                    draft.selected = selected;
                    draft.updating = true;
                }
                row.set_selected(selected);
                draft.borrow_mut().updating = false;
                row.set_subtitle(
                    accounts
                        .borrow()
                        .iter()
                        .find(|a| a.id == id)
                        .map(|a| a.name.as_str())
                        .unwrap_or(""),
                );
                file.set_text(&if importing {
                    String::new()
                } else {
                    shell.products.connect.status().storage_path(Some(&id))
                });
                changed();
            }),
        );
    });
    let select = select_connection.clone();
    storage.connect_selected_notify(move |row| select(row, false));
    let row = storage.downgrade();
    choose_connection.connect_clicked(move |_| {
        if let Some(row) = row.upgrade() {
            select_connection(&row, true);
        }
    });
    let weak = Rc::downgrade(shell);
    let folders_weak = folders.downgrade();
    let parent = shell.chrome.window.downgrade();
    folders
        .bind_property("visible", &folders_section, "visible")
        .sync_create()
        .build();
    let folder_rows = Rc::new(RefCell::new(Vec::<adw::EntryRow>::new()));
    let load_folders: Rc<dyn Fn()> = Rc::new(move || {
        let (Some(shell), Some(group), Some(parent)) =
            (weak.upgrade(), folders_weak.upgrade(), parent.upgrade())
        else {
            return;
        };
        let owner = shell.products.connect.clone();
        let sources = shell.products.source.list_sources().sources;
        let result = shell.products.runtime.spawn(async move {
            let mut mappings = Vec::new();
            for source in sources.iter() {
                for root in owner.roots(source.id.as_str()).await? {
                    mappings.push((source.id.to_string(), source.name.clone(), root));
                }
            }
            Ok::<_, String>(mappings)
        });
        let rows = folder_rows.clone();
        let chooser = folder_chooser.clone();
        gtk::glib::spawn_future_local(async move {
            let values = match result.await {
                Ok(Ok(values)) => values,
                Ok(Err(error)) => {
                    show_feedback(&shell, error);
                    return;
                }
                Err(_) => return,
            };
            for row in rows.borrow_mut().drain(..) {
                group.remove(&row);
            }
            group.set_visible(!values.is_empty());
            for (source, name, root) in values {
                let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
                let builder = ui_shared::ui_resource::builder(resource);
                ui_shared::objects!(builder, resource, { folder_mapping: adw::EntryRow, browse_folder: gtk::Button });
                folder_mapping.set_title(&format!("{name} · {}", root.label));
                let key = format!("{source}/{}", root.id);
                let settings = shell.products.connect.status().settings;
                let path = settings
                    .folders
                    .get(&key)
                    .or_else(|| settings.folders.get(&source));
                folder_mapping.set_text(
                    &path
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
                let weak = Rc::downgrade(&shell);
                let id = root.id.clone();
                let source_id = source.clone();
                folder_mapping.connect_apply(move |row| {
                    if let Some(shell) = weak.upgrade() {
                        run(
                            &shell,
                            Action::Folder {
                                source: source_id.clone(),
                                root_id: Some(id.clone()),
                                path: row.text().as_str().into(),
                            },
                        );
                    }
                });
                let weak = Rc::downgrade(&shell);
                let row = folder_mapping.downgrade();
                let parent = parent.downgrade();
                let chooser = chooser.clone();
                browse_folder.connect_clicked(move |_| {
                    let (Some(shell), Some(row), Some(parent)) =
                        (weak.upgrade(), row.upgrade(), parent.upgrade())
                    else {
                        return;
                    };
                    let chooser = chooser.clone();
                    let source = source.clone();
                    let root_id = root.id.clone();
                    gtk::glib::spawn_future_local(async move {
                        if let Ok(file) = chooser.select_folder_future(Some(&parent)).await
                            && let Some(path) = file.path()
                        {
                            row.set_text(&path.to_string_lossy());
                            run(
                                &shell,
                                Action::Folder {
                                    source,
                                    root_id: Some(root_id),
                                    path,
                                },
                            );
                        }
                    });
                });
                group.add(&folder_mapping);
                rows.borrow_mut().push(folder_mapping);
            }
        });
    });
    let weak = Rc::downgrade(shell);
    device_name.connect_apply(move |row| {
        if let Some(shell) = weak.upgrade() {
            run(
                &shell,
                Action::Rename {
                    name: row.text().into(),
                },
            );
        }
    });
    let weak = Rc::downgrade(shell);
    copy_invitation.connect_clicked(move |button| {
        if let Some(shell) = weak.upgrade()
            && let Some(invitation) = shell.products.connect.status().invitation
        {
            button.clipboard().set_text(&invitation);
            flash_success(button, "rufin-edit-copy-symbolic");
        }
    });
    let reload_folders = load_folders.clone();
    refresh.connect_clicked(move |_| reload_folders());
    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    leave.connect_clicked(move |_| {
        let (Some(shell), Some(parent)) = (weak.upgrade(), parent.upgrade()) else {
            return;
        };
        gtk::glib::spawn_future_local(async move {
            let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                ui_shared::ui_resource::object(&builder, resource, "disconnect_confirmation");
            if dialog.choose_future(Some(&parent)).await == "disconnect" {
                run(&shell, Action::Leave);
            }
        });
    });
    for (button, action) in [
        (create.clone(), Action::Enable { enabled: true }),
        (refresh, Action::Refresh),
        (cancel_pairing, Action::CancelPairing),
        (finish_setup, Action::FinishSetup),
    ] {
        let weak = Rc::downgrade(shell);
        button.connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                run(&shell, action.clone());
            }
        });
    }
    let weak = Rc::downgrade(shell);
    let input = peer.downgrade();
    let parent = connect_dialog.downgrade();
    join.connect_clicked(move |_| {
        if let (Some(shell), Some(peer), Some(parent)) =
            (weak.upgrade(), input.upgrade(), parent.upgrade())
        {
            join_profile(&shell, &parent, peer.text().into());
        }
    });
    for (button, approve) in [(approve, true), (reject, false)] {
        let weak = Rc::downgrade(shell);
        button.connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade()
                && let Some(pairing) = shell.products.connect.status().pairing
            {
                run(
                    &shell,
                    Action::Pair {
                        session: pairing.session,
                        approve,
                    },
                );
            }
        });
    }
    let weak = Rc::downgrade(shell);
    let updating = updating_network.clone();
    nearby.connect_active_notify(move |row| {
        if updating.get() {
            return;
        }
        if let Some(shell) = weak.upgrade() {
            let settings = shell.products.connect.status().settings;
            run(
                &shell,
                Action::Network {
                    nearby: row.is_active(),
                    relay: settings.relay,
                    public_relay: settings.public_relay,
                },
            );
        }
    });
    let weak = Rc::downgrade(shell);
    relay.connect_apply(move |row| {
        if let Some(shell) = weak.upgrade() {
            run(
                &shell,
                Action::Network {
                    nearby: shell.products.connect.status().settings.nearby,
                    relay: Some(row.text().trim().into()),
                    public_relay: false,
                },
            );
        }
    });
    let weak = Rc::downgrade(shell);
    let relay_row = relay.downgrade();
    let updating = updating_network.clone();
    relay_enabled.connect_active_notify(move |row| {
        let Some(relay) = relay_row.upgrade() else {
            return;
        };
        relay.set_visible(row.is_active());
        if !updating.get()
            && (!row.is_active() || !relay.text().trim().is_empty())
            && let Some(shell) = weak.upgrade()
        {
            run(
                &shell,
                Action::Network {
                    nearby: shell.products.connect.status().settings.nearby,
                    relay: row.is_active().then(|| relay.text().trim().into()),
                    public_relay: false,
                },
            );
        }
    });
    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    encoding.connect_selected_notify(move |row| {
        let (Some(shell), Some(parent)) = (weak.upgrade(), parent.upgrade()) else {
            return;
        };
        let encoding = if row.selected() == 0 {
            Encoding::Original
        } else {
            Encoding::Mp3
        };
        let settings = shell.products.connect.status().settings;
        if encoding == settings.encoding {
            return;
        }
        if !settings.established || settings.setup_pending {
            run(
                &shell,
                Action::Encoding {
                    encoding,
                    confirm: false,
                },
            );
            return;
        }
        row.set_sensitive(false);
        let row = row.downgrade();
        gtk::glib::spawn_future_local(async move {
            let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                ui_shared::ui_resource::object(&builder, resource, "encoding_confirmation");
            let accepted = dialog.choose_future(Some(&parent)).await == "continue";
            if let Some(row) = row.upgrade() {
                row.set_sensitive(true);
                if accepted {
                    run(
                        &shell,
                        Action::Encoding {
                            encoding,
                            confirm: true,
                        },
                    );
                } else {
                    row.set_selected(u32::from(
                        shell.products.connect.status().settings.encoding == Encoding::Mp3,
                    ));
                }
            }
        });
    });
    let parent = shell.chrome.window.downgrade();
    let entry = file.downgrade();
    let chooser = file_chooser.clone();
    let folders = storage_chooser.clone();
    let importing = import.downgrade();
    browse_file.connect_clicked(move |_| {
        let (Some(parent), Some(entry)) = (parent.upgrade(), entry.upgrade()) else {
            return;
        };
        let chooser = chooser.clone();
        let folders = folders.clone();
        let importing = importing
            .upgrade()
            .is_some_and(|button| button.is_visible());
        gtk::glib::spawn_future_local(async move {
            let selected = if importing {
                chooser.open_future(Some(&parent)).await
            } else {
                folders.select_folder_future(Some(&parent)).await
            };
            if let Ok(file) = selected
                && let Some(path) = file.path()
            {
                entry.set_text(&path.to_string_lossy());
            }
        });
    });
    for (button, importing) in [(import.clone(), true), (save_storage, false)] {
        let applying = applying_storage.clone();
        let applying_label = storage_applying_text.label();
        let applied_label = storage_applied_text.label();
        let apply_label = apply_label.clone();
        let update_storage = update_storage.clone();
        let weak = Rc::downgrade(shell);
        let parent = shell.chrome.window.downgrade();
        let entry = file.downgrade();
        let storage = storage.downgrade();
        let storage_draft = storage_draft.clone();
        let chooser = file_chooser.clone();
        let folders = storage_chooser.clone();
        button.connect_clicked(move |button| {
            let (Some(shell), Some(parent), Some(entry), Some(storage)) = (
                weak.upgrade(),
                parent.upgrade(),
                entry.upgrade(),
                storage.upgrade(),
            ) else {
                return;
            };
            let remote = storage.selected() > 0;
            let source_id = storage_draft.borrow().source.clone();
            if remote && source_id.is_none() {
                show_feedback(
                    &shell,
                    localization::tr("Choose a connection for profile storage."),
                );
                return;
            }
            if !importing {
                applying.set(true);
                button.set_label(&applying_label);
                button.set_sensitive(false);
            }
            let chooser = chooser.clone();
            let folders = folders.clone();
            let button = button.downgrade();
            let applying = applying.clone();
            let applied_label = applied_label.clone();
            let apply_label = apply_label.clone();
            let update_storage = update_storage.clone();
            gtk::glib::spawn_future_local(async move {
                let path = if remote || !entry.text().is_empty() {
                    entry.text().as_str().into()
                } else {
                    let result = if importing {
                        chooser.open_future(Some(&parent)).await
                    } else {
                        folders.select_folder_future(Some(&parent)).await
                    };
                    let Ok(file) = result else { return };
                    let Some(path) = file.path() else { return };
                    entry.set_text(&path.to_string_lossy());
                    path
                };
                if importing {
                    run(
                        &shell,
                        Action::Import {
                            path,
                            source_id,
                            replace: false,
                            key: None,
                        },
                    );
                } else {
                    let action = Action::Destination {
                        destination: Some(match source_id {
                            Some(source_id) => Destination::Remote {
                                source_id,
                                path: path.to_string_lossy().into_owned(),
                            },
                            None => Destination::Local { path },
                        }),
                    };
                    let owner = shell.products.connect.clone();
                    let result = shell
                        .products
                        .runtime
                        .spawn(async move { owner.execute(action).await })
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                    applying.set(false);
                    update_storage();
                    match result {
                        Ok(_) => {
                            if let Some(button) = button.upgrade() {
                                button.set_label(&applied_label);
                            }
                            gtk::glib::timeout_future(std::time::Duration::from_secs(2)).await;
                            if let Some(button) = button.upgrade()
                                && button.label().as_deref() == Some(applied_label.as_str())
                            {
                                button.set_label(&apply_label);
                            }
                        }
                        Err(error) => {
                            if let Some(button) = button.upgrade() {
                                button.set_label(&apply_label);
                                button.set_sensitive(true);
                            }
                            show_feedback(&shell, error);
                        }
                    }
                }
            });
        });
    }

    let mut updates = shell.products.connect.subscribe();
    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    let device_rows = RefCell::new(Vec::new());
    let nearby_rows = RefCell::new(Vec::new());
    load_folders();
    let task = gtk::glib::spawn_future_local(async move {
        let mut previous_devices = None;
        let mut previous_profile = initial.settings.profile;
        let mut previous_setup = initial.settings.setup_pending && !initial.connecting;
        let mut completed_pairings = initial.completed_pairings;
        let mut previous_network = (
            initial.settings.nearby,
            initial.settings.relay,
            initial.settings.public_relay,
        );
        let mut previous_destination = initial.settings.destination;
        loop {
            let state = updates.borrow_and_update().clone();
            let (Some(shell), Some(parent)) = (weak.upgrade(), parent.upgrade()) else {
                return;
            };
            if state.completed_pairings != completed_pairings {
                parent.close();
                show_connected(&shell, &localization::tr("Connected"));
                return;
            }
            completed_pairings = state.completed_pairings;
            let accounts_changed = reload_accounts();
            if (accounts_changed || previous_destination != state.settings.destination)
                && !import.is_visible()
            {
                show_destination(&storage, &file, &storage_draft, &accounts.borrow(), &state);
                previous_destination = state.settings.destination.clone();
            }
            let network = (
                state.settings.nearby,
                state.settings.relay.clone(),
                state.settings.public_relay,
            );
            if previous_network != network {
                updating_network.set(true);
                nearby.set_active(network.0);
                if let Some(address) = network.1.as_deref() {
                    relay.set_text(address);
                } else if network.2 {
                    relay.set_text("https://euc1-1.relay.n0.iroh.link/");
                }
                relay_enabled.set_active(network.1.is_some() || network.2);
                updating_network.set(false);
                previous_network = network;
            }
            update_storage();
            connecting_status.set_text(&localization::tr(&state.profile_status));
            connection_error.set_title(state.error.as_deref().unwrap_or_default());
            connection_error.set_revealed(state.error.is_some());
            let message = state.media_status.as_deref();
            media_status.set_visible(message.is_some());
            media_status.set_text(&localization::tr(message.unwrap_or_default()));
            create.set_visible(!state.settings.enabled);
            copy_invitation.set_visible(state.invitation.is_some());
            if let Some(pairing) = &state.pairing {
                pair_name.set_text(&pairing.name);
                emoji.set_text(&pairing.emoji.join(" "));
            }
            let setup_ready = state.settings.setup_pending && !state.connecting;
            if previous_profile != state.settings.profile || previous_setup != setup_ready {
                previous_profile = state.settings.profile.clone();
                previous_setup = setup_ready;
                load_folders();
            }
            let membership: Vec<_> = state
                .devices
                .iter()
                .map(|device| (device.id.clone(), device.enrolled))
                .collect();
            if previous_devices.as_ref() != Some(&membership) {
                for row in device_rows.borrow_mut().drain(..) {
                    devices.remove(&row);
                }
                for row in nearby_rows.borrow_mut().drain(..) {
                    nearby_devices.remove(&row);
                }
                render_devices(
                    &shell,
                    &parent,
                    &devices,
                    &nearby_devices,
                    &state,
                    &device_rows,
                    &nearby_rows,
                );
                previous_devices = Some(membership);
            }
            for (enrolled, rows) in [(true, &device_rows), (false, &nearby_rows)] {
                for (device, row) in state
                    .devices
                    .iter()
                    .filter(|device| device.enrolled == enrolled)
                    .zip(rows.borrow().iter())
                {
                    row.set_title(&device.name);
                    row.set_subtitle(&device.connection);
                    row.remove_css_class(if device.reachable {
                        "connect-device-offline"
                    } else {
                        "connect-device-online"
                    });
                    if device.enrolled {
                        row.add_css_class(if device.reachable {
                            "connect-device-online"
                        } else {
                            "connect-device-offline"
                        });
                    }
                }
            }
            update_view();
            drop(shell);
            drop(parent);
            if updates.changed().await.is_err() {
                return;
            }
        }
    });
    connect_dialog.connect_closed(move |_| task.abort());
    present_light_dismiss_dialog(&connect_dialog, &shell.chrome.window);
}

#[derive(Default)]
struct StorageDraft {
    source: Option<sources::SourceId>,
    selected: u32,
    updating: bool,
}

fn show_destination(
    storage: &adw::ComboRow,
    file: &adw::EntryRow,
    draft: &RefCell<StorageDraft>,
    accounts: &[rufin_core::source::FileIntegration],
    state: &Status,
) {
    let source = state
        .settings
        .destination
        .as_ref()
        .and_then(Destination::source_id)
        .cloned();
    {
        let mut draft = draft.borrow_mut();
        draft.updating = true;
        draft.source = source.clone();
    }
    let connection = source
        .as_ref()
        .and_then(|id| accounts.iter().find(|a| &a.id == id));
    storage.set_selected(if source.is_none() {
        0
    } else if connection.is_some_and(|a| a.kind == "smb") {
        2
    } else {
        1
    });
    draft.borrow_mut().selected = storage.selected();
    file.set_text(&state.storage_path(source.as_ref()));
    storage.set_subtitle(
        source
            .as_ref()
            .and_then(|id| accounts.iter().find(|a| &a.id == id))
            .map(|a| a.name.as_str())
            .unwrap_or(""),
    );
    draft.borrow_mut().updating = false;
}

fn join_profile(shell: &Rc<Shell>, parent: &adw::Dialog, invitation: String) {
    let shell = shell.clone();
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        let dialog: adw::AlertDialog =
            ui_shared::ui_resource::object(&builder, resource, "replace_confirmation");
        if dialog.choose_future(Some(&parent)).await == "join" {
            run(
                &shell,
                Action::Join {
                    invitation,
                    replace: true,
                },
            );
        }
    });
}

fn render_devices(
    shell: &Rc<Shell>,
    parent: &adw::Dialog,
    group: &adw::PreferencesGroup,
    nearby: &adw::PreferencesGroup,
    state: &Status,
    rows: &RefCell<Vec<adw::ActionRow>>,
    nearby_rows: &RefCell<Vec<adw::ActionRow>>,
) {
    for device in &state.devices {
        let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder,resource,{device_row:adw::ActionRow,play:gtk::Button,pause:gtk::Button,remove:gtk::Button,join_device:gtk::Button,test_connection:gtk::Button});
        device_row.set_title(&device.name);
        device_row.set_subtitle(&device.connection);
        if device.enrolled {
            device_row.add_css_class(if device.reachable {
                "connect-device-online"
            } else {
                "connect-device-offline"
            });
        }
        play.set_visible(device.enrolled);
        pause.set_visible(device.enrolled);
        remove.set_visible(device.enrolled);
        test_connection.set_visible(device.enrolled);
        join_device.set_visible(!device.enrolled);
        let weak = Rc::downgrade(shell);
        let parent = parent.downgrade();
        let invitation = device.id.clone();
        join_device.connect_clicked(move |_| {
            if let (Some(shell), Some(parent)) = (weak.upgrade(), parent.upgrade()) {
                join_profile(&shell, &parent, invitation.clone());
            }
        });
        for (button, action) in [
            (
                play,
                Action::Control {
                    peer: device.id.clone(),
                    command: Control::Play,
                },
            ),
            (
                pause,
                Action::Control {
                    peer: device.id.clone(),
                    command: Control::Pause,
                },
            ),
            (
                remove,
                Action::Remove {
                    peer: device.id.clone(),
                },
            ),
            (
                test_connection,
                Action::TestConnection {
                    peer: device.id.clone(),
                },
            ),
        ] {
            let weak = Rc::downgrade(shell);
            button.connect_clicked(move |button| {
                if let Some(shell) = weak.upgrade() {
                    run_with_feedback(
                        &shell,
                        action.clone(),
                        matches!(action, Action::TestConnection { .. }).then_some(button),
                    );
                }
            });
        }
        if device.enrolled {
            group.add(&device_row);
            rows.borrow_mut().push(device_row);
        } else {
            nearby.add(&device_row);
            nearby_rows.borrow_mut().push(device_row);
        }
    }
}
