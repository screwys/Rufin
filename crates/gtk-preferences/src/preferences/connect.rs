use crate::Preferences;
use adw::prelude::*;
use gtk_widgets::popup::present_light_dismiss_dialog;
use rufin_core::connect::{
    Action, ActionError, CollectionProgress, CollectionStage, Confirmation, Control, Status,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub fn install(shell: &Rc<Preferences>) {
    let mut updates = shell.products.connect.subscribe();
    let weak = Rc::downgrade(shell);
    let pairing = gtk::glib::spawn_future_local(async move {
        let mut previous = None;
        let mut working = updates.borrow().connecting;
        loop {
            let state = updates.borrow_and_update().clone();
            let session = state
                .pairing
                .as_ref()
                .map(|pairing| pairing.session.clone());
            let active = state.connecting;
            if (session.is_some() && session != previous) || (active && !working) {
                let Some(shell) = weak.upgrade() else { return };
                present(&shell);
            }
            previous = session;
            working = active;
            if updates.changed().await.is_err() {
                return;
            }
        }
    });
    shell.window.connect_destroy(move |_| pairing.abort());
}

fn show_feedback(shell: &Preferences, message: String) {
    shell
        .state
        .connect_feedback
        .borrow()
        .as_ref()
        .unwrap_or(&shell.control_feedback)
        .show_feedback_toast(message);
}

fn show_connected(shell: &Rc<Preferences>, connected: &str) {
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
    shell.toast_overlay.add_toast(toast.clone());
    let mut updates = shell.products.connect.subscribe();
    gtk::glib::spawn_future_local(async move {
        loop {
            let state = updates.borrow_and_update().clone();
            toast.set_title(state.error.as_deref().unwrap_or(&state.profile_status));
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

fn run(shell: &Rc<Preferences>, action: Action) {
    run_with_feedback(shell, action, None);
}

fn run_with_feedback(shell: &Rc<Preferences>, action: Action, button: Option<&gtk::Button>) {
    let button = button.map(|button| button.downgrade());
    let testing_connection = matches!(action, Action::TestConnection { .. });
    let pending_replace = matches!(
        action,
        Action::Import { replace: false, .. }
            | Action::Pair {
                replace: false,
                approve: true,
                ..
            }
    )
    .then(|| action.clone());
    let owner = shell.products.connect.clone();
    let result = shell
        .products
        .runtime
        .spawn(async move { owner.execute(action).await });
    let weak = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let result = result
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))
            .and_then(|result| result);
        if result.is_ok()
            && let Some(button) = button.and_then(|button| button.upgrade())
        {
            flash_success(&button, "rufin-view-refresh-symbolic");
        }
        if let Err(error) = result
            && let Some(shell) = weak.upgrade()
        {
            if let Some(mut action) = pending_replace
                && matches!(
                    error,
                    ActionError::ConfirmationRequired {
                        confirmation: Confirmation::ReplaceProfile,
                        ..
                    }
                )
            {
                let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
                let builder = gtk_widgets::ui_resource::builder(resource);
                let dialog: adw::AlertDialog =
                    gtk_widgets::ui_resource::object(&builder, resource, "replace_confirmation");
                let parent = shell
                    .state
                    .connect_dialog
                    .upgrade()
                    .map(|dialog| dialog.upcast::<gtk::Widget>())
                    .unwrap_or_else(|| shell.window.clone().upcast());
                if dialog.choose_future(Some(&parent)).await == "join" {
                    if let Action::Import { replace, .. } | Action::Pair { replace, .. } =
                        &mut action
                    {
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
                    error.to_string()
                },
            );
        }
    });
}

fn show_disable_confirmation(shell: &Rc<Preferences>) {
    let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        disable_confirmation: adw::AlertDialog,
    });
    let shell = shell.clone();
    let parent = shell
        .state
        .connect_dialog
        .upgrade()
        .map(|dialog| dialog.upcast::<gtk::Widget>())
        .unwrap_or_else(|| shell.window.clone().upcast());
    gtk::glib::spawn_future_local(async move {
        if disable_confirmation.choose_future(Some(&parent)).await == "disable" {
            run(&shell, Action::Enable { enabled: false });
        }
    });
}

pub fn bind_controller(
    shell: &Rc<Preferences>,
    popover: &gtk::Popover,
    builder: &gtk::Builder,
    resource: &str,
) {
    gtk_widgets::objects!(builder, resource, {
        connect_enabled: adw::SwitchRow, open_connect: gtk::Button,
        continue_connect: gtk::Button, continue_connect_label: gtk::Label,
    });
    connect_enabled.set_active(shell.products.connect.status().settings.enabled);
    let weak = Rc::downgrade(&shell);
    let popup = popover.downgrade();
    connect_enabled.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            let enabled = row.is_active();
            if shell.products.connect.status().settings.enabled != enabled {
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                }
                if enabled {
                    run(&shell, Action::Enable { enabled });
                    present(&shell);
                } else {
                    row.set_active(true);
                    show_disable_confirmation(&shell);
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
    let continue_button = continue_connect.downgrade();
    let continue_label = continue_connect_label.downgrade();
    popover.connect_map(move |_| {
        let mut updates = updates.clone();
        let row = row.clone();
        if let Some(row) = row.upgrade() {
            row.set_active(updates.borrow().settings.enabled);
        }
        let button = continue_button.clone();
        let offered = offered_device.clone();
        let label = continue_label.clone();
        if let Some(button) = button.upgrade() {
            button.set_visible(false);
        }
        *running.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            loop {
                let state = updates.borrow_and_update().clone();
                let device = state.continuation_device().cloned();
                let Some(row) = row.upgrade() else { return };
                row.set_active(state.settings.enabled);
                if let Some(button) = button.upgrade() {
                    button.set_visible(device.is_some());
                    if let Some(device) = &device {
                        let title = localization::tr_with(
                            "Continue from {device}",
                            &[("device", &device.name)],
                        );
                        if let Some(label) = label.upgrade() {
                            label.set_label(&title);
                        }
                        button.update_property(&[gtk::accessible::Property::Label(&title)]);
                    }
                }
                *offered.borrow_mut() = device;
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

fn present(shell: &Rc<Preferences>) {
    if let Some(dialog) = shell.state.connect_dialog.upgrade() {
        present_light_dismiss_dialog(&dialog, &shell.window);
        return;
    }
    let resource = crate::ui_resource::CONNECT_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        connect_dialog: adw::Dialog, page_title: adw::WindowTitle,
        choose_join: gtk::ToggleButton, choose_invite: gtk::ToggleButton,
        connection_choices: gtk::Box,
        join_method: adw::ComboRow, pairing: adw::PreferencesGroup,
        start_section: gtk::Box, join_section: gtk::Box, invite_section: adw::ActionRow, file_section: gtk::Box,
        verify_section: gtk::Box, connecting_section: gtk::Box, devices_section: gtk::Box,
        media_section: gtk::Box, network_section: gtk::Box, approval_status: gtk::Label,
        membership_status_text: gtk::Label, connecting_status: gtk::Label,
        collection_progress: gtk::ProgressBar, collection_spinner: gtk::Spinner,
        collection_counter: gtk::Label,
        feedback_label: gtk::Label, connection_error: adw::Banner,
        device_name: adw::EntryRow,
        devices: adw::PreferencesGroup, nearby_devices: adw::PreferencesGroup,
        no_nearby_devices: adw::ActionRow, verification: adw::PreferencesGroup,
        pair_name: gtk::Label, emoji: gtk::Label,
        peer: adw::EntryRow, nearby: adw::SwitchRow, relay: adw::EntryRow,
        relay_enabled: gtk::Switch,
        folders_section: gtk::Box,
        storage: adw::ComboRow,
        choose_connection: gtk::Button,
        file: adw::EntryRow,
        folders: adw::PreferencesGroup,
        create: gtk::Button, copy_invitation: gtk::Button, join: gtk::Button,
        approve: gtk::Button, reject: gtk::Button, refresh: gtk::Button, discover: gtk::Button,
        cancel_pairing: gtk::Button,
        import: gtk::Button, browse_file: gtk::Button,
        leave: gtk::Button, file_chooser: gtk::FileDialog,
        folder_chooser: gtk::FileDialog,
    });
    *shell.state.connect_feedback.borrow_mut() = Some(
        gtk_widgets::feedback::ControlFeedbackState::new(&feedback_label, shell.settings.clone()),
    );
    let weak = Rc::downgrade(shell);
    connect_dialog.connect_closed(move |_| {
        if let Some(shell) = weak.upgrade() {
            shell.state.connect_dialog.set(None::<&adw::Dialog>);
            shell.state.connect_feedback.borrow_mut().take();
        }
    });
    shell.state.connect_dialog.set(Some(&connect_dialog));
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
    let accounts = Rc::new(RefCell::new(shell.products.source.file_integrations()));
    let storage_draft = Rc::new(RefCell::new(StorageDraft::default()));
    let update_view: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(shell);
        let choose_join = choose_join.downgrade();
        let connection_choices = connection_choices.downgrade();
        let cancel_pairing = cancel_pairing.downgrade();
        let start_section = start_section.downgrade();
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
        let no_nearby_devices = no_nearby_devices.downgrade();
        let access_description = verification.description();
        let verification = verification.downgrade();
        let approve = approve.downgrade();
        let waiting_for_approval = approval_status.text();
        let connecting_devices = membership_status_text.text();
        let approval_status = approval_status.downgrade();
        let title = page_title.downgrade();
        let dialog = connect_dialog.downgrade();
        Rc::new(move || {
            let (Some(shell), Some(choice), Some(method)) =
                (weak.upgrade(), choose_join.upgrade(), join_method.upgrade())
            else {
                return;
            };
            let state = shell.products.connect.status();
            let working = state.connecting || state.collection_progress.is_some();
            let busy = state.pairing.is_some() || working;
            let progress_visible = working && state.pairing.is_none() && state.error.is_none();
            let has_profile = state.settings.profile.is_some();
            let joining = choice.is_active() && !has_profile;
            let importing = joining && method.selected() == 1;
            if let Some(dialog) = dialog.upgrade() {
                dialog.set_can_close(!state.connecting);
            }
            if let Some(title) = title.upgrade() {
                title.set_subtitle(if joining && !busy && state.error.is_none() {
                    &state.profile_status
                } else {
                    ""
                });
            }
            if let Some(choices) = connection_choices.upgrade() {
                choices.set_visible(!has_profile && !busy);
            }
            if let Some(button) = cancel_pairing.upgrade() {
                button.set_visible(state.connecting);
            }
            if let Some(invite) = invite_section.upgrade() {
                invite.set_visible(!joining && !busy);
            }
            let profile_controls = has_profile && !busy;
            for (section, visible) in [
                (&start_section, !busy),
                (&join_section, joining && !busy),
                (&file_section, importing && !busy),
                (&verify_section, state.pairing.is_some()),
                (&connecting_section, progress_visible),
                (&network_section, !busy && !importing),
                (&media_section, profile_controls),
                (
                    &devices_section,
                    !busy && state.devices.iter().any(|device| device.enrolled),
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
                group.set_visible(!busy && !importing && state.settings.nearby);
            }
            if let Some(row) = no_nearby_devices.upgrade() {
                row.set_visible(!state.devices.iter().any(|device| !device.enrolled));
            }
            if let Some(group) = verification.upgrade() {
                group.set_description(
                    if state
                        .pairing
                        .as_ref()
                        .is_some_and(|pairing| pairing.joining)
                    {
                        None
                    } else {
                        access_description.as_deref()
                    },
                );
            }
            if let Some(button) = leave.upgrade() {
                button.set_visible(
                    profile_controls && state.devices.iter().any(|device| device.enrolled),
                );
            }
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
        let file = file.downgrade();
        let update_view = update_view.clone();
        Rc::new(move || {
            if let Some(file) = file.upgrade() {
                file.set_text("");
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
                && (!shell.products.connect.status().settings.enabled
                    || shell.products.connect.status().settings.profile.is_none())
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
    let choose = choose_connection.downgrade();
    choose_connection.set_visible(storage.selected() > 0);
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
        let (Some(file), Some(shell), Some(parent)) =
            (file_weak.upgrade(), weak.upgrade(), parent.upgrade())
        else {
            return;
        };
        if selected == 0 {
            draft.borrow_mut().source = None;
            draft.borrow_mut().selected = 0;
            row.set_subtitle("");
            file.set_text("");
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
                file.set_text("");
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
    let parent = shell.window.downgrade();
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
                for root in owner
                    .roots(source.id.to_string())
                    .await
                    .map_err(|error| error.to_string())?
                {
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
                let builder = gtk_widgets::ui_resource::builder(resource);
                gtk_widgets::objects!(builder, resource, { folder_mapping: adw::EntryRow, browse_folder: gtk::Button });
                folder_mapping.set_title(&format!("{name} · {}", root.label));
                let key = format!("{source}/{}", root.id);
                let settings = shell.products.connect.status().settings;
                let path = settings
                    .folders
                    .get(&key)
                    .or_else(|| settings.folders.get(&source));
                folder_mapping.set_text(&path.map(ToString::to_string).unwrap_or_default());
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
                                path: downloads::DownloadDirectory::Native(
                                    row.text().as_str().into(),
                                ),
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
                                    path: downloads::DownloadDirectory::Native(path),
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
    let retry_label = connection_error.button_label();
    let weak = Rc::downgrade(shell);
    connection_error.connect_button_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            run(&shell, Action::Retry);
        }
    });
    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    leave.connect_clicked(move |_| {
        let (Some(shell), Some(parent)) = (weak.upgrade(), parent.upgrade()) else {
            return;
        };
        gtk::glib::spawn_future_local(async move {
            let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                gtk_widgets::ui_resource::object(&builder, resource, "disconnect_confirmation");
            if dialog.choose_future(Some(&parent)).await == "disconnect" {
                run(&shell, Action::Leave);
            }
        });
    });
    for (button, action) in [
        (create.clone(), Action::Enable { enabled: true }),
        (refresh, Action::Refresh),
        (discover, Action::Discover),
        (cancel_pairing, Action::CancelPairing),
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
                        replace: false,
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
    let parent = shell.window.downgrade();
    let entry = file.downgrade();
    let chooser = file_chooser.clone();
    browse_file.connect_clicked(move |_| {
        let (Some(parent), Some(entry)) = (parent.upgrade(), entry.upgrade()) else {
            return;
        };
        let chooser = chooser.clone();
        gtk::glib::spawn_future_local(async move {
            if let Ok(file) = chooser.open_future(Some(&parent)).await
                && let Some(path) = file.path()
            {
                entry.set_text(&path.to_string_lossy());
            }
        });
    });
    let weak = Rc::downgrade(shell);
    let parent = shell.window.downgrade();
    let entry = file.downgrade();
    let storage = storage.downgrade();
    let chooser = file_chooser.clone();
    import.connect_clicked(move |_| {
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
            show_feedback(&shell, localization::tr("Choose connection"));
            return;
        }
        let chooser = chooser.clone();
        gtk::glib::spawn_future_local(async move {
            let path = if remote || !entry.text().is_empty() {
                entry.text().as_str().into()
            } else {
                let Ok(file) = chooser.open_future(Some(&parent)).await else {
                    return;
                };
                let Some(path) = file.path() else { return };
                entry.set_text(&path.to_string_lossy());
                path
            };
            run(
                &shell,
                Action::Import {
                    path,
                    source_id,
                    replace: false,
                    key: None,
                },
            );
        });
    });

    let mut updates = shell.products.connect.subscribe();
    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    let device_rows = RefCell::new(Vec::new());
    let nearby_rows = RefCell::new(Vec::new());
    load_folders();
    let task = gtk::glib::spawn_future_local(async move {
        let mut previous_devices = None;
        let mut previous_profile = initial.settings.profile;
        let mut previous_working = initial.connecting;
        let mut completed_pairings = initial.completed_pairings;
        let mut previous_network = (
            initial.settings.nearby,
            initial.settings.relay,
            initial.settings.public_relay,
        );
        loop {
            let state = updates.borrow_and_update().clone();
            let (Some(shell), Some(parent)) = (weak.upgrade(), parent.upgrade()) else {
                return;
            };
            if state.completed_pairings != completed_pairings {
                parent.set_can_close(true);
                parent.close();
                show_connected(&shell, &localization::tr("Connected"));
                return;
            }
            completed_pairings = state.completed_pairings;
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
            connecting_status.set_text(&state.profile_status);
            let progress = state.collection_progress.as_ref();
            let fraction = progress.and_then(|progress| {
                progress
                    .total
                    .filter(|total| *total > 0)
                    .map(|total| (progress.completed as f64 / total as f64).clamp(0.0, 1.0))
            });
            collection_progress.set_visible(fraction.is_some());
            collection_progress.set_fraction(fraction.unwrap_or_default());
            collection_spinner.set_visible(fraction.is_none());
            let counter = progress.map(collection_progress_text).unwrap_or_default();
            collection_counter.set_visible(!counter.is_empty());
            collection_counter.set_text(&counter);
            connection_error.set_title(state.error.as_deref().unwrap_or_default());
            connection_error.set_revealed(state.error.is_some());
            connection_error.set_button_label(
                if (state.settings.profile.is_some() || state.settings.pending_import.is_some())
                    && !state.connecting
                {
                    retry_label.as_deref()
                } else {
                    None
                },
            );
            create.set_visible(
                (!state.settings.enabled || state.settings.profile.is_none())
                    && !state.connecting
                    && state.pairing.is_none(),
            );
            copy_invitation
                .set_visible(state.invitation.is_some() && state.settings.profile.is_some());
            if let Some(pairing) = &state.pairing {
                pair_name.set_text(&pairing.name);
                emoji.set_text(&pairing.emoji.join(" "));
            }
            let working = state.connecting;
            if previous_profile != state.settings.profile || (previous_working && !working) {
                previous_profile = state.settings.profile.clone();
                load_folders();
            }
            previous_working = working;
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
    let state = shell.products.connect.status();
    if state.settings.enabled
        && state.settings.profile.is_none()
        && state.pairing.is_none()
        && !state.connecting
    {
        run(shell, Action::Enable { enabled: true });
    }
    present_light_dismiss_dialog(&connect_dialog, &shell.window);
}

fn collection_progress_text(progress: &CollectionProgress) -> String {
    let bytes = matches!(
        progress.stage,
        CollectionStage::Receiving | CollectionStage::Validating
    );
    let count = |count: u64| {
        if bytes {
            format!("{:.1}", count as f64 / 1_048_576.0)
        } else {
            count.to_string()
        }
    };
    let current = count(progress.completed);
    if let Some(total) = progress.total.filter(|total| *total > 0) {
        let percent =
            (u128::from(progress.completed.min(total)) * 100 / u128::from(total)).to_string();
        let total = count(total);
        if bytes {
            localization::tr_with(
                "{current} / {total} MiB ({percent}%)",
                &[
                    ("current", &current),
                    ("total", &total),
                    ("percent", &percent),
                ],
            )
        } else {
            format!("{current} / {total} ({percent}%)")
        }
    } else if progress.completed == 0 {
        String::new()
    } else if bytes {
        localization::tr_with("{current} MiB", &[("current", &current)])
    } else {
        current
    }
}

#[derive(Default)]
struct StorageDraft {
    source: Option<sources::SourceId>,
    selected: u32,
    updating: bool,
}

fn join_profile(shell: &Rc<Preferences>, parent: &adw::Dialog, invitation: String) {
    let shell = shell.clone();
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        let dialog: adw::AlertDialog =
            gtk_widgets::ui_resource::object(&builder, resource, "replace_confirmation");
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
    shell: &Rc<Preferences>,
    parent: &adw::Dialog,
    group: &adw::PreferencesGroup,
    nearby: &adw::PreferencesGroup,
    state: &Status,
    rows: &RefCell<Vec<adw::ActionRow>>,
    nearby_rows: &RefCell<Vec<adw::ActionRow>>,
) {
    for device in &state.devices {
        let resource = crate::ui_resource::CONNECT_COMPONENTS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder,resource,{device_row:adw::ActionRow,play:gtk::Button,pause:gtk::Button,remove:gtk::Button,join_device:gtk::Button,invite_device:gtk::Button,test_connection:gtk::Button});
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
        invite_device.set_visible(!device.enrolled);
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
                invite_device,
                Action::Invite {
                    peer: device.id.clone(),
                },
            ),
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
