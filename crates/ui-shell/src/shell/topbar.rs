use std::rc::Rc;

use adw::prelude::*;

use super::{Shell, layout::ResolvedLeftSidebarMode};

pub(crate) struct Topbar {
    pub window_content: gtk::Box,
    pub content_host: adw::Bin,
    pub header: gtk::HeaderBar,
    pub search: gtk::SearchEntry,
    pub search_session: Rc<ui_library::SearchSession>,
    search_popup: std::cell::RefCell<Option<Rc<super::topbar_search::SearchPopup>>>,
    search_host: gtk::Overlay,
    search_shortcut: adw::ShortcutLabel,
    pub menu: gtk::MenuButton,
    sidebar: gtk::Button,
    source: gtk::MenuButton,
    controller: gtk::MenuButton,
    pub casting_host: gtk::Box,
}

impl Topbar {
    pub fn contains_search_focus(&self, focus: &gtk::Widget) -> bool {
        focus.is_ancestor(&self.search_host)
    }

    pub fn focus_search(&self) {
        self.search.grab_focus();
        if let Some(popup) = self.search_popup.borrow().as_ref() {
            popup.open();
        }
    }

    pub fn allocate_search_popup(&self) {
        if let Some(popup) = self.search_popup.borrow().as_ref() {
            popup.present();
        }
    }

    pub fn refresh_search_playback(
        &self,
        current: Option<&ui_shared::mounted_route::RouteCurrentTrack>,
    ) {
        if let Some(popup) = self.search_popup.borrow().as_ref() {
            popup.refresh_playback(current);
        }
    }

    pub fn new() -> Self {
        let resource = crate::ui_resource::TOPBAR_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            window_content: gtk::Box,
            content_host: adw::Bin,
            header: gtk::HeaderBar,
            search: gtk::SearchEntry,
            search_host: gtk::Overlay,
            search_shortcut: adw::ShortcutLabel,
            menu: gtk::MenuButton,
            sidebar: gtk::Button,
            source: gtk::MenuButton,
            controller: gtk::MenuButton,
            casting_host: gtk::Box,
        });
        ui_shared::controls::configure_search_entry(&search);
        Self {
            window_content,
            content_host,
            header,
            search,
            search_session: Rc::new(ui_library::SearchSession::default()),
            search_popup: std::cell::RefCell::new(None),
            search_host,
            search_shortcut,
            menu,
            sidebar,
            source,
            controller,
            casting_host,
        }
    }

    pub fn bind(&self, shell: &Rc<Shell>) {
        self.search_shortcut
            .set_accelerator(if cfg!(target_os = "macos") {
                "<Meta>k"
            } else {
                "<Control>k"
            });
        self.search_popup
            .replace(Some(super::topbar_search::SearchPopup::new(
                shell,
                &self.search_host,
            )));
        super::navigation::install_primary_menu(&self.menu, shell);
        crate::preferences::controller::bind_popover(shell, &self.controller);
        let weak = Rc::downgrade(shell);
        self.source.set_create_popup_func(move |button| {
            if let Some(shell) = weak.upgrade() {
                let (_, _, model) = crate::preferences::source::selector::source_submenu(&shell);
                let popover = gtk::PopoverMenu::from_model(Some(&model));
                button.set_popover(Some(&popover));
                ui_shared::interactions::show_native_menu_icons(&popover);
                ui_shared::interactions::replace_native_menu_checkmarks(&popover);
            }
        });

        let weak = Rc::downgrade(shell);
        self.sidebar.connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                if shell.left_sidebar_mode() == ResolvedLeftSidebarMode::Hidden {
                    let split = &shell.navigation_view.split_view;
                    split.set_show_sidebar(!split.shows_sidebar());
                } else {
                    gtk::prelude::ActionGroupExt::activate_action(
                        &shell.chrome.window,
                        "toggle-left-sidebar",
                        None,
                    );
                }
            }
        });
    }

    pub fn update_sidebar(&self, mode: ResolvedLeftSidebarMode) {
        let label = match mode {
            ResolvedLeftSidebarMode::Full => "Collapse sidebar",
            ResolvedLeftSidebarMode::Compact => "Expand sidebar",
            ResolvedLeftSidebarMode::Hidden => "Show sidebar",
        };
        let label = localization::tr(label);
        self.sidebar.set_tooltip_text(Some(&label));
        self.sidebar
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }
}
