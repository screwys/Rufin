use std::rc::Rc;

use adw::prelude::*;
use ui_shared::route::Route;

use super::{Shell, layout::ResolvedLeftSidebarMode};

pub(crate) struct Topbar {
    pub window_content: gtk::Box,
    pub content_host: adw::Bin,
    pub header: gtk::HeaderBar,
    pub search: gtk::SearchEntry,
    pub menu: gtk::MenuButton,
    sidebar: gtk::Button,
    source: gtk::MenuButton,
    pub playback_host: gtk::Box,
}

impl Topbar {
    pub fn new() -> Self {
        let resource = crate::ui_resource::TOPBAR_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            window_content: gtk::Box,
            content_host: adw::Bin,
            header: gtk::HeaderBar,
            search: gtk::SearchEntry,
            menu: gtk::MenuButton,
            sidebar: gtk::Button,
            source: gtk::MenuButton,
            playback_host: gtk::Box,
        });
        Self {
            window_content,
            content_host,
            header,
            search,
            menu,
            sidebar,
            source,
            playback_host,
        }
    }

    pub fn bind(&self, shell: &Rc<Shell>) {
        super::navigation::install_primary_menu(&self.menu, shell);
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
        let weak = Rc::downgrade(shell);
        self.search.connect_search_changed(move |entry| {
            if let Some(shell) = weak.upgrade() {
                let searching = shell.navigation.routes.borrow().current() == &Route::Search;
                if !searching && !entry.text().trim().is_empty() {
                    shell.navigate(Route::Search);
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
