mod activity;
pub mod backup;
use crate::Preferences;
use crate::{
    MAX_NARROW_LAYOUT_THRESHOLD, MIN_NARROW_LAYOUT_THRESHOLD, SidebarRouteItem,
    SidebarRouteItemSettings,
};
use adw::prelude::*;
use gtk_widgets::layout::{LARGE_POPUP_BASE_HEIGHT, large_popup_content_height};
use gtk_widgets::popup::present_light_dismiss_dialog;
use localization::{language_option_index, language_options, tr};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub mod connect;
mod context_menu;
pub mod controller;
pub mod dialogs;
mod general;
mod layout;
mod library;
pub mod persistence;
mod privacy;
pub mod source;
mod storage;
mod themes;

use general::{
    ScrobblingCredentialDrafts, appearance_page, equalizer_page, playback_page, scrobbling_page,
};

use layout::{
    ReorderRowPresentation, left_sidebar_mode_from_index, left_sidebar_row, reorder_row,
    right_sidebar_mode_from_index, right_sidebar_row, visibility_position_subtitle,
};
pub use library::locate_local_folder;

fn show_discord_flatpak_help(row: &adw::SwitchRow) {
    if !row.is_active() || std::env::var_os("FLATPAK_ID").is_none() {
        return;
    }
    let resource = crate::ui_resource::DISCORD_FLATPAK_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::AlertDialog,
        user_command: gtk::Label,
        system_command: gtk::Label,
        copy_user: gtk::Button,
        copy_system: gtk::Button,
        copy_user_icon: gtk::Image,
        copy_system_icon: gtk::Image,
    });
    for (button, icon, command) in [
        (copy_user, copy_user_icon, user_command),
        (copy_system, copy_system_icon, system_command),
    ] {
        let copy = gtk_widgets::clipboard::copy_action(button.upcast_ref(), &icon);
        button.connect_clicked(move |_| {
            copy(&command.text());
        });
    }
    present_light_dismiss_dialog(&dialog, row);
}

const LASTFM_API_CREATE_URL: &str = "https://www.last.fm/api/account/create";
const LISTENBRAINZ_TOKEN_URL: &str = "https://listenbrainz.org/settings/";

pub struct PreferencesState {
    pub source_choice: RefCell<Option<sources::SourceId>>,
    pub connect_dialog: gtk::glib::WeakRef<adw::Dialog>,
    pub connect_feedback: RefCell<Option<Rc<gtk_widgets::feedback::ControlFeedbackState>>>,
    pub secret_storage_row: gtk::glib::WeakRef<adw::ComboRow>,
    pub dialog: gtk::glib::WeakRef<adw::Dialog>,
    pub release_history: RefCell<rufin_core::runtime::ReleaseHistory>,
    pub release_history_view: RefCell<Option<dialogs::release_notes::ReleaseHistoryView>>,
    pub release_check_source: RefCell<Option<gtk::glib::SourceId>>,
    pub preview_windows_updates: bool,
    pub release_notification_toast: RefCell<Option<adw::Toast>>,
    pub release_updating: RefCell<Option<String>>,
}

impl Drop for PreferencesState {
    fn drop(&mut self) {
        if let Some(source) = self.release_check_source.get_mut().take() {
            source.remove();
        }
    }
}

impl PreferencesState {
    pub fn active_dialog(&self) -> Option<adw::Dialog> {
        self.dialog.upgrade()
    }

    pub fn set_active_dialog(&self, dialog: &adw::Dialog) {
        self.dialog.set(Some(dialog));
    }
}

pub fn present_preferences_dialog(shell: &Rc<Preferences>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::General, false, false);
}
pub fn present_library_preferences_dialog(shell: &Rc<Preferences>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, false, false);
}
pub fn present_add_server_preferences_dialog(shell: &Rc<Preferences>) {
    if shell.source.configured.borrow().sources.is_empty() {
        shell.present_onboarding();
    } else {
        present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, true, false);
    }
}
pub fn present_downloads_preferences_dialog(shell: &Rc<Preferences>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::Downloads, false, true);
}

impl Preferences {
    pub fn set_external_metadata_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .settings
            .set_app_setting("metadata setting", enabled, |settings| {
                &mut settings.application.external_metadata_enabled
            })
            .is_some()
        {
            if enabled {
                self.retry_external_artwork("metadata setting");
            } else {
                (self.effects)(crate::Effect::MediaControlsChanged);
            }
        }
    }

    pub fn close_preferences_dialog(&self) {
        if let Some(dialog) = self.state.active_dialog() {
            dialog.close();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreferencesPageKind {
    General,
    Customization,
    Theme,
    Playback,
    Equalizer,
    Library,
    Downloads,
    Storage,
    Integrations,
    Controller,
    Connect,
    Privacy,
}

impl PreferencesPageKind {
    const ALL: [Self; 12] = [
        Self::General,
        Self::Customization,
        Self::Theme,
        Self::Integrations,
        Self::Playback,
        Self::Equalizer,
        Self::Library,
        Self::Downloads,
        Self::Connect,
        Self::Controller,
        Self::Storage,
        Self::Privacy,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Customization => "customization",
            Self::Theme => "theme",
            Self::Playback => "playback",
            Self::Equalizer => "equalizer",
            Self::Library => "library",
            Self::Downloads => "downloads",
            Self::Storage => "storage",
            Self::Integrations => "integrations",
            Self::Controller => "controller",
            Self::Connect => "connect",
            Self::Privacy => "privacy",
        }
    }

    fn title(self) -> String {
        match self {
            Self::General => tr("General"),
            Self::Customization => tr("Customization"),
            Self::Theme => tr("Theme"),
            Self::Playback => tr("Playback"),
            Self::Equalizer => tr("Equalizer"),
            Self::Library => tr("Library"),
            Self::Downloads => tr("Downloads"),
            Self::Storage => tr("Storage"),
            Self::Integrations => tr("Integrations"),
            Self::Controller => tr("Controller"),
            Self::Connect => tr("Connect"),
            Self::Privacy => tr("Privacy & Security"),
        }
    }

    fn icon_name(self) -> &'static str {
        match self {
            Self::General => "rufin-preferences-system-symbolic",
            Self::Customization => "rufin-sliders-2-symbolic",
            Self::Theme => "rufin-preferences-desktop-appearance-symbolic",
            Self::Playback => "rufin-music-queue-symbolic",
            Self::Equalizer => "rufin-sliders-vertical-symbolic",
            Self::Library => "rufin-library-symbolic",
            Self::Downloads => "rufin-download-symbolic",
            Self::Storage => "rufin-drive-harddisk-symbolic",
            Self::Integrations => "rufin-network-workgroup-symbolic",
            Self::Controller => "rufin-phonelink-symbolic",
            Self::Connect => "rufin-network-hotspot-symbolic",
            Self::Privacy => "rufin-system-lock-screen-symbolic",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Clone)]
struct PreferencesSearchItem {
    page: PreferencesPageKind,
    title: String,
    context: String,
    searchable_text: String,
    expander_title: Option<String>,
}

#[derive(Clone)]
pub struct PreferencesNavigationControls {
    back: gtk::Button,
    navigation: Rc<gtk::glib::WeakRef<adw::NavigationView>>,
}

impl PreferencesNavigationControls {
    fn new(back: gtk::Button) -> Self {
        back.update_property(&[gtk::accessible::Property::Label(&tr("Back"))]);
        set_preferences_back_visible(&back, false);

        let controls = Self {
            back,
            navigation: Rc::new(gtk::glib::WeakRef::new()),
        };
        let navigation = Rc::clone(&controls.navigation);
        controls.back.connect_clicked(move |button| {
            if let Some(navigation) = navigation.upgrade() {
                navigation.pop();
            }
            set_preferences_back_visible(button, false);
        });
        controls
    }

    pub fn set_navigation(&self, navigation: &adw::NavigationView) {
        self.navigation.set(Some(navigation));
    }

    pub fn set_nested_page_visible(&self, visible: bool) {
        set_preferences_back_visible(&self.back, visible);
    }

    fn return_to_root(&self) {
        if let Some(navigation) = self.navigation.upgrade() {
            while navigation.pop() {}
        }
        set_preferences_back_visible(&self.back, false);
    }
}

fn set_preferences_back_visible(button: &gtk::Button, visible: bool) {
    button.set_visible(visible);
    button.update_state(&[gtk::accessible::State::Hidden(!visible)]);
}

fn present_preferences_dialog_with_page(
    shell: &Rc<Preferences>,
    initial_page: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) {
    if !open_add_server {
        shell.clear_retained_add_server_form();
    }
    if let Some(dialog) = shell.state.active_dialog() {
        rebuild_preferences_dialog(
            shell,
            &dialog,
            initial_page,
            open_add_server,
            focus_download_queue,
        );
        present_light_dismiss_dialog(&dialog, &shell.window);
        return;
    }

    let dialog = adw::Dialog::builder()
        .title(tr("Preferences"))
        .content_width(900)
        .content_height(large_popup_content_height(
            shell.window.height(),
            LARGE_POPUP_BASE_HEIGHT,
        ))
        .build();
    dialog.add_css_class("preferences");
    shell.state.set_active_dialog(&dialog);
    let close_shell = Rc::downgrade(shell);
    dialog.connect_closed(move |_| {
        if let Some(shell) = close_shell.upgrade() {
            shell.release_inactive_add_server_form();
        }
    });
    rebuild_preferences_dialog(
        shell,
        &dialog,
        initial_page,
        open_add_server,
        focus_download_queue,
    );

    present_light_dismiss_dialog(&dialog, &shell.window);
}

fn rebuild_preferences_dialog(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    initial_page: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) {
    dialog.set_title(&tr("Preferences"));
    let credential_drafts = Rc::new(RefCell::new(shell.products.scrobbling.preferences()));
    let resource = crate::ui_resource::PREFERENCES_DIALOG_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        responsive: adw::BreakpointBin,
        split_view: adw::NavigationSplitView,
        categories: gtk::ListBox,
        content_page: adw::NavigationPage,
        page_header: adw::HeaderBar,
        page_title: gtk::Label,
        category_back: gtk::Button,
        stack: adw::ViewStack,
        back: gtk::Button,
        search_button: gtk::ToggleButton,
        search_entry: gtk::SearchEntry,
        header_content: gtk::Stack,
        search_results_group: adw::PreferencesGroup,
        content_stack: gtk::Stack,
    });
    gtk_widgets::controls::configure_search_entry(&search_entry);
    gtk_widgets::controls::configure_dialog_header(&page_header);
    let navigation_controls = PreferencesNavigationControls::new(back.clone());
    let update_header: Rc<dyn Fn()> = {
        let split = split_view.downgrade();
        let nested_back = back.downgrade();
        let category_back = category_back.downgrade();
        let title = page_title.downgrade();
        let content = content_page.downgrade();
        Rc::new(move || {
            let (Some(split), Some(nested_back), Some(category_back), Some(title), Some(content)) = (
                split.upgrade(),
                nested_back.upgrade(),
                category_back.upgrade(),
                title.upgrade(),
                content.upgrade(),
            ) else {
                return;
            };
            category_back.set_visible(
                split.is_collapsed() && split.shows_content() && !nested_back.is_visible(),
            );
            let page_title = if split.is_collapsed() && !split.shows_content() {
                split.sidebar().unwrap().title()
            } else {
                content.title()
            };
            title.set_label(&page_title);
        })
    };
    let update = update_header.clone();
    split_view.connect_collapsed_notify(move |_| update());
    let update = update_header.clone();
    split_view.connect_show_content_notify(move |_| update());
    let update = update_header.clone();
    content_page.connect_title_notify(move |_| update());
    back.connect_visible_notify(move |_| update_header());
    let category_split = split_view.downgrade();
    let category_search = search_button.downgrade();
    category_back.connect_clicked(move |_| {
        if let Some(split) = category_split.upgrade() {
            split.set_show_content(false);
        }
        if let Some(search) = category_search.upgrade() {
            search.set_active(false);
        }
    });
    dialog
        .bind_property("can-close", &category_back, "sensitive")
        .sync_create()
        .build();
    search_button.update_property(&[gtk::accessible::Property::Label(&tr("Search"))]);
    dialog
        .bind_property("can-close", &categories, "sensitive")
        .sync_create()
        .build();
    dialog
        .bind_property("can-close", &search_button, "sensitive")
        .sync_create()
        .build();
    dialog
        .bind_property("can-close", &content_page, "can-pop")
        .sync_create()
        .build();
    let breakpoint = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 760sp").expect("preferences breakpoint"),
    );
    breakpoint.add_setter(&split_view, "collapsed", Some(&true.to_value()));
    responsive.add_breakpoint(breakpoint);
    dialog.set_child(Some(&responsive));

    let page_slots = Rc::new(
        PreferencesPageKind::ALL
            .into_iter()
            .map(|kind| {
                let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
                slot.set_hexpand(true);
                slot.set_vexpand(true);
                let title = kind.title();
                stack.add_titled_with_icon(&slot, Some(kind.name()), &title, kind.icon_name());
                (kind, slot)
            })
            .collect::<Vec<_>>(),
    );
    mount_preferences_page(
        shell,
        dialog,
        &page_slots,
        &navigation_controls,
        &credential_drafts,
        initial_page,
        open_add_server,
        focus_download_queue,
    );
    stack.set_visible_child_name(initial_page.name());
    content_page.set_title(&initial_page.title());
    categories.select_row(
        categories
            .row_at_index(
                PreferencesPageKind::ALL
                    .iter()
                    .position(|kind| *kind == initial_page)
                    .unwrap() as i32,
            )
            .as_ref(),
    );
    split_view.set_show_content(true);
    let category_stack = stack.downgrade();
    let category_split = split_view.downgrade();
    let category_search = search_button.downgrade();
    categories.connect_row_activated(move |_, row| {
        let Some(kind) = PreferencesPageKind::ALL.get(row.index() as usize) else {
            return;
        };
        if let Some(stack) = category_stack.upgrade() {
            stack.set_visible_child_name(kind.name());
        }
        if let Some(search) = category_search.upgrade() {
            search.set_active(false);
        }
        if let Some(split) = category_split.upgrade() {
            split.set_show_content(true);
        }
    });
    let selected_categories = categories.downgrade();
    let selected_content = content_page.downgrade();
    let selected_split = split_view.downgrade();
    let page_shell = Rc::clone(shell);
    let page_dialog = dialog.downgrade();
    let page_slots_for_switch = Rc::clone(&page_slots);
    let navigation_controls_for_switch = navigation_controls.clone();
    let credential_drafts_for_switch = credential_drafts.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        let Some(page_dialog) = page_dialog.upgrade() else {
            return;
        };
        let Some(name) = stack.visible_child_name() else {
            return;
        };
        let Some(kind) = PreferencesPageKind::from_name(name.as_str()) else {
            return;
        };
        if let Some(content) = selected_content.upgrade() {
            content.set_title(&kind.title());
        }
        if let Some(categories) = selected_categories.upgrade() {
            categories.select_row(
                categories
                    .row_at_index(
                        PreferencesPageKind::ALL
                            .iter()
                            .position(|page| *page == kind)
                            .unwrap() as i32,
                    )
                    .as_ref(),
            );
        }
        if let Some(split) = selected_split.upgrade() {
            split.set_show_content(true);
        }
        mount_preferences_page(
            &page_shell,
            &page_dialog,
            &page_slots_for_switch,
            &navigation_controls_for_switch,
            &credential_drafts_for_switch,
            kind,
            false,
            false,
        );
    });

    let search_items = Rc::new(RefCell::new(Vec::<PreferencesSearchItem>::new()));
    let rendered_search_rows = Rc::new(RefCell::new(Vec::<gtk::Widget>::new()));
    let search_shell = Rc::clone(shell);
    let search_dialog = dialog.downgrade();
    let search_slots = Rc::clone(&page_slots);
    let search_drafts = credential_drafts.clone();
    let search_items_for_button = Rc::clone(&search_items);
    let search_entry_for_button = search_entry.downgrade();
    let header_for_search = header_content.downgrade();
    let content_for_search = content_stack.downgrade();
    let page_stack_for_button = stack.downgrade();
    search_button.connect_toggled(move |button| {
        let Some(search_entry) = search_entry_for_button.upgrade() else {
            return;
        };
        let enabled = button.is_active();
        if let Some(header) = header_for_search.upgrade() {
            header.set_visible_child_name(if enabled { "search" } else { "title" });
        }
        if !enabled {
            search_entry.set_text("");
            if let Some(content) = content_for_search.upgrade() {
                content.set_visible_child_name("pages");
            }
            return;
        }
        let Some(search_dialog) = search_dialog.upgrade() else {
            return;
        };
        let Some(page_stack) = page_stack_for_button.upgrade() else {
            return;
        };
        if search_items_for_button.borrow().is_empty() {
            let retained = page_stack
                .visible_child_name()
                .and_then(|name| PreferencesPageKind::from_name(name.as_str()))
                .unwrap_or(PreferencesPageKind::General);
            let items = build_preferences_search_index(
                &search_shell,
                &search_dialog,
                &search_slots,
                &search_drafts,
                retained,
            );
            *search_items_for_button.borrow_mut() = items;
        }
        search_entry.grab_focus();
    });

    let stop_search = search_button.downgrade();
    search_entry.connect_stop_search(move |_| {
        if let Some(button) = stop_search.upgrade() {
            button.set_active(false);
        }
    });

    let search_items_for_entry = Rc::clone(&search_items);
    let rendered_rows_for_entry = Rc::clone(&rendered_search_rows);
    let results_group_for_entry = search_results_group.clone();
    let page_stack_for_entry = stack.downgrade();
    let content_stack_for_entry = content_stack.downgrade();
    let search_button_for_entry = search_button.downgrade();
    let navigation_for_entry = navigation_controls.clone();
    search_entry.connect_search_changed(move |entry| {
        let Some(content_stack) = content_stack_for_entry.upgrade() else {
            return;
        };
        for row in rendered_rows_for_entry.borrow_mut().drain(..) {
            results_group_for_entry.remove(&row);
        }
        let query = entry.text();
        if query.trim().is_empty() {
            content_stack.set_visible_child_name("pages");
            return;
        }
        content_stack.set_visible_child_name("search");
        let mut matched = false;
        for item in search_items_for_entry
            .borrow()
            .iter()
            .filter(|item| preferences_search_matches(&item.searchable_text, query.as_str()))
        {
            matched = true;
            let row = adw::ActionRow::builder()
                .title(&item.title)
                .subtitle(&item.context)
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name(item.page.icon_name()));
            row.add_suffix(&gtk::Image::from_icon_name("rufin-go-next-symbolic"));
            let page = item.page;
            let item = item.clone();
            let target_slot = page_slots
                .iter()
                .find(|(kind, _)| *kind == page)
                .map(|(_, slot)| slot.downgrade());
            let page_stack = page_stack_for_entry.clone();
            let search_button = search_button_for_entry.clone();
            let navigation = navigation_for_entry.clone();
            row.connect_activated(move |_| {
                let Some(page_stack) = page_stack.upgrade() else {
                    return;
                };
                let Some(search_button) = search_button.upgrade() else {
                    return;
                };
                navigation.return_to_root();
                page_stack.set_visible_child_name(page.name());
                search_button.set_active(false);
                let item = item.clone();
                let target_slot = target_slot.clone();
                gtk::glib::idle_add_local_once(move || {
                    if let Some(slot) = target_slot.and_then(|slot| slot.upgrade()) {
                        focus_preferences_search_item(slot.upcast_ref(), &item);
                    }
                });
            });
            results_group_for_entry.add(&row);
            rendered_rows_for_entry
                .borrow_mut()
                .push(row.upcast::<gtk::Widget>());
        }
        if !matched {
            let row = adw::ActionRow::builder()
                .title(tr(r"No results ¯\_(°╭╮°)_/¯"))
                .build();
            results_group_for_entry.add(&row);
            rendered_rows_for_entry
                .borrow_mut()
                .push(row.upcast::<gtk::Widget>());
        }
    });
}

fn collect_preferences_search_items(
    widget: &gtk::Widget,
    page: PreferencesPageKind,
    group_title: &str,
    expander_title: Option<String>,
    items: &mut Vec<PreferencesSearchItem>,
) {
    let group_title = widget
        .clone()
        .downcast::<adw::PreferencesGroup>()
        .ok()
        .map(|group| group.title().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| group_title.to_owned());
    let (group_title, expander_title) = widget
        .clone()
        .downcast::<adw::ExpanderRow>()
        .ok()
        .map(|row| {
            let title = row.title().to_string();
            let title = if title.is_empty() {
                group_title.clone()
            } else {
                title
            };
            (title.clone(), Some(title))
        })
        .unwrap_or((group_title, expander_title));

    if let Ok(row) = widget.clone().downcast::<adw::PreferencesRow>() {
        let title = row.title().to_string();
        if !title.is_empty() {
            let page_title = page.title();
            let context = if group_title.is_empty() {
                page_title.clone()
            } else {
                format!("{page_title} · {group_title}")
            };
            let subtitle = row
                .clone()
                .downcast::<adw::ActionRow>()
                .ok()
                .and_then(|row| row.subtitle())
                .unwrap_or_default();
            items.push(PreferencesSearchItem {
                page,
                searchable_text: format!("{title} {subtitle} {context}").to_lowercase(),
                title,
                context,
                expander_title: expander_title.clone(),
            });
        }
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        collect_preferences_search_items(
            &current,
            page,
            &group_title,
            expander_title.clone(),
            items,
        );
    }
}

fn focus_preferences_search_item(root: &gtk::Widget, item: &PreferencesSearchItem) {
    focus_preferences_search_descendant(root, item, "");
}

fn focus_preferences_search_descendant(
    widget: &gtk::Widget,
    item: &PreferencesSearchItem,
    group_title: &str,
) -> bool {
    let group_title = widget
        .clone()
        .downcast::<adw::PreferencesGroup>()
        .ok()
        .map(|group| group.title().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| group_title.to_owned());
    let group_title = if let Ok(row) = widget.clone().downcast::<adw::ExpanderRow>() {
        let title = row.title().to_string();
        if item.expander_title.as_deref() == Some(title.as_str()) {
            row.set_expanded(true);
        }
        if title.is_empty() { group_title } else { title }
    } else {
        group_title
    };

    if let Ok(row) = widget.clone().downcast::<adw::PreferencesRow>() {
        let title = row.title().to_string();
        let context = if group_title.is_empty() {
            item.page.title()
        } else {
            format!("{} · {group_title}", item.page.title())
        };
        if title == item.title && context == item.context {
            return widget.grab_focus();
        }
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        if focus_preferences_search_descendant(&current, item, &group_title) {
            return true;
        }
    }
    false
}

fn materialize_preferences_expanders(widget: &gtk::Widget) {
    if let Ok(expander) = widget.clone().downcast::<adw::ExpanderRow>() {
        let expanded = expander.is_expanded();
        if !expanded {
            expander.set_expanded(true);
            expander.set_expanded(false);
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        materialize_preferences_expanders(&current);
        child = current.next_sibling();
    }
}

fn build_preferences_search_index(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    page_slots: &[(PreferencesPageKind, gtk::Box)],
    credential_drafts: &ScrobblingCredentialDrafts,
    retained: PreferencesPageKind,
) -> Vec<PreferencesSearchItem> {
    let mut items = Vec::new();
    for kind in PreferencesPageKind::ALL {
        let page = if kind == retained {
            page_slots
                .iter()
                .find(|(page, _)| *page == kind)
                .and_then(|(_, slot)| slot.first_child())
                .unwrap()
        } else if matches!(
            kind,
            PreferencesPageKind::Connect | PreferencesPageKind::Storage
        ) {
            let (resource, id) = if kind == PreferencesPageKind::Connect {
                (crate::ui_resource::CONNECT_RESOURCE, "connect_content")
            } else {
                (
                    crate::ui_resource::STORAGE_PREFERENCES_RESOURCE,
                    "storage_page",
                )
            };
            let builder = gtk_widgets::ui_resource::builder(resource);
            let page: gtk::Widget = gtk_widgets::ui_resource::object(&builder, resource, id);
            if kind == PreferencesPageKind::Storage {
                let storage = page.clone().downcast::<adw::PreferencesPage>().unwrap();
                let backup_resource = crate::ui_resource::BACKUP_PREFERENCES_RESOURCE;
                let backup_builder = gtk_widgets::ui_resource::builder(backup_resource);
                let backups: adw::PreferencesGroup = gtk_widgets::ui_resource::object(
                    &backup_builder,
                    backup_resource,
                    "backup_group",
                );
                let reset: adw::PreferencesGroup =
                    gtk_widgets::ui_resource::object(&builder, resource, "reset_group");
                storage.add(&backups);
                storage.add(&reset);
            }
            page
        } else {
            let navigation = PreferencesNavigationControls::new(gtk::Button::new());
            build_preferences_page(
                shell,
                dialog,
                &navigation,
                credential_drafts,
                kind,
                false,
                false,
            )
        };
        materialize_preferences_expanders(&page);
        collect_preferences_search_items(&page, kind, "", None, &mut items);
    }
    items
}

pub(super) fn populate_expander_once(
    expander: &adw::ExpanderRow,
    populate: impl Fn(&adw::ExpanderRow) + 'static,
) {
    let populated = Rc::new(Cell::new(false));
    let populate: Rc<dyn Fn(&adw::ExpanderRow)> = Rc::new(populate);
    let populated_for_expand = Rc::clone(&populated);
    let populate_for_expand = Rc::clone(&populate);
    expander.connect_expanded_notify(move |expander| {
        if expander.is_expanded() && !populated_for_expand.replace(true) {
            populate_for_expand(expander);
        }
    });
    if expander.is_expanded() && !populated.replace(true) {
        populate(expander);
    }
}

fn preferences_search_matches(searchable_text: &str, query: &str) -> bool {
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .all(|term| searchable_text.contains(&term))
}

#[cfg(test)]
mod search_tests {
    use super::preferences_search_matches;

    #[test]
    fn preference_search_matches_terms_across_row_context() {
        let searchable = "show tray icon app window general";

        assert!(preferences_search_matches(searchable, "tray general"));
        assert!(preferences_search_matches(searchable, "GENERAL ICON"));
        assert!(!preferences_search_matches(searchable, "tray playback"));
    }
}

fn mount_preferences_page(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    page_slots: &[(PreferencesPageKind, gtk::Box)],
    navigation_controls: &PreferencesNavigationControls,
    credential_drafts: &ScrobblingCredentialDrafts,
    kind: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) {
    for (slot_kind, slot) in page_slots {
        if *slot_kind == kind || slot.first_child().is_none() {
            continue;
        }
        if matches!(
            slot_kind,
            PreferencesPageKind::Library
                | PreferencesPageKind::General
                | PreferencesPageKind::Storage
        ) {
            navigation_controls.return_to_root();
        }
        while let Some(page) = slot.first_child() {
            slot.remove(&page);
        }
    }
    let Some((_, slot)) = page_slots.iter().find(|(slot_kind, _)| *slot_kind == kind) else {
        return;
    };
    if slot.first_child().is_some() {
        return;
    }
    slot.append(&build_preferences_page(
        shell,
        dialog,
        navigation_controls,
        credential_drafts,
        kind,
        open_add_server,
        focus_download_queue,
    ));
}

fn build_preferences_page(
    shell: &Rc<Preferences>,
    dialog: &adw::Dialog,
    navigation_controls: &PreferencesNavigationControls,
    credential_drafts: &ScrobblingCredentialDrafts,
    kind: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) -> gtk::Widget {
    match kind {
        PreferencesPageKind::General => general_page(shell, navigation_controls),
        PreferencesPageKind::Customization => appearance_page(shell).upcast(),
        PreferencesPageKind::Theme => themes::page(shell).upcast(),
        PreferencesPageKind::Privacy => privacy::page(shell, dialog).upcast(),
        PreferencesPageKind::Equalizer => equalizer_page(shell).upcast(),
        PreferencesPageKind::Downloads => {
            library::downloads_page(shell, dialog, focus_download_queue).upcast()
        }
        PreferencesPageKind::Storage => storage::page(shell, navigation_controls),
        PreferencesPageKind::Controller => controller::page(shell).upcast(),
        PreferencesPageKind::Connect => connect::page(shell, dialog),
        PreferencesPageKind::Integrations => scrobbling_page(shell, credential_drafts).upcast(),
        PreferencesPageKind::Playback => playback_page(shell).upcast(),
        PreferencesPageKind::Library => {
            library::library_page(shell, dialog, navigation_controls, open_add_server)
        }
    }
}
fn general_page(
    shell: &Rc<Preferences>,
    navigation_controls: &PreferencesNavigationControls,
) -> gtk::Widget {
    let resource = crate::ui_resource::GENERAL_PREFERENCES_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        page: adw::PreferencesPage,
        prefer_distinct_track_covers_row: adw::SwitchRow,
        external_metadata_row: adw::SwitchRow,
        external_lyrics_row: adw::SwitchRow,
        language_row: adw::ComboRow,
        starting_page_row: adw::ComboRow,
        window_group: adw::PreferencesGroup,
        activity_overview_row: adw::ActionRow,
        tray_row: adw::SwitchRow,
        keep_running_row: adw::SwitchRow,
        start_minimized_row: adw::SwitchRow,
        type_to_search_row: adw::SwitchRow,
        automatic_updates_row: adw::SwitchRow,
        release_check_interval_row: adw::ComboRow,
        control_notifications_row: adw::SwitchRow,
        notifications_row: adw::SwitchRow,
        release_notifications_row: adw::SwitchRow,
    });
    let settings = shell.settings.current.borrow().clone();
    prefer_distinct_track_covers_row.set_active(settings.application.prefer_distinct_track_covers);
    let distinct_track_covers_shell = Rc::clone(shell);
    prefer_distinct_track_covers_row.connect_active_notify(move |row| {
        if distinct_track_covers_shell
            .settings
            .set_app_setting("track cover setting", row.is_active(), |settings| {
                &mut settings.application.prefer_distinct_track_covers
            })
            .is_some()
        {
            if let Some(selected) = distinct_track_covers_shell
                .products
                .source
                .selected_library()
            {
                let source = distinct_track_covers_shell.products.source.clone();
                gtk::glib::spawn_future_local(async move {
                    let _ = source.refresh_source(selected.source_id, false).await;
                });
            }
        }
    });

    external_metadata_row.set_active(settings.application.external_metadata_enabled);
    let metadata_shell = Rc::clone(shell);
    external_metadata_row.connect_active_notify(move |row| {
        metadata_shell.set_external_metadata_enabled(row.is_active());
    });

    external_lyrics_row.set_active(settings.lyrics.preferences.external_lyrics_enabled);
    let weak = Rc::downgrade(shell);
    external_lyrics_row.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.player_ui.set_external_lyrics_enabled(row.is_active());
        }
    });

    let starting_pages = SidebarRouteItem::all();
    let starting_page_titles = starting_pages
        .iter()
        .map(|item| tr(gtk_widgets::route::sidebar_route_descriptor(*item).title))
        .collect::<Vec<_>>();
    starting_page_row.set_model(Some(&gtk::StringList::new(
        &starting_page_titles
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )));
    starting_page_row.set_selected(
        starting_pages
            .iter()
            .position(|item| *item == settings.application.starting_page)
            .unwrap() as u32,
    );
    let starting_page_shell = Rc::downgrade(shell);
    starting_page_row.connect_selected_notify(move |row| {
        let Some(shell) = starting_page_shell.upgrade() else {
            return;
        };
        let Some(&page) = starting_pages.get(row.selected() as usize) else {
            return;
        };
        shell
            .settings
            .set_app_setting("starting page", page, |settings| {
                &mut settings.application.starting_page
            });
    });
    let language_options = Rc::new(language_options());
    let language_titles = language_options
        .iter()
        .map(|option| option.title.as_str())
        .collect::<Vec<_>>();
    let language_model = gtk::StringList::new(&language_titles);
    language_row.set_model(Some(&language_model));
    language_row.set_selected(language_option_index(
        language_options.as_ref(),
        &settings.appearance.language,
    ));
    let language_shell = Rc::clone(shell);
    language_row.connect_selected_notify(move |row| {
        let Some(option) = language_options.get(row.selected() as usize) else {
            return;
        };
        language_shell.settings.set_app_setting(
            "language setting",
            option.id.clone(),
            |settings| &mut settings.appearance.language,
        );
    });

    {
        #[cfg(not(target_os = "macos"))]
        tray_row.set_active(settings.tray_enabled);
        #[cfg(not(target_os = "macos"))]
        tray_row.set_sensitive(!settings.keep_running_after_close);
        #[cfg(not(target_os = "macos"))]
        keep_running_row.set_active(settings.keep_running_after_close);
        #[cfg(not(target_os = "macos"))]
        start_minimized_row.set_active(settings.tray_enabled && settings.start_minimized);
        type_to_search_row.set_active(settings.type_to_search_enabled);
        #[cfg(target_os = "macos")]
        for row in [&tray_row, &keep_running_row, &start_minimized_row] {
            window_group.remove(row);
        }
        #[cfg(not(target_os = "macos"))]
        start_minimized_row.set_visible(settings.tray_enabled);
        #[cfg(not(target_os = "macos"))]
        let tray_shell = Rc::clone(shell);
        #[cfg(not(target_os = "macos"))]
        let start_minimized_row_for_tray = start_minimized_row.clone();
        #[cfg(not(target_os = "macos"))]
        tray_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            start_minimized_row_for_tray.set_visible(enabled);
            if !enabled {
                start_minimized_row_for_tray.set_active(false);
            }
            (tray_shell.effects)(crate::Effect::TrayEnabled(enabled));
        });
        #[cfg(not(target_os = "macos"))]
        let keep_running_shell = Rc::clone(shell);
        #[cfg(not(target_os = "macos"))]
        let tray_row_for_keep_running = tray_row.clone();
        #[cfg(not(target_os = "macos"))]
        keep_running_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            (keep_running_shell.effects)(crate::Effect::KeepRunningAfterClose(enabled));
            if enabled {
                tray_row_for_keep_running.set_active(true);
            }
            tray_row_for_keep_running.set_sensitive(!enabled);
        });
        #[cfg(not(target_os = "macos"))]
        let start_minimized_shell = Rc::clone(shell);
        #[cfg(not(target_os = "macos"))]
        start_minimized_row.connect_active_notify(move |row| {
            (start_minimized_shell.effects)(crate::Effect::StartMinimized(row.is_active()));
        });
        let type_to_search_shell = Rc::clone(shell);
        type_to_search_row.connect_active_notify(move |row| {
            type_to_search_shell.settings.set_app_setting(
                "type to search setting",
                row.is_active(),
                |settings| &mut settings.type_to_search_enabled,
            );
        });
        if shell
            .state
            .release_history
            .borrow()
            .automatic_updates_supported
            || shell.state.preview_windows_updates
        {
            automatic_updates_row.set_active(settings.automatic_updates_enabled);
            let automatic_updates_shell = Rc::clone(shell);
            automatic_updates_row.connect_active_notify(move |row| {
                automatic_updates_shell.settings.set_app_setting(
                    "automatic update setting",
                    row.is_active(),
                    |settings| &mut settings.automatic_updates_enabled,
                );
            });
        } else {
            window_group.remove(&automatic_updates_row);
        }
    }

    let intervals = [1, 6, 12, 24];
    release_check_interval_row.set_selected(
        intervals
            .iter()
            .position(|hours| *hours == settings.application.release_check_interval_hours)
            .unwrap_or(1) as u32,
    );
    let interval_shell = Rc::downgrade(shell);
    release_check_interval_row.connect_selected_notify(move |row| {
        let Some(shell) = interval_shell.upgrade() else {
            return;
        };
        let Some(&hours) = intervals.get(row.selected() as usize) else {
            return;
        };
        if shell
            .settings
            .set_app_setting("release check interval", hours, |settings| {
                &mut settings.application.release_check_interval_hours
            })
            .is_some()
        {
            dialogs::release_notes::schedule_periodic_release_checks(&shell);
        }
    });

    control_notifications_row.set_active(settings.application.control_notifications_enabled);
    let control_notifications_shell = Rc::clone(shell);
    control_notifications_row.connect_active_notify(move |row| {
        control_notifications_shell.settings.set_app_setting(
            "control notification setting",
            row.is_active(),
            |settings| &mut settings.application.control_notifications_enabled,
        );
    });
    notifications_row.set_active(settings.application.notifications_enabled);
    let notifications_shell = Rc::clone(shell);
    notifications_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        if notifications_shell
            .settings
            .set_app_setting("notification setting", enabled, |settings| {
                &mut settings.application.notifications_enabled
            })
            .is_some()
            && !enabled
        {
            (notifications_shell.effects)(crate::Effect::WithdrawNotification);
        }
    });
    release_notifications_row.set_active(settings.application.release_notifications_enabled);
    let release_notifications_shell = Rc::clone(shell);
    release_notifications_row.connect_active_notify(move |row| {
        release_notifications_shell.settings.set_app_setting(
            "release notification setting",
            row.is_active(),
            |settings| &mut settings.application.release_notifications_enabled,
        );
    });
    activity::wrap_general(shell, &page, &activity_overview_row, navigation_controls)
}
fn populate_layout_group(shell: &Rc<Preferences>, builder: &gtk::Builder) {
    let resource = crate::ui_resource::APPEARANCE_PREFERENCES_RESOURCE;
    gtk_widgets::objects!(builder, resource, {
        layout_group: adw::PreferencesGroup,
        lyrics_panel_row: adw::SwitchRow,
        visualizer_panel_row: adw::SwitchRow,
        combine_panels_row: adw::SwitchRow,
        bottom_bar_rating_row: adw::SwitchRow,
        narrow_row: adw::SwitchRow,
        threshold_row: adw::SpinRow,
    });
    let group = &layout_group;
    let settings = shell.settings.current.borrow().clone();

    let default_left_shell = Rc::clone(shell);
    let default_left_row = left_sidebar_row(
        &tr("Default left sidebar"),
        settings.layout.default_profile.left_sidebar,
        move |selected| {
            let mode = left_sidebar_mode_from_index(selected);
            default_left_shell
                .settings
                .update_app_settings("layout setting", |settings| {
                    if settings.layout.default_profile.left_sidebar == mode {
                        return false;
                    }
                    settings.layout.default_profile.left_sidebar = mode;
                    true
                });
            (default_left_shell.effects)(crate::Effect::LayoutChanged);
        },
    );
    group.add(&default_left_row);

    let default_right_shell = Rc::clone(shell);
    let default_right_row = right_sidebar_row(
        &tr("Default right sidebar"),
        settings.layout.default_profile.right_sidebar,
        move |selected| {
            let mode = right_sidebar_mode_from_index(selected);
            default_right_shell
                .settings
                .update_app_settings("layout setting", |settings| {
                    if settings.layout.default_profile.right_sidebar == mode {
                        return false;
                    }
                    settings.layout.default_profile.right_sidebar = mode;
                    settings.layout.sanitize();
                    true
                });
            (default_right_shell.effects)(crate::Effect::LayoutChanged);
        },
    );
    group.add(&default_right_row);

    lyrics_panel_row.set_active(settings.lyrics_panel_visible);
    let lyrics_panel_shell = Rc::clone(shell);
    lyrics_panel_row.connect_active_notify(move |row| {
        (lyrics_panel_shell.effects)(crate::Effect::LyricsPanelVisible(row.is_active()));
    });
    group.add(&lyrics_panel_row);

    visualizer_panel_row.set_active(settings.visualizer_panel_visible);
    let visualizer_panel_shell = Rc::clone(shell);
    visualizer_panel_row.connect_active_notify(move |row| {
        (visualizer_panel_shell.effects)(crate::Effect::VisualizerPanelVisible(row.is_active()));
    });
    group.add(&visualizer_panel_row);

    combine_panels_row.set_active(settings.right_panel.combined);
    let player = Rc::downgrade(&shell.player_ui);
    combine_panels_row.connect_active_notify(move |row| {
        if let Some(player) = player.upgrade() {
            player.set_right_panel_combined(row.is_active());
        }
    });
    group.add(&combine_panels_row);

    bottom_bar_rating_row.set_active(settings.show_bottom_bar_rating);
    let bottom_bar_rating_shell = Rc::clone(shell);
    bottom_bar_rating_row.connect_active_notify(move |row| {
        if bottom_bar_rating_shell
            .settings
            .set_app_setting("bottom bar rating setting", row.is_active(), |settings| {
                &mut settings.show_bottom_bar_rating
            })
            .is_some()
        {
            bottom_bar_rating_shell.player_ui.update_bottom_player();
        }
    });
    group.add(&bottom_bar_rating_row);

    narrow_row.set_active(settings.layout.narrow_enabled);
    group.add(&narrow_row);

    let threshold_adjustment = gtk::Adjustment::new(
        f64::from(settings.layout.narrow_threshold),
        f64::from(MIN_NARROW_LAYOUT_THRESHOLD),
        f64::from(MAX_NARROW_LAYOUT_THRESHOLD),
        10.0,
        100.0,
        0.0,
    );
    threshold_row.set_adjustment(Some(&threshold_adjustment));
    threshold_row.set_sensitive(settings.layout.narrow_enabled);
    let threshold_shell = Rc::clone(shell);
    threshold_row.connect_value_notify(move |row| {
        let threshold = row.value().round() as i32;
        threshold_shell
            .settings
            .update_app_settings("layout setting", |settings| {
                if settings.layout.narrow_threshold == threshold {
                    return false;
                }
                settings.layout.narrow_threshold = threshold;
                settings.layout.sanitize();
                true
            });
        (threshold_shell.effects)(crate::Effect::LayoutChanged);
    });
    group.add(&threshold_row);

    let narrow_left_shell = Rc::clone(shell);
    let narrow_left_row = left_sidebar_row(
        &tr("Narrow left sidebar"),
        settings.layout.narrow_profile.left_sidebar,
        move |selected| {
            let mode = left_sidebar_mode_from_index(selected);
            narrow_left_shell
                .settings
                .update_app_settings("layout setting", |settings| {
                    if settings.layout.narrow_profile.left_sidebar == mode {
                        return false;
                    }
                    settings.layout.narrow_profile.left_sidebar = mode;
                    true
                });
            (narrow_left_shell.effects)(crate::Effect::LayoutChanged);
        },
    );
    narrow_left_row.set_sensitive(settings.layout.narrow_enabled);
    group.add(&narrow_left_row);

    let narrow_right_shell = Rc::clone(shell);
    let narrow_right_row = right_sidebar_row(
        &tr("Narrow right sidebar"),
        settings.layout.narrow_profile.right_sidebar,
        move |selected| {
            let mode = right_sidebar_mode_from_index(selected);
            narrow_right_shell
                .settings
                .update_app_settings("layout setting", |settings| {
                    if settings.layout.narrow_profile.right_sidebar == mode {
                        return false;
                    }
                    settings.layout.narrow_profile.right_sidebar = mode;
                    settings.layout.sanitize();
                    true
                });
            (narrow_right_shell.effects)(crate::Effect::LayoutChanged);
        },
    );
    narrow_right_row.set_sensitive(settings.layout.narrow_enabled);
    group.add(&narrow_right_row);

    let threshold_row_for_toggle = threshold_row.clone();
    let narrow_left_row_for_toggle = narrow_left_row.clone();
    let narrow_right_row_for_toggle = narrow_right_row.clone();
    let narrow_shell = Rc::clone(shell);
    narrow_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        threshold_row_for_toggle.set_sensitive(enabled);
        narrow_left_row_for_toggle.set_sensitive(enabled);
        narrow_right_row_for_toggle.set_sensitive(enabled);
        narrow_shell
            .settings
            .update_app_settings("layout setting", |settings| {
                if settings.layout.narrow_enabled == enabled {
                    return false;
                }
                settings.layout.narrow_enabled = enabled;
                true
            });
        (narrow_shell.effects)(crate::Effect::LayoutChanged);
    });
}
fn configure_sidebar_items_expander(
    shell: &Rc<Preferences>,
    expander: &adw::ExpanderRow,
    pins: &adw::SwitchRow,
) {
    pins.set_active(shell.settings.current.borrow().sidebar.pins_visible);
    let pins_shell = Rc::clone(shell);
    pins.connect_active_notify(move |row| {
        let visible = row.is_active();
        if pins_shell
            .settings
            .update_app_settings("Pins visibility setting", |settings| {
                if settings.sidebar.pins_visible == visible {
                    return false;
                }
                settings.sidebar.pins_visible = visible;
                true
            })
            .is_some()
        {
            (pins_shell.effects)(crate::Effect::SidebarChanged);
        }
    });
    let rows = Rc::new(RefCell::new(
        Vec::<gtk::glib::WeakRef<adw::ActionRow>>::new(),
    ));
    let shell = Rc::clone(shell);
    let pins = pins.clone();
    populate_expander_once(&expander, move |expander| {
        populate_sidebar_item_rows(&shell, expander, &rows, &pins);
    });
}
fn populate_sidebar_item_rows(
    shell: &Rc<Preferences>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
    pins: &adw::SwitchRow,
) {
    for row in rows.borrow_mut().drain(..) {
        if let Some(row) = row.upgrade() {
            expander.remove(&row);
        }
    }

    let items = shell.settings.current.borrow().sidebar.route_items.clone();
    for (position, entry) in items.into_iter().enumerate() {
        let row = sidebar_item_row(shell, expander, rows, pins, entry, position);
        expander.add_row(&row);
        rows.borrow_mut().push(row.downgrade());
    }
    if pins.parent().is_some() {
        expander.remove(pins);
    }
    expander.add_row(pins);
}
fn sidebar_item_row(
    shell: &Rc<Preferences>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
    pins: &adw::SwitchRow,
    entry: SidebarRouteItemSettings,
    position: usize,
) -> adw::ActionRow {
    let descriptor = gtk_widgets::route::sidebar_route_descriptor(entry.item);
    let visible_shell = Rc::clone(shell);
    let visible_expander = expander.downgrade();
    let visible_rows = Rc::clone(rows);
    let visible_pins = pins.clone();
    let on_visible = move |is_visible| {
        visible_shell
            .settings
            .update_app_settings("sidebar setting", |settings| {
                settings.sidebar.set_route_visible(entry.item, is_visible)
            });
        (visible_shell.effects)(crate::Effect::SidebarChanged);
        if let Some(expander) = visible_expander.upgrade() {
            populate_sidebar_item_rows(&visible_shell, &expander, &visible_rows, &visible_pins);
        }
    };

    let move_shell = Rc::clone(shell);
    let move_expander = expander.downgrade();
    let move_rows = Rc::clone(rows);
    let move_pins = pins.clone();
    let on_move = move |delta| {
        move_sidebar_item(&move_shell, entry.item, delta);
        if let Some(expander) = move_expander.upgrade() {
            populate_sidebar_item_rows(&move_shell, &expander, &move_rows, &move_pins);
        }
    };

    let drop_shell = Rc::clone(shell);
    let drop_expander = expander.downgrade();
    let drop_rows = Rc::downgrade(rows);
    let drop_pins = pins.clone();
    let on_drop = move |source_id: String, after| {
        let Some(source_item) = SidebarRouteItem::from_stable_id(&source_id) else {
            return false;
        };
        let (Some(expander), Some(rows)) = (drop_expander.upgrade(), drop_rows.upgrade()) else {
            return false;
        };
        let changed = drop_shell
            .settings
            .update_app_settings("sidebar setting", |settings| {
                settings
                    .sidebar
                    .reorder_route(source_item, entry.item, after)
            })
            .is_some();
        if changed {
            (drop_shell.effects)(crate::Effect::SidebarChanged);
            populate_sidebar_item_rows(&drop_shell, &expander, &rows, &drop_pins);
        }
        changed
    };

    reorder_row(
        ReorderRowPresentation {
            title: tr(descriptor.title),
            subtitle: sidebar_route_item_subtitle(&entry, position),
            visible: entry.visible,
            visible_sensitive: true,
            up_sensitive: true,
            down_sensitive: true,
            drag_id: descriptor.stable_id.to_string(),
        },
        on_visible,
        on_move,
        on_drop,
    )
}
fn move_sidebar_item(shell: &Rc<Preferences>, item: SidebarRouteItem, delta: isize) {
    shell
        .settings
        .update_app_settings("sidebar setting", |settings| {
            settings.sidebar.move_route(item, delta)
        });
    (shell.effects)(crate::Effect::SidebarChanged);
}
fn sidebar_route_item_subtitle(entry: &SidebarRouteItemSettings, position: usize) -> String {
    let state = visibility_position_subtitle(entry.visible, position);
    if entry.item == SidebarRouteItem::Moods {
        format!(
            "{state} · {}",
            tr("Files need Mood/BPM tags written on them. Not supported for Jellyfin")
        )
    } else {
        state
    }
}

fn open_folder(shell: &Rc<Preferences>, folder: gtk::gio::File) {
    let shell = Rc::clone(shell);
    gtk::glib::spawn_future_local(async move {
        let result = async {
            if let Err(error) = folder
                .make_directory_future(gtk::glib::Priority::DEFAULT)
                .await
            {
                if !error.matches(gtk::gio::IOErrorEnum::Exists) {
                    return Err(error);
                }
            }
            gtk::FileLauncher::new(Some(&folder))
                .launch_future(Some(&shell.window))
                .await
        }
        .await;
        if let Err(error) = result {
            if !error.matches(gtk::DialogError::Dismissed)
                && !error.matches(gtk::DialogError::Cancelled)
            {
                shell
                    .control_feedback
                    .show_feedback_toast(localization::tr_with(
                        "Could not open folder: {error}",
                        &[("error", &error.to_string())],
                    ));
            }
        }
    });
}
