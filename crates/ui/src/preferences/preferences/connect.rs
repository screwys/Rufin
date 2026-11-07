use crate::shell::shell::Shell;
use crate::widgets::popup::present_light_dismiss_dialog;
use adw::prelude::*;
use rufin_core::connect::{
    Action, ActionError, CollectionProgress, Confirmation, Continuation, Control, Status,
    SyncFolder,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub fn install(shell: &Rc<Shell>) {
    let mut updates = shell.products.connect.subscribe();
    let weak = Rc::downgrade(shell);
    let pairing = gtk::glib::spawn_future_local(async move {
        let mut previous = None;
        let mut working = updates.borrow().connecting;
        let mut offered = None;
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
            // A device's next track or pause is no new offer.
            let offer = state
                .continuation
                .as_ref()
                .map(|continuation| continuation.device.clone());
            if let Some(continuation) = &state.continuation
                && offer != offered
            {
                let Some(shell) = weak.upgrade() else { return };
                offer_continuation(&shell, continuation);
            }
            offered = offer;
            if updates.changed().await.is_err() {
                return;
            }
        }
    });
    shell
        .chrome
        .window
        .connect_destroy(move |_| pairing.abort());
}

fn offer_continuation(shell: &Rc<Shell>, continuation: &Continuation) {
    let toast = adw::Toast::builder()
        .title(localization::tr_with(
            "Continue from {device}",
            &[("device", &continuation.name)],
        ))
        .button_label(localization::tr("Continue"))
        .build();
    let weak = Rc::downgrade(shell);
    let peer = continuation.device.clone();
    toast.connect_button_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            run(&shell, Action::Continue { peer: peer.clone() });
        }
    });
    shell.chrome.toast_overlay.add_toast(toast);
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

fn run(shell: &Rc<Shell>, action: Action) {
    run_with_feedback(shell, action, None);
}

fn run_with_feedback(shell: &Rc<Shell>, action: Action, button: Option<&gtk::Button>) {
    let button = button.map(|button| button.downgrade());
    let testing_connection = matches!(action, Action::TestConnection { .. });
    let pending_replace = matches!(
        action,
        Action::Pair {
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
                let resource = "preferences/connect-components.ui";
                let builder = crate::ui_resource::builder(resource);
                let dialog: adw::AlertDialog =
                    crate::ui_resource::object(&builder, resource, "replace_confirmation");
                let parent = shell
                    .preferences
                    .connect_dialog
                    .upgrade()
                    .map(|dialog| dialog.upcast::<gtk::Widget>())
                    .unwrap_or_else(|| shell.chrome.window.clone().upcast());
                if dialog.choose_future(Some(&parent)).await == "join" {
                    if let Action::Pair { replace, .. } = &mut action {
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

fn show_disable_confirmation(shell: &Rc<Shell>) {
    let resource = "preferences/connect-components.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        disable_confirmation: adw::AlertDialog,
    });
    let shell = shell.clone();
    let parent = shell
        .preferences
        .connect_dialog
        .upgrade()
        .map(|dialog| dialog.upcast::<gtk::Widget>())
        .unwrap_or_else(|| shell.chrome.window.clone().upcast());
    gtk::glib::spawn_future_local(async move {
        if disable_confirmation.choose_future(Some(&parent)).await == "disable" {
            run(&shell, Action::Enable { enabled: false });
        }
    });
}

pub fn bind_controller(
    shell: &Rc<Shell>,
    popover: &gtk::Popover,
    builder: &gtk::Builder,
    resource: &str,
) {
    crate::objects!(builder, resource, {
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
    let offered_continuation = Rc::new(RefCell::new(None::<Continuation>));
    let offered = offered_continuation.clone();
    let weak = Rc::downgrade(shell);
    let popup = popover.downgrade();
    continue_connect.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade()
            && let Some(continuation) = offered.borrow().as_ref()
        {
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
            run(
                &shell,
                Action::Continue {
                    peer: continuation.device.clone(),
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
        let offered = offered_continuation.clone();
        let label = continue_label.clone();
        if let Some(button) = button.upgrade() {
            button.set_visible(false);
        }
        *running.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            loop {
                let state = updates.borrow_and_update().clone();
                let continuation = state.continuation.clone();
                let Some(row) = row.upgrade() else { return };
                row.set_active(state.settings.enabled);
                if let Some(button) = button.upgrade() {
                    button.set_visible(continuation.is_some());
                    if let Some(continuation) = &continuation {
                        let title = localization::tr_with(
                            "Continue from {device}",
                            &[("device", &continuation.name)],
                        );
                        if let Some(label) = label.upgrade() {
                            label.set_label(&title);
                        }
                        button.update_property(&[gtk::accessible::Property::Label(&title)]);
                    }
                }
                *offered.borrow_mut() = continuation;
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
    super::present_preferences_dialog_with_page(
        shell,
        super::PreferencesPageKind::Connect,
        false,
        false,
    );
}

pub(super) fn page(shell: &Rc<Shell>, connect_dialog: &adw::Dialog) -> gtk::Widget {
    let resource = "preferences/connect.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        connect_content: gtk::Overlay, page_title: gtk::Label,
        choose_join: gtk::ToggleButton, choose_invite: gtk::ToggleButton,
        connection_choices: gtk::Box,
        start_section: gtk::Box, join_section: gtk::Box, invite_section: adw::ActionRow,
        verify_section: gtk::Box, connecting_section: gtk::Box, devices_section: gtk::Box,
        network_section: gtk::Box, approval_status: gtk::Label,
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
        sync_folder: adw::PreferencesGroup, storage: adw::ComboRow,
        choose_connection: gtk::Button,
        folder: adw::EntryRow,
        create: gtk::Button, copy_invitation: gtk::Button, join: gtk::Button,
        approve: gtk::Button, reject: gtk::Button, refresh: gtk::Button, discover: gtk::Button,
        cancel_pairing: gtk::Button,
        browse_folder: gtk::Button, clear_folder: gtk::Button,
        leave: gtk::Button, folder_chooser: gtk::FileDialog,
    });
    *shell.preferences.connect_feedback.borrow_mut() =
        Some(crate::widgets::feedback::ControlFeedbackState::new(
            &feedback_label,
            shell.settings.clone(),
        ));
    shell.preferences.connect_dialog.set(Some(connect_dialog));
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
    show_folder(
        &storage,
        &folder,
        &storage_draft,
        &accounts.borrow(),
        initial.settings.folder.as_ref(),
    );
    let update_view: Rc<dyn Fn()> = {
        let weak = Rc::downgrade(shell);
        let choose_join = choose_join.downgrade();
        let connection_choices = connection_choices.downgrade();
        let cancel_pairing = cancel_pairing.downgrade();
        let start_section = start_section.downgrade();
        let join_section = join_section.downgrade();
        let invite_section = invite_section.downgrade();
        let verify_section = verify_section.downgrade();
        let connecting_section = connecting_section.downgrade();
        let devices_section = devices_section.downgrade();
        let network_section = network_section.downgrade();
        let sync_folder = sync_folder.downgrade();
        let leave = leave.downgrade();
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
            let (Some(shell), Some(choice)) = (weak.upgrade(), choose_join.upgrade()) else {
                return;
            };
            let state = shell.products.connect.status();
            let working = state.connecting || state.collection_progress.is_some();
            let busy = state.pairing.is_some() || working;
            let progress_visible = working && state.pairing.is_none() && state.error.is_none();
            let has_profile = state.settings.profile.is_some();
            let joining = choice.is_active() && !has_profile;
            if let Some(dialog) = dialog.upgrade() {
                dialog.set_can_close(!state.connecting);
            }
            if let Some(title) = title.upgrade() {
                let visible = joining && !busy && state.error.is_none();
                title.set_visible(visible);
                title.set_label(if visible { &state.profile_status } else { "" });
            }
            if let Some(choices) = connection_choices.upgrade() {
                choices.set_visible(!has_profile && !busy);
            }
            if let Some(button) = cancel_pairing.upgrade() {
                button.set_visible(state.connecting && !state.receiving_collection);
            }
            if let Some(invite) = invite_section.upgrade() {
                invite.set_visible(!joining && !busy);
            }
            let profile_controls = has_profile && !busy;
            for (section, visible) in [
                (&start_section, !busy),
                (&join_section, joining && !busy),
                (&verify_section, state.pairing.is_some()),
                (&connecting_section, progress_visible),
                (&network_section, !busy),
                (
                    &devices_section,
                    !busy && state.devices.iter().any(|device| device.enrolled),
                ),
            ] {
                if let Some(section) = section.upgrade() {
                    section.set_visible(visible);
                }
            }
            if let Some(group) = sync_folder.upgrade() {
                group.set_visible(profile_controls);
            }
            if let Some(group) = nearby_devices.upgrade() {
                group.set_visible(!busy && state.settings.nearby);
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
    let select = update_view.clone();
    let weak = Rc::downgrade(shell);
    choose_join.connect_toggled(move |button| {
        select();
        if button.is_active()
            && let Some(shell) = weak.upgrade()
        {
            run(&shell, Action::Discover);
        }
    });
    let select = update_view.clone();
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
    update_view();
    browse_folder.set_visible(storage.selected() == 0);
    let browse = browse_folder.downgrade();
    let folder_weak = folder.downgrade();
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
        let (Some(folder), Some(shell), Some(parent)) =
            (folder_weak.upgrade(), weak.upgrade(), parent.upgrade())
        else {
            return;
        };
        if selected == 0 {
            draft.borrow_mut().source = None;
            draft.borrow_mut().selected = 0;
            row.set_subtitle("");
            folder.set_text("");
            return;
        }
        let previous = draft.borrow().selected;
        draft.borrow_mut().updating = true;
        row.set_selected(previous);
        draft.borrow_mut().updating = false;
        let kind = if selected == 2 { "smb" } else { "webdav" };
        let draft = draft.clone();
        let accounts = storage_accounts.clone();
        let folder = folder.downgrade();
        let weak = Rc::downgrade(&shell);
        let row = row.downgrade();
        super::source::login::choose_integration(
            &shell,
            &parent,
            kind,
            Rc::new(move |id| {
                let (Some(shell), Some(folder), Some(row)) =
                    (weak.upgrade(), folder.upgrade(), row.upgrade())
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
                folder.set_text("");
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
            let resource = "preferences/connect-components.ui";
            let builder = crate::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                crate::ui_resource::object(&builder, resource, "disconnect_confirmation");
            if dialog.choose_future(Some(&parent)).await == "disconnect" {
                run(&shell, Action::Enable { enabled: false });
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
    let weak = Rc::downgrade(shell);
    let draft = storage_draft.clone();
    folder.connect_apply(move |row| {
        let Some(shell) = weak.upgrade() else { return };
        let path = row.text().trim().to_owned();
        let folder = match draft.borrow().source.clone() {
            _ if path.is_empty() => None,
            Some(connection) => Some(SyncFolder::Connection { connection, path }),
            None => Some(SyncFolder::Path { path: path.into() }),
        };
        run(&shell, Action::Folder { folder });
    });
    let parent = shell.chrome.window.downgrade();
    let entry = folder.downgrade();
    browse_folder.connect_clicked(move |_| {
        let (Some(parent), Some(entry)) = (parent.upgrade(), entry.upgrade()) else {
            return;
        };
        let chooser = folder_chooser.clone();
        gtk::glib::spawn_future_local(async move {
            if let Ok(file) = chooser.select_folder_future(Some(&parent)).await
                && let Some(path) = file.path()
            {
                entry.set_text(&path.to_string_lossy());
                entry.emit_by_name::<()>("apply", &[]);
            }
        });
    });
    let weak = Rc::downgrade(shell);
    clear_folder.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            run(&shell, Action::Folder { folder: None });
        }
    });

    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    let device_rows = Rc::new(RefCell::new(Vec::new()));
    let nearby_rows = Rc::new(RefCell::new(Vec::new()));
    let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let running = task.clone();
    connect_content.connect_map(move |_| {
        let Some(shell) = weak.upgrade() else { return };
        let mut updates = shell.products.connect.subscribe();
        let initial = shell.products.connect.status();
        let parent = parent.clone();
        let weak = weak.clone();
        let device_rows = device_rows.clone();
        let nearby_rows = nearby_rows.clone();
        let update_view = update_view.clone();
        let updating_network = updating_network.clone();
        let nearby = nearby.clone();
        let relay = relay.clone();
        let relay_enabled = relay_enabled.clone();
        let storage = storage.clone();
        let folder = folder.clone();
        let accounts = accounts.clone();
        let storage_draft = storage_draft.clone();
        let connecting_status = connecting_status.clone();
        let collection_progress = collection_progress.clone();
        let collection_spinner = collection_spinner.clone();
        let collection_counter = collection_counter.clone();
        let connection_error = connection_error.clone();
        let retry_label = retry_label.clone();
        let create = create.clone();
        let copy_invitation = copy_invitation.clone();
        let pair_name = pair_name.clone();
        let emoji = emoji.clone();
        let devices = devices.clone();
        let nearby_devices = nearby_devices.clone();
        *running.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            let mut previous_devices = None;
            let mut completed_pairings = initial.completed_pairings;
            let mut previous_network = (
                initial.settings.nearby,
                initial.settings.relay,
                initial.settings.public_relay,
            );
            let mut previous_folder = initial.settings.folder;
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
                if previous_folder != state.settings.folder {
                    show_folder(
                        &storage,
                        &folder,
                        &storage_draft,
                        &accounts.borrow(),
                        state.settings.folder.as_ref(),
                    );
                    previous_folder = state.settings.folder.clone();
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
                    if state.settings.profile.is_some() && !state.connecting {
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
        }));
    });
    connect_content.connect_unmap(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
    });
    let weak = Rc::downgrade(shell);
    let parent = connect_dialog.downgrade();
    connect_content.connect_destroy(move |_| {
        if let Some(parent) = parent.upgrade() {
            parent.set_can_close(true);
        }
        if let Some(shell) = weak.upgrade() {
            shell.preferences.connect_dialog.set(None::<&adw::Dialog>);
            shell.preferences.connect_feedback.borrow_mut().take();
        }
    });
    let state = shell.products.connect.status();
    if state.settings.enabled
        && state.settings.profile.is_none()
        && state.pairing.is_none()
        && !state.connecting
    {
        run(shell, Action::Enable { enabled: true });
    }
    connect_content.upcast()
}

fn collection_progress_text(progress: &CollectionProgress) -> String {
    let current = progress.completed;
    if let Some(total) = progress.total.filter(|total| *total > 0) {
        let percent = u128::from(current.min(total)) * 100 / u128::from(total);
        format!("{current} / {total} ({percent}%)")
    } else if current == 0 {
        String::new()
    } else {
        current.to_string()
    }
}

#[derive(Default)]
struct StorageDraft {
    source: Option<sources::SourceId>,
    selected: u32,
    updating: bool,
}

fn show_folder(
    storage: &adw::ComboRow,
    row: &adw::EntryRow,
    draft: &RefCell<StorageDraft>,
    accounts: &[rufin_core::source::FileIntegration],
    folder: Option<&SyncFolder>,
) {
    let (source, path) = match folder {
        Some(SyncFolder::Connection { connection, path }) => (Some(connection), path.clone()),
        Some(SyncFolder::Path { path }) => (None, path.to_string_lossy().into_owned()),
        Some(SyncFolder::Document { .. }) | None => (None, String::new()),
    };
    let account = source.and_then(|id| accounts.iter().find(|account| &account.id == id));
    let selected = match account.map(|account| account.kind.as_str()) {
        Some("smb") => 2,
        Some(_) => 1,
        None => 0,
    };
    {
        let mut draft = draft.borrow_mut();
        draft.source = source.cloned();
        draft.selected = selected;
        draft.updating = true;
    }
    storage.set_selected(selected);
    draft.borrow_mut().updating = false;
    storage.set_subtitle(account.map_or("", |account| account.name.as_str()));
    row.set_text(&path);
}

fn join_profile(shell: &Rc<Shell>, parent: &adw::Dialog, invitation: String) {
    let shell = shell.clone();
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let resource = "preferences/connect-components.ui";
        let builder = crate::ui_resource::builder(resource);
        let dialog: adw::AlertDialog =
            crate::ui_resource::object(&builder, resource, "replace_confirmation");
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
        let resource = "preferences/connect-components.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder,resource,{device_row:adw::ActionRow,play:gtk::Button,pause:gtk::Button,remove:gtk::Button,join_device:gtk::Button,invite_device:gtk::Button,test_connection:gtk::Button});
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
