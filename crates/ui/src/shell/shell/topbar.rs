use std::rc::Rc;

use adw::prelude::*;

use super::{Shell, layout::ResolvedLeftSidebarMode};

pub(crate) struct Topbar {
    pub window_content: gtk::Box,
    pub content_host: adw::Bin,
    pub header: gtk::HeaderBar,
    /// The header, free to be narrower than its current buttons while the window shrinks.
    pub titlebar: super::layout::ShellAllocationOwner,
    pub search: gtk::SearchEntry,
    search_popup: std::cell::RefCell<Option<Rc<super::topbar_search::SearchPopup>>>,
    search_host: gtk::Overlay,
    search_shortcut: adw::ShortcutLabel,
    pub menu: gtk::MenuButton,
    sidebar: gtk::Button,
    navigation: gtk::Box,
    forward: gtk::Button,
    actions: gtk::Box,
    source: gtk::MenuButton,
    controller: gtk::MenuButton,
    playback_targets: gtk::Box,
    pub casting_host: gtk::Box,
    mini_title: adw::WindowTitle,
    mini: std::cell::Cell<bool>,
}

impl Topbar {
    pub fn contains_search_focus(&self, focus: &gtk::Widget) -> bool {
        focus.is_ancestor(&self.search_host)
    }

    pub fn focus_search(&self) {
        // The mini player has no search; the window grows back to pages for it.
        if self.mini.get() {
            return;
        }
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

    pub fn close_search_popup(&self) {
        if let Some(popup) = self.search_popup.borrow().as_ref() {
            popup.close();
        }
    }

    pub fn refresh_lyrics_cache_prompt(&self) {
        if let Some(popup) = self.search_popup.borrow().as_ref() {
            popup.refresh_cache_prompt();
        }
    }

    pub fn refresh_search_playback(
        &self,
        current: Option<&crate::widgets::mounted_route::RouteCurrentTrack>,
    ) {
        if let Some(popup) = self.search_popup.borrow().as_ref() {
            popup.refresh_playback(current);
        }
    }

    pub fn new() -> Self {
        let resource = "shell/topbar.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            window_content: gtk::Box,
            content_host: adw::Bin,
            header: gtk::HeaderBar,
            search: gtk::SearchEntry,
            search_host: gtk::Overlay,
            search_shortcut: adw::ShortcutLabel,
            menu: gtk::MenuButton,
            sidebar: gtk::Button,
            navigation: gtk::Box,
            forward: gtk::Button,
            actions: gtk::Box,
            source: gtk::MenuButton,
            controller: gtk::MenuButton,
            playback_targets: gtk::Box,
            casting_host: gtk::Box,
            mini_title: adw::WindowTitle,
        });
        crate::widgets::controls::configure_search_entry(&search);
        window_content.remove(&header);
        let titlebar = super::layout::ShellAllocationOwner::new(&header);
        titlebar.set_vexpand(false);
        titlebar.set_minimum_size(Some(0), None);
        window_content.prepend(&titlebar);
        Self {
            window_content,
            content_host,
            header,
            titlebar,
            search,
            search_popup: std::cell::RefCell::new(None),
            search_host,
            search_shortcut,
            menu,
            sidebar,
            navigation,
            forward,
            actions,
            source,
            controller,
            playback_targets,
            casting_host,
            mini_title,
            mini: std::cell::Cell::new(false),
        }
    }

    /// What the mini player shows in place of search.
    pub fn set_now_playing(&self, title: &str, artist: &str) {
        self.mini_title.set_title(title);
        self.mini_title.set_subtitle(artist);
    }

    /// A narrow window keeps navigation and search; the controller and output buttons move to the
    /// fullscreen player's bar. The mini player keeps only the window controls and what is playing.
    pub fn apply_window_size(&self, narrow: bool, mini: bool, playback_targets_host: &gtk::Box) {
        if narrow {
            self.header.add_css_class("narrow");
        } else {
            self.header.remove_css_class("narrow");
        }
        self.forward.set_visible(!narrow);
        let targets_host = if narrow || mini {
            playback_targets_host
        } else {
            &self.navigation
        };
        if self.playback_targets.parent().as_ref() != Some(targets_host.upcast_ref()) {
            if let Some(parent) = self.playback_targets.parent().and_downcast::<gtk::Box>() {
                parent.remove(&self.playback_targets);
            }
            targets_host.append(&self.playback_targets);
        }
        if self.mini.replace(mini) == mini {
            return;
        }
        self.navigation.set_visible(!mini);
        self.actions.set_visible(!mini);
        if mini {
            self.close_search_popup();
            self.header.set_title_widget(Some(&self.mini_title));
        } else {
            self.header.set_title_widget(Some(&self.search_host));
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
        crate::preferences::preferences::controller::bind_popover(&shell, &self.controller);
        let weak = Rc::downgrade(shell);
        self.source.set_create_popup_func(move |button| {
            if let Some(shell) = weak.upgrade() {
                let (_, _, model) =
                    crate::preferences::preferences::source::selector::source_submenu(&shell);
                let popover = gtk::PopoverMenu::from_model(Some(&model));
                popover.connect_show(crate::widgets::interactions::align_topbar_popup);
                button.set_popover(Some(&popover));
                crate::widgets::interactions::show_native_menu_icons(&popover);
                crate::widgets::interactions::replace_native_menu_checkmarks(&popover);
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
