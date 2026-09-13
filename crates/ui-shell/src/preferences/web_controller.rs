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
        web_enabled: adw::SwitchRow,
        web_allow_remote: adw::SwitchRow,
        web_port: adw::SpinRow,
        web_regenerate: gtk::Button,
        web_copy_token: gtk::Button,
        web_copy_token_icon: gtk::Image,
        web_copy_link: gtk::MenuButton,
        web_copy_link_icon: gtk::Image,
        web_status: adw::ActionRow,
        web_open: gtk::Button,
    });
    let config = shell.settings.persistence.load().web_controller;
    web_enabled.set_active(config.enabled);
    web_allow_remote.set_active(!config.address.is_loopback());
    web_port.set_value(f64::from(config.port));
    let access_token = Rc::new(RefCell::new(String::new()));
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
    web_enabled.connect_active_notify(move |row| {
        if let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) {
            save(&shell, &status, |config| config.enabled = row.is_active());
        }
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

    let weak = Rc::downgrade(shell);
    let status = web_status.downgrade();
    let copy_link = Rc::new(copy_action(web_copy_link.upcast_ref(), &web_copy_link_icon));
    web_copy_link.set_create_popup_func(move |button| {
        let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) else {
            button.set_active(false);
            return;
        };
        let state = shell.web_controller.status().borrow().clone();
        let addresses = match state.addresses() {
            Ok(addresses) => addresses,
            Err(error) => {
                status.set_subtitle(&error);
                button.set_active(false);
                return;
            }
        };
        if addresses.is_empty() {
            status.set_subtitle(&tr("No network address available"));
            button.set_active(false);
            return;
        }
        if let [address] = addresses.as_slice() {
            copy_link(&format!("http://{address}/#token={}", state.token));
            button.set_active(false);
            return;
        }
        let resource = crate::ui_resource::INTEGRATIONS_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        let popover: gtk::Popover =
            ui_shared::ui_resource::object(&builder, resource, "controller_addresses");
        let list: gtk::Box =
            ui_shared::ui_resource::object(&builder, resource, "controller_address_list");
        let cancel: gtk::Button =
            ui_shared::ui_resource::object(&builder, resource, "controller_address_cancel");
        for address in addresses {
            let choice = gtk::Button::with_label(&address.to_string());
            let link = format!("http://{address}/#token={}", state.token);
            let copy_link = copy_link.clone();
            let popover = popover.downgrade();
            choice.connect_clicked(move |_| {
                copy_link(&link);
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
            list.append(&choice);
        }
        let weak = popover.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(popover) = weak.upgrade() {
                popover.popdown();
            }
        });
        button.set_popover(Some(&popover));
        let weak = button.downgrade();
        popover.connect_closed(move |_| {
            if let Some(button) = weak.upgrade() {
                button.set_popover(None::<&gtk::Popover>);
            }
        });
    });
    let weak = Rc::downgrade(shell);
    let status = web_status.downgrade();
    web_open.connect_clicked(move |_| {
        let (Some(shell), Some(status)) = (weak.upgrade(), status.upgrade()) else {
            return;
        };
        let Some(link) = controller_link(&shell) else {
            return;
        };
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
    let token = access_token;
    let copy = web_copy_link.downgrade();
    let open = web_open.downgrade();
    page.connect_map(move |_| {
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
    page.connect_unmap(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
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

fn controller_link(shell: &Shell) -> Option<String> {
    let status = shell.web_controller.status();
    let state = status.borrow();
    let mut address = state.address?;
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Some(format!("http://{address}/#token={}", state.token))
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
        for (property, value) in [
            ("--window-bg-color", "var(--window-bg-color)"),
            ("--window-fg-color", "var(--window-fg-color)"),
            ("--card-bg-color", "var(--card-bg-color)"),
            ("--accent-color", "var(--accent-color)"),
            ("--bg", "var(--view-bg-color)"),
            ("--sidebar", "var(--sidebar-bg-color)"),
            ("--surface", "var(--card-bg-color)"),
            ("--player", "var(--headerbar-bg-color)"),
            ("--text", "var(--view-fg-color)"),
            ("--accent", "var(--accent-color)"),
            ("--blue", "var(--accent-bg-color)"),
            ("--favorite", "var(--error-color)"),
            ("--accent-foreground", "var(--accent-fg-color)"),
            ("--popover", "var(--popover-bg-color)"),
            ("--right-sidebar", "var(--secondary-sidebar-bg-color)"),
            ("--line", "var(--sidebar-border-color)"),
            ("--border-color", "var(--border-color)"),
            ("--hover", "var(--shade-color)"),
            (
                "--muted",
                "color-mix(in srgb, var(--view-fg-color) 60%, transparent)",
            ),
        ] {
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
