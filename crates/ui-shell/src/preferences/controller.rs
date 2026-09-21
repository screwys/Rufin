use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::tr;
use rufin_core::api::ControllerSettings;

use crate::shell::Shell;

pub(super) fn bind(
    shell: &Rc<Shell>,
    page: &adw::PreferencesPage,
    builder: &gtk::Builder,
    resource: &str,
) {
    ui_shared::objects!(builder, resource, {
        web_allow_remote: adw::SwitchRow,
        web_port: adw::SpinRow,
        web_regenerate: gtk::Button,
        web_copy_token: gtk::Button,
        web_copy_token_icon: gtk::Image,
        web_status: adw::ActionRow,
    });
    let config = shell.settings.persistence.load().web_controller;
    web_allow_remote.set_active(!config.address.is_loopback());
    web_port.set_value(f64::from(config.port));
    let access_token = bind_controls(shell, page.upcast_ref(), builder, resource);
    let token = access_token.clone();
    let copy_token = copy_action(web_copy_token.upcast_ref(), &web_copy_token_icon);
    web_copy_token.connect_clicked(move |_| {
        copy_token(&token.borrow());
    });
    let weak = Rc::downgrade(shell);
    let token = access_token.clone();
    let status = web_status.downgrade();
    web_regenerate.connect_clicked(move |button| {
        let Some(shell) = weak.upgrade() else {
            return;
        };
        button.set_sensitive(false);
        let button = button.downgrade();
        let token = token.clone();
        let status = status.clone();
        gtk::glib::spawn_future_local(async move {
            let resource = crate::ui_resource::INTEGRATIONS_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            let dialog: adw::AlertDialog =
                ui_shared::ui_resource::object(&builder, resource, "controller_regenerate");
            if dialog.choose_future(Some(&shell.chrome.window)).await != "regenerate" {
                if let Some(button) = button.upgrade() {
                    button.set_sensitive(true);
                }
                return;
            }
            let result = shell
                .web_controller
                .regenerate_token()
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
            match result {
                Ok(value) => {
                    *token.borrow_mut() = value;
                }
                Err(error) => {
                    if let Some(status) = status.upgrade() {
                        status.set_subtitle(&error);
                    }
                }
            }
        });
    });

    let weak = Rc::downgrade(shell);
    let status = web_status.downgrade();
    web_allow_remote.connect_active_notify(move |row| {
        if let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) {
            save(&shell, &status, |config| {
                config.address = match (config.address.is_ipv6(), row.is_active()) {
                    (false, false) => std::net::Ipv4Addr::LOCALHOST.into(),
                    (false, true) => std::net::Ipv4Addr::UNSPECIFIED.into(),
                    (true, false) => std::net::Ipv6Addr::LOCALHOST.into(),
                    (true, true) => std::net::Ipv6Addr::UNSPECIFIED.into(),
                };
            });
        }
    });
    let weak = Rc::downgrade(shell);
    let status = web_status.downgrade();
    web_port.connect_value_notify(move |row| {
        if let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) {
            save(&shell, &status, |config| config.port = row.value() as u16);
        }
    });
}

pub(crate) fn bind_popover(shell: &Rc<Shell>, button: &gtk::MenuButton) {
    let weak = Rc::downgrade(shell);
    button.set_create_popup_func(move |button| {
        let Some(shell) = weak.upgrade() else {
            return;
        };
        let resource = crate::ui_resource::CONTROLLER_POPOVER_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        let popover: gtk::Popover = ui_shared::ui_resource::object(&builder, resource, "popover");
        let open_settings: gtk::Button =
            ui_shared::ui_resource::object(&builder, resource, "open_settings");
        let weak = Rc::downgrade(&shell);
        let popup = popover.downgrade();
        open_settings.connect_clicked(move |_| {
            if let Some(popover) = popup.upgrade() {
                popover.popdown();
            }
            if let Some(shell) = weak.upgrade() {
                super::present_preferences_dialog_with_page(
                    &shell,
                    super::PreferencesPageKind::Integrations,
                    false,
                    false,
                );
            }
        });
        bind_controls(&shell, popover.upcast_ref(), &builder, resource);
        super::connect::bind_controller(&shell, &popover, &builder, resource);
        button.set_popover(Some(&popover));
    });
}

fn bind_controls(
    shell: &Rc<Shell>,
    surface: &gtk::Widget,
    builder: &gtk::Builder,
    resource: &str,
) -> Rc<RefCell<String>> {
    ui_shared::objects!(builder, resource, {
        web_enabled: adw::SwitchRow,
        web_status: adw::ActionRow,
        web_copy_link: gtk::Button,
        web_copy_link_icon: gtk::Image,
        web_open: gtk::Button,
    });
    web_enabled.set_active(shell.settings.persistence.load().web_controller.enabled);
    web_status.set_visible(web_enabled.is_active());
    let access_token = Rc::new(RefCell::new(String::new()));
    let weak = Rc::downgrade(shell);
    let status = web_status.downgrade();
    web_enabled.connect_active_notify(move |row| {
        if let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) {
            save(&shell, &status, |config| config.enabled = row.is_active());
            status.set_visible(shell.settings.persistence.load().web_controller.enabled);
        }
    });
    bind_address_action(
        shell,
        &web_status,
        &web_copy_link,
        copy_action(web_copy_link.upcast_ref(), &web_copy_link_icon),
    );
    let weak = Rc::downgrade(shell);
    let status = web_status.downgrade();
    bind_address_action(shell, &web_status, &web_open, move |link| {
        let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) else {
            return;
        };
        let link = link.to_owned();
        gtk::glib::spawn_future_local(async move {
            if let Err(error) = gtk::UriLauncher::new(&link)
                .launch_future(Some(&shell.chrome.window))
                .await
            {
                status.set_subtitle(&error.to_string());
            }
        });
    });

    let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let running = task.clone();
    let updates = shell.web_controller.status();
    let settings = shell.settings.persistence.clone();
    let status = web_status.downgrade();
    let token = access_token.clone();
    let copy = web_copy_link.downgrade();
    let open = web_open.downgrade();
    surface.connect_map(move |_| {
        let (Some(status), Some(copy), Some(open)) =
            (status.upgrade(), copy.upgrade(), open.upgrade())
        else {
            return;
        };
        let mut updates = updates.clone();
        let settings = settings.clone();
        let token = token.clone();
        *running.borrow_mut() = Some(gtk::glib::spawn_future_local(async move {
            loop {
                let state = updates.borrow_and_update().clone();
                status.set_visible(settings.load().web_controller.enabled);
                if !state.token.is_empty() {
                    *token.borrow_mut() = state.token.clone();
                }
                copy.set_sensitive(!state.token.is_empty());
                open.set_sensitive(state.address.is_some());
                status.set_subtitle(&if let Some(error) = state.error {
                    error
                } else if let Some(address) = state.address {
                    format!("http://{address}/")
                } else if settings.load().web_controller.enabled {
                    tr("Starting...")
                } else {
                    tr("Disabled")
                });
                if updates.changed().await.is_err() {
                    break;
                }
            }
        }));
    });
    surface.connect_unmap(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
    });
    access_token
}

fn bind_address_action(
    shell: &Rc<Shell>,
    status: &adw::ActionRow,
    button: &gtk::Button,
    action: impl Fn(&str) + 'static,
) {
    let weak = Rc::downgrade(shell);
    let status = status.downgrade();
    let action = Rc::new(action);
    button.connect_clicked(move |button| {
        let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) else {
            return;
        };
        let state = shell.web_controller.status().borrow().clone();
        let addresses = match state.addresses() {
            Ok(addresses) => addresses,
            Err(error) => {
                status.set_subtitle(&error);
                return;
            }
        };
        if addresses.is_empty() {
            status.set_subtitle(&tr("No network address available"));
            return;
        }
        if let [address] = addresses.as_slice() {
            action(&format!("http://{address}/#token={}", state.token));
            return;
        }
        let resource = crate::ui_resource::CONTROLLER_POPOVER_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        let popover: gtk::Popover =
            ui_shared::ui_resource::object(&builder, resource, "addresses_popover");
        let list: gtk::Box = ui_shared::ui_resource::object(&builder, resource, "addresses");
        popover.set_parent(button);
        let dismiss = button.ancestor(gtk::Popover::static_type()).map(|parent| {
            // Keep the Controller popover's grab while its address chooser is open.
            popover.set_autohide(false);
            let click = gtk::GestureClick::new();
            click.set_button(0);
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            click.set_propagation_limit(gtk::PropagationLimit::SameNative);
            let chooser = popover.downgrade();
            click.connect_pressed(move |_, _, _, _| {
                if let Some(chooser) = chooser.upgrade() {
                    chooser.popdown();
                }
            });
            parent.add_controller(click.clone());
            (parent.downgrade(), click)
        });
        for address in addresses {
            let row = gtk::Button::with_label(&address.to_string());
            row.add_css_class("flat");
            let link = format!("http://{address}/#token={}", state.token);
            let action = action.clone();
            let weak_popover = popover.downgrade();
            row.connect_clicked(move |_| {
                action(&link);
                if let Some(popover) = weak_popover.upgrade() {
                    popover.popdown();
                }
            });
            list.append(&row);
        }
        let handler = RefCell::new(Some(ui_shared::interactions::popdown_on_anchor_unmap(
            button, &popover,
        )));
        let anchor = button.downgrade();
        popover.connect_closed(move |popover| {
            if let (Some(anchor), Some(handler)) = (anchor.upgrade(), handler.borrow_mut().take()) {
                anchor.disconnect(handler);
                if anchor.is_mapped() {
                    anchor.grab_focus();
                }
            }
            if let Some((parent, click)) = &dismiss
                && let Some(parent) = parent.upgrade()
            {
                parent.remove_controller(click);
            }
            // GTK still uses the parent chain after emitting closed.
            let popover = popover.clone();
            gtk::glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
        popover.child_focus(gtk::DirectionType::TabForward);
    });
}

fn copy_action(button: &gtk::Widget, icon: &gtk::Image) -> impl Fn(&str) + use<> {
    let reset = Rc::new(RefCell::new(None::<gtk::glib::SourceId>));
    let pending = reset.clone();
    let image = icon.downgrade();
    button.connect_unrealize(move |_| {
        if let Some(source) = pending.borrow_mut().take() {
            source.remove();
        }
        if let Some(image) = image.upgrade() {
            image.set_icon_name(Some("rufin-edit-copy-symbolic"));
        }
    });
    let button = button.downgrade();
    let icon = icon.downgrade();
    move |text| {
        let Some(button) = button.upgrade() else {
            return;
        };
        button.clipboard().set_text(text);
        if let Some(source) = reset.borrow_mut().take() {
            source.remove();
        }
        if let Some(icon) = icon.upgrade() {
            icon.set_icon_name(Some("rufin-object-select-symbolic"));
        }
        let icon = icon.clone();
        let pending = reset.clone();
        reset.replace(Some(gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(1500),
            move || {
                pending.borrow_mut().take();
                if let Some(icon) = icon.upgrade() {
                    icon.set_icon_name(Some("rufin-edit-copy-symbolic"));
                }
            },
        )));
    }
}

fn save(shell: &Shell, status: &adw::ActionRow, update: impl FnOnce(&mut ControllerSettings)) {
    let mut settings = shell.settings.persistence.load();
    update(&mut settings.web_controller);
    match shell.settings.persistence.save(&settings) {
        Ok(settings) => *shell.settings.current.borrow_mut() = settings,
        Err(error) => status.set_subtitle(&error),
    }
}

impl Shell {
    pub(crate) fn bind_controller_appearance(self: &Rc<Self>) {
        self.publish_controller_appearance();
        let weak = Rc::downgrade(self);
        adw::StyleManager::default().connect_notify_local(None, move |_, _| {
            if let Some(shell) = weak.upgrade() {
                shell.publish_controller_appearance();
            }
        });
        if let Some(settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(self);
            settings.connect_gtk_theme_name_notify(move |_| {
                if let Some(shell) = weak.upgrade() {
                    shell.publish_controller_appearance();
                }
            });
        }
    }

    #[allow(deprecated)]
    pub(crate) fn publish_controller_appearance(&self) {
        // Resolve CSS variables in the window's theme, rather than legacy named colors.
        let sample = gtk::Label::new(None);
        sample.set_visible(false);
        self.chrome.app_root_overlay.add_overlay(&sample);
        let provider = gtk::CssProvider::new();
        sample
            .style_context()
            .add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 2);
        let resolve = |value: &str| {
            provider.load_from_string(&format!("label {{ color: {value}; }}"));
            sample.color()
        };
        let mut colors = std::collections::BTreeMap::new();
        for &(property, value) in rufin_core::themes::CONTROLLER_COLORS {
            colors.insert(property.into(), resolve(value).to_string());
        }
        let background = resolve("var(--view-bg-color)");
        let dark =
            background.red() * 0.2126 + background.green() * 0.7152 + background.blue() * 0.0722
                < 0.5;
        self.chrome.app_root_overlay.remove_overlay(&sample);
        colors.insert(
            "color-scheme".into(),
            if dark { "dark" } else { "light" }.into(),
        );
        if let Some(font) = self.chrome.window.pango_context().font_description()
            && let Some(family) = font.family()
        {
            colors.insert("font-family".into(), format!("{family:?}, system-ui"));
            let size = f64::from(font.size()) / f64::from(gtk::pango::SCALE)
                * if font.is_size_absolute() {
                    1.0
                } else {
                    96.0 / 72.0
                };
            colors.insert("font-size".into(), format!("{size}px"));
        }
        self.products.appearance.send_if_modified(|current| {
            if *current == colors {
                return false;
            }
            *current = colors;
            true
        });
    }
}
