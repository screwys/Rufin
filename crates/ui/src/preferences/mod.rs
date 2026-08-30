use crate::layout::{large_popup_content_height, large_popup_content_width};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;
use crate::{
    MAX_NARROW_LAYOUT_THRESHOLD, MIN_NARROW_LAYOUT_THRESHOLD, SidebarRouteItem,
    SidebarRouteItemSettings,
};
use adw::prelude::*;
use localization::{language_option_index, language_options, msgid, tr};
use playback::StreamQuality;
use secrets::SecretStorageMode;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

mod context_menu;
pub(crate) mod dialogs;
mod general;
mod layout;
mod library;
pub(crate) mod persistence;
pub(crate) mod source;

use crate::player::casting_network_dropdown;
use general::{appearance_page, playback_page, scrobbling_page};
pub(crate) use general::{
    transition_from_index, transition_index, volume_scale_from_index, volume_scale_index,
};
use layout::{
    discord_display_from_index, discord_display_index, discord_link_from_index, discord_link_index,
    left_sidebar_mode_from_index, left_sidebar_row, right_sidebar_mode_from_index,
    right_sidebar_row, visibility_position_subtitle,
};
pub(crate) use library::locate_local_folder;

const PREFERENCES_DIALOG_WIDTH: i32 = 700;
const PREFERENCES_DIALOG_HEIGHT: i32 = 640;
const LASTFM_API_CREATE_URL: &str = "https://www.last.fm/api/account/create";
const LISTENBRAINZ_TOKEN_URL: &str = "https://listenbrainz.org/settings/";
const INTEGRATIONS_ICON_NAME: &str = "rufin-network-workgroup-symbolic";

pub(crate) struct PreferencesState {
    pub(crate) dialog: RefCell<Option<gtk::glib::WeakRef<adw::Dialog>>>,
    pub(crate) release_history: RefCell<crate::runtime::ReleaseHistory>,
    pub(crate) release_history_view: RefCell<Option<gtk::glib::WeakRef<gtk::Box>>>,
    pub(crate) release_notification_toast: RefCell<Option<adw::Toast>>,
    pub(crate) release_updating: RefCell<Option<String>>,
}

impl PreferencesState {
    pub(crate) fn active_dialog(&self) -> Option<adw::Dialog> {
        self.dialog
            .borrow()
            .as_ref()
            .and_then(gtk::glib::WeakRef::upgrade)
    }

    pub(crate) fn set_active_dialog(&self, dialog: &adw::Dialog) {
        self.dialog.replace(Some(dialog.downgrade()));
    }
}

pub(crate) fn selection_row<F>(
    title: &str,
    option_titles: &[String],
    selected: u32,
    on_selected: F,
) -> adw::ActionRow
where
    F: Fn(u32) + 'static,
{
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(&selection_buttons(
        option_titles,
        option_titles,
        selected,
        on_selected,
    ));
    row
}

pub(crate) fn controlled_selection_row(
    title: &str,
    option_titles: &[String],
    selected: u32,
) -> (adw::ActionRow, Vec<gtk::ToggleButton>) {
    let row = adw::ActionRow::builder().title(title).build();
    let (buttons, controls) = selection_buttons_unconnected(option_titles, option_titles, selected);
    row.add_suffix(&buttons);
    (row, controls)
}

fn quality_selection_row<F>(
    title: &str,
    qualities: &[StreamQuality],
    selected: u32,
    on_selected: F,
) -> adw::PreferencesRow
where
    F: Fn(u32) + 'static,
{
    let accessible_titles = qualities
        .iter()
        .copied()
        .map(quality_accessible_title)
        .collect::<Vec<_>>();
    let display_titles = qualities
        .iter()
        .copied()
        .map(quality_button_title)
        .collect::<Vec<_>>();
    let buttons = selection_buttons(&accessible_titles, &display_titles, selected, on_selected);

    let title_group = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    title_group.set_hexpand(true);
    title_group.set_width_request(1);
    title_group.set_valign(gtk::Align::Center);
    let title_label = gtk::Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.set_wrap(true);
    title_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title_group.append(&title_label);
    let unit = gtk::Label::new(Some(&tr("kbps")));
    unit.add_css_class("dim-label");
    title_group.append(&unit);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&title_group);
    content.append(&buttons);

    adw::PreferencesRow::builder()
        .title(title)
        .child(&content)
        .activatable(false)
        .selectable(false)
        .build()
}

fn selection_buttons<F>(
    accessible_titles: &[String],
    display_titles: &[String],
    selected: u32,
    on_selected: F,
) -> gtk::Box
where
    F: Fn(u32) + 'static,
{
    let (buttons, controls) =
        selection_buttons_unconnected(accessible_titles, display_titles, selected);
    let on_selected = Rc::new(on_selected);
    for (index, button) in controls.iter().enumerate() {
        let on_selected = Rc::clone(&on_selected);
        button.connect_toggled(move |button| {
            if button.is_active() {
                on_selected(index as u32);
            }
        });
    }
    buttons
}

fn selection_buttons_unconnected(
    accessible_titles: &[String],
    display_titles: &[String],
    selected: u32,
) -> (gtk::Box, Vec<gtk::ToggleButton>) {
    debug_assert_eq!(accessible_titles.len(), display_titles.len());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    buttons.add_css_class("linked");
    buttons.add_css_class("preference-selection-buttons");
    buttons.set_valign(gtk::Align::Center);
    let mut first_button: Option<gtk::ToggleButton> = None;
    let mut controls = Vec::with_capacity(accessible_titles.len());

    for (index, (accessible_title, display_title)) in
        accessible_titles.iter().zip(display_titles).enumerate()
    {
        let button = gtk::ToggleButton::with_label(display_title);
        button.add_css_class("preference-selection-button");
        button.set_tooltip_text(Some(accessible_title));
        button.update_property(&[gtk::accessible::Property::Label(accessible_title)]);
        if let Some(first) = &first_button {
            button.set_group(Some(first));
        } else {
            first_button = Some(button.clone());
        }
        button.set_active(index as u32 == selected);
        buttons.append(&button);
        controls.push(button);
    }

    (buttons, controls)
}

fn quality_accessible_title(quality: StreamQuality) -> String {
    match quality {
        StreamQuality::Original => tr("Original"),
        StreamQuality::MaxBitrateKbps(320) => tr("320 kbps"),
        StreamQuality::MaxBitrateKbps(256) => tr("256 kbps"),
        StreamQuality::MaxBitrateKbps(192) => tr("192 kbps"),
        StreamQuality::MaxBitrateKbps(128) => tr("128 kbps"),
        StreamQuality::MaxBitrateKbps(bitrate) => format!("{bitrate} kbps"),
    }
}

fn quality_button_title(quality: StreamQuality) -> String {
    match quality {
        StreamQuality::Original => tr("Original"),
        StreamQuality::MaxBitrateKbps(bitrate) => bitrate.to_string(),
    }
}

pub(crate) fn present_preferences_dialog(shell: &Rc<Shell>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::General, false, false);
}
pub(crate) fn present_library_preferences_dialog(shell: &Rc<Shell>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, false, false);
}
pub(crate) fn present_add_server_preferences_dialog(shell: &Rc<Shell>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, true, false);
}
pub(crate) fn present_downloads_preferences_dialog(shell: &Rc<Shell>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, false, true);
}

impl Shell {
    pub(crate) fn close_preferences_dialog(&self) {
        if let Some(dialog) = self.preferences.active_dialog() {
            dialog.close();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreferencesPageKind {
    General,
    Appearance,
    Integrations,
    Playback,
    Library,
}

impl PreferencesPageKind {
    const ALL: [Self; 5] = [
        Self::General,
        Self::Appearance,
        Self::Integrations,
        Self::Playback,
        Self::Library,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Integrations => "integrations",
            Self::Playback => "playback",
            Self::Library => "library",
        }
    }

    fn title(self) -> String {
        match self {
            Self::General => tr("General"),
            Self::Appearance => tr("Appearance"),
            Self::Integrations => tr("Integrations"),
            Self::Playback => tr("Playback"),
            Self::Library => tr("Library"),
        }
    }

    fn icon_name(self) -> &'static str {
        match self {
            Self::General => "rufin-preferences-system-symbolic",
            Self::Appearance => "rufin-preferences-desktop-appearance-symbolic",
            Self::Integrations => INTEGRATIONS_ICON_NAME,
            Self::Playback => "rufin-media-playback-start-symbolic",
            Self::Library => "rufin-drive-multidisk-symbolic",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

struct PreferencesSearchItem {
    page: PreferencesPageKind,
    title: String,
    context: String,
    searchable_text: String,
    target: gtk::glib::WeakRef<gtk::Widget>,
    expander: Option<gtk::glib::WeakRef<adw::ExpanderRow>>,
}

#[derive(Clone)]
pub(crate) struct PreferencesNavigationControls {
    back: gtk::Button,
    navigation: Rc<RefCell<Option<adw::NavigationView>>>,
    page_allows_back: Rc<Cell<bool>>,
    nested_page_visible: Rc<Cell<bool>>,
}

impl PreferencesNavigationControls {
    fn new() -> Self {
        let back = gtk::Button::from_icon_name("rufin-go-previous-symbolic");
        back.add_css_class("flat");
        back.add_css_class("preferences-nested-back");
        back.update_property(&[gtk::accessible::Property::Label(&tr("Back"))]);
        back.set_visible(false);

        let controls = Self {
            back,
            navigation: Rc::new(RefCell::new(None)),
            page_allows_back: Rc::new(Cell::new(false)),
            nested_page_visible: Rc::new(Cell::new(false)),
        };
        let navigation = Rc::clone(&controls.navigation);
        let page_allows_back = Rc::clone(&controls.page_allows_back);
        let nested_page_visible = Rc::clone(&controls.nested_page_visible);
        controls.back.connect_clicked(move |button| {
            if let Some(navigation) = navigation.borrow().as_ref() {
                navigation.pop();
            }
            nested_page_visible.set(false);
            button.set_visible(page_allows_back.get() && nested_page_visible.get());
        });
        controls
    }

    fn set_page_allows_back(&self, allowed: bool) {
        self.page_allows_back.set(allowed);
        self.update_visibility();
    }

    pub(crate) fn set_navigation(&self, navigation: &adw::NavigationView) {
        *self.navigation.borrow_mut() = Some(navigation.clone());
    }

    pub(crate) fn set_nested_page_visible(&self, visible: bool) {
        self.nested_page_visible.set(visible);
        self.update_visibility();
    }

    fn update_visibility(&self) {
        self.back
            .set_visible(self.page_allows_back.get() && self.nested_page_visible.get());
    }

    fn return_to_root(&self) {
        if let Some(navigation) = self.navigation.borrow().as_ref() {
            while navigation.pop() {}
        }
        self.nested_page_visible.set(false);
        self.update_visibility();
    }
}

fn present_preferences_dialog_with_page(
    shell: &Rc<Shell>,
    initial_page: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) {
    if !open_add_server {
        shell.clear_retained_add_server_form();
    }
    if let Some(dialog) = shell.preferences.active_dialog() {
        rebuild_preferences_dialog(
            shell,
            &dialog,
            initial_page,
            open_add_server,
            focus_download_queue,
        );
        present_light_dismiss_dialog(&dialog, &shell.chrome.window);
        return;
    }

    let dialog = adw::Dialog::builder()
        .title(tr("Preferences"))
        .content_width(large_popup_content_width(PREFERENCES_DIALOG_WIDTH))
        .content_height(large_popup_content_height(
            shell.chrome.window.height(),
            PREFERENCES_DIALOG_HEIGHT,
        ))
        .build();
    dialog.add_css_class("preferences");
    shell.preferences.set_active_dialog(&dialog);
    rebuild_preferences_dialog(
        shell,
        &dialog,
        initial_page,
        open_add_server,
        focus_download_queue,
    );

    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

fn rebuild_preferences_dialog(
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    initial_page: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) {
    dialog.set_title(&tr("Preferences"));

    let toolbar = adw::ToolbarView::new();

    let stack = adw::ViewStack::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    let switcher = adw::ViewSwitcher::builder()
        .policy(adw::ViewSwitcherPolicy::Wide)
        .stack(&stack)
        .build();
    let navigation_controls = PreferencesNavigationControls::new();
    let search_button = gtk::ToggleButton::builder()
        .icon_name("rufin-system-search-symbolic")
        .tooltip_text(tr("Search"))
        .build();
    search_button.add_css_class("flat");
    search_button.add_css_class("preferences-search-button");
    search_button.update_property(&[gtk::accessible::Property::Label(&tr("Search"))]);
    let start_controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    start_controls.append(&search_button);
    start_controls.append(&navigation_controls.back);
    let close_button = gtk::Button::from_icon_name("rufin-window-close-symbolic");
    close_button.add_css_class("flat");
    close_button.add_css_class("preferences-dialog-close");
    close_button.set_tooltip_text(Some(&tr("Close")));
    close_button.update_property(&[gtk::accessible::Property::Label(&tr("Close"))]);
    let dialog_for_close = dialog.downgrade();
    close_button.connect_clicked(move |_| {
        if let Some(dialog) = dialog_for_close.upgrade() {
            dialog.close();
        }
    });
    let switcher_bar = gtk::CenterBox::new();
    switcher_bar.add_css_class("preferences-tab-bar");
    switcher_bar.set_hexpand(true);
    switcher_bar.set_start_widget(Some(&start_controls));
    switcher_bar.set_center_widget(Some(&switcher));
    switcher_bar.set_end_widget(Some(&close_button));
    toolbar.add_top_bar(&switcher_bar);

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text(tr("Search preferences"))
        .hexpand(true)
        .build();
    let search_bar = gtk::SearchBar::new();
    search_bar.connect_entry(&search_entry);
    search_bar.set_child(Some(&search_entry));
    toolbar.add_top_bar(&search_bar);

    let search_results = adw::PreferencesPage::new();
    let search_results_group = adw::PreferencesGroup::new();
    search_results.add(&search_results_group);
    let content_stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    content_stack.add_named(&stack, Some("pages"));
    content_stack.add_named(&search_results, Some("search"));
    content_stack.set_visible_child_name("pages");
    toolbar.set_content(Some(&content_stack));

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
    navigation_controls.set_page_allows_back(initial_page == PreferencesPageKind::Library);
    ensure_preferences_page(
        shell,
        dialog,
        &page_slots,
        &navigation_controls,
        initial_page,
        open_add_server,
        focus_download_queue,
    );
    stack.set_visible_child_name(initial_page.name());
    let page_shell = Rc::clone(shell);
    let page_dialog = dialog.downgrade();
    let page_slots_for_switch = Rc::clone(&page_slots);
    let navigation_controls_for_switch = navigation_controls.clone();
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
        navigation_controls_for_switch.set_page_allows_back(kind == PreferencesPageKind::Library);
        ensure_preferences_page(
            &page_shell,
            &page_dialog,
            &page_slots_for_switch,
            &navigation_controls_for_switch,
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
    let search_navigation = navigation_controls.clone();
    let search_items_for_button = Rc::clone(&search_items);
    let search_entry_for_button = search_entry.clone();
    let search_bar_for_button = search_bar.clone();
    search_button.connect_toggled(move |button| {
        let enabled = button.is_active();
        search_bar_for_button.set_search_mode(enabled);
        if !enabled {
            search_entry_for_button.set_text("");
            return;
        }
        let Some(search_dialog) = search_dialog.upgrade() else {
            return;
        };
        for kind in PreferencesPageKind::ALL {
            ensure_preferences_page(
                &search_shell,
                &search_dialog,
                &search_slots,
                &search_navigation,
                kind,
                false,
                false,
            );
        }
        {
            let mut items = search_items_for_button.borrow_mut();
            items.clear();
            for (kind, slot) in search_slots.iter() {
                collect_preferences_search_items(slot.upcast_ref(), *kind, "", None, &mut items);
            }
        }
        search_entry_for_button.grab_focus();
    });

    let search_button_for_bar = search_button.clone();
    let content_stack_for_bar = content_stack.clone();
    search_bar.connect_search_mode_enabled_notify(move |bar| {
        search_button_for_bar.set_active(bar.is_search_mode());
        if !bar.is_search_mode() {
            content_stack_for_bar.set_visible_child_name("pages");
        }
    });

    let search_items_for_entry = Rc::clone(&search_items);
    let rendered_rows_for_entry = Rc::clone(&rendered_search_rows);
    let results_group_for_entry = search_results_group.clone();
    let page_stack_for_entry = stack.clone();
    let content_stack_for_entry = content_stack.clone();
    let search_bar_for_entry = search_bar.clone();
    let navigation_for_entry = navigation_controls.clone();
    search_entry.connect_search_changed(move |entry| {
        for row in rendered_rows_for_entry.borrow_mut().drain(..) {
            results_group_for_entry.remove(&row);
        }
        let query = entry.text();
        if query.trim().is_empty() {
            content_stack_for_entry.set_visible_child_name("pages");
            return;
        }
        content_stack_for_entry.set_visible_child_name("search");
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
            let target = item.target.clone();
            let expander = item.expander.clone();
            let page_stack = page_stack_for_entry.clone();
            let content_stack = content_stack_for_entry.clone();
            let search_bar = search_bar_for_entry.clone();
            let navigation = navigation_for_entry.clone();
            row.connect_activated(move |_| {
                if let Some(expander) = expander.as_ref().and_then(gtk::glib::WeakRef::upgrade) {
                    expander.set_expanded(true);
                }
                navigation.return_to_root();
                page_stack.set_visible_child_name(page.name());
                content_stack.set_visible_child_name("pages");
                search_bar.set_search_mode(false);
                let target = target.clone();
                gtk::glib::idle_add_local_once(move || {
                    if let Some(target) = target.upgrade() {
                        target.grab_focus();
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

    dialog.set_child(Some(&toolbar));
}

fn collect_preferences_search_items(
    widget: &gtk::Widget,
    page: PreferencesPageKind,
    group_title: &str,
    expander: Option<gtk::glib::WeakRef<adw::ExpanderRow>>,
    items: &mut Vec<PreferencesSearchItem>,
) {
    let group_title = widget
        .clone()
        .downcast::<adw::PreferencesGroup>()
        .ok()
        .map(|group| group.title().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| group_title.to_owned());
    let (group_title, expander) = widget
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
            (title, Some(row.downgrade()))
        })
        .unwrap_or((group_title, expander));

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
                target: widget.downgrade(),
                expander: expander.clone(),
            });
        }
    }

    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        collect_preferences_search_items(&current, page, &group_title, expander.clone(), items);
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

fn ensure_preferences_page(
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    page_slots: &[(PreferencesPageKind, gtk::Box)],
    navigation_controls: &PreferencesNavigationControls,
    kind: PreferencesPageKind,
    open_add_server: bool,
    focus_download_queue: bool,
) {
    let Some((_, slot)) = page_slots.iter().find(|(slot_kind, _)| *slot_kind == kind) else {
        return;
    };
    if slot.first_child().is_some() {
        return;
    }
    slot.append(&build_preferences_page(
        kind,
        shell,
        dialog,
        navigation_controls,
        open_add_server,
        focus_download_queue,
    ));
}
fn build_preferences_page(
    kind: PreferencesPageKind,
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    navigation_controls: &PreferencesNavigationControls,
    open_add_server: bool,
    focus_download_queue: bool,
) -> gtk::Widget {
    match kind {
        PreferencesPageKind::General => general_page(shell, dialog).upcast(),
        PreferencesPageKind::Appearance => appearance_page(shell).upcast(),
        PreferencesPageKind::Integrations => scrobbling_page(shell).upcast(),
        PreferencesPageKind::Playback => playback_page(shell).upcast(),
        PreferencesPageKind::Library => library::library_page(
            shell,
            dialog,
            navigation_controls,
            open_add_server,
            focus_download_queue,
        ),
    }
}
fn general_page(shell: &Rc<Shell>, dialog: &adw::Dialog) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title(tr("General"))
        .icon_name("rufin-preferences-system-symbolic")
        .build();

    let settings = shell.settings.current.borrow().clone();

    let language_group = adw::PreferencesGroup::builder()
        .title(tr("Language"))
        .build();
    let language_options = Rc::new(language_options());
    let language_titles = language_options
        .iter()
        .map(|option| option.title.as_str())
        .collect::<Vec<_>>();
    let language_model = gtk::StringList::new(&language_titles);
    let language_row = adw::ComboRow::builder()
        .title(tr("Language"))
        .model(&language_model)
        .selected(language_option_index(
            language_options.as_ref(),
            &settings.language,
        ))
        .build();
    let language_shell = Rc::clone(shell);
    let language_options_for_row = Rc::clone(&language_options);
    let dialog_for_language = dialog.downgrade();
    language_row.connect_selected_notify(move |row| {
        let Some(option) = language_options_for_row.get(row.selected() as usize) else {
            return;
        };
        let language = option.id.clone();
        if language_shell.set_language_preference(language) {
            let Some(dialog) = dialog_for_language.upgrade() else {
                return;
            };
            rebuild_preferences_dialog(
                &language_shell,
                &dialog,
                PreferencesPageKind::General,
                false,
                false,
            );
        }
    });
    language_group.add(&language_row);
    page.add(&language_group);

    {
        let window_group = adw::PreferencesGroup::builder()
            .title(tr("App settings"))
            .build();
        #[cfg(not(target_os = "macos"))]
        let tray_row = adw::SwitchRow::builder()
            .title(tr("Show tray icon"))
            .active(settings.tray_enabled)
            .sensitive(!settings.keep_running_after_close)
            .build();
        #[cfg(not(target_os = "macos"))]
        let keep_running_row = adw::SwitchRow::builder()
            .title(tr("Keep Rufin running after closing the window"))
            .active(settings.keep_running_after_close)
            .build();
        #[cfg(not(target_os = "macos"))]
        let start_minimized_row = adw::SwitchRow::builder()
            .title(tr("Start minimized"))
            .active(settings.tray_enabled && settings.start_minimized)
            .build();
        let type_to_search_row = adw::SwitchRow::builder()
            .title(tr("Type to search"))
            .active(settings.type_to_search_enabled)
            .build();
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
            tray_shell.set_tray_enabled(enabled);
        });
        #[cfg(not(target_os = "macos"))]
        let keep_running_shell = Rc::clone(shell);
        #[cfg(not(target_os = "macos"))]
        let tray_row_for_keep_running = tray_row.clone();
        #[cfg(not(target_os = "macos"))]
        keep_running_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            keep_running_shell.set_keep_running_after_close(enabled);
            if enabled {
                tray_row_for_keep_running.set_active(true);
            }
            tray_row_for_keep_running.set_sensitive(!enabled);
        });
        #[cfg(not(target_os = "macos"))]
        let start_minimized_shell = Rc::clone(shell);
        #[cfg(not(target_os = "macos"))]
        start_minimized_row.connect_active_notify(move |row| {
            start_minimized_shell.set_start_minimized_enabled(row.is_active());
        });
        let type_to_search_shell = Rc::clone(shell);
        type_to_search_row.connect_active_notify(move |row| {
            type_to_search_shell.set_type_to_search_enabled(row.is_active());
        });
        #[cfg(not(target_os = "macos"))]
        window_group.add(&tray_row);
        #[cfg(not(target_os = "macos"))]
        window_group.add(&keep_running_row);
        #[cfg(not(target_os = "macos"))]
        window_group.add(&start_minimized_row);
        window_group.add(&type_to_search_row);
        if shell
            .preferences
            .release_history
            .borrow()
            .automatic_updates_supported
        {
            let automatic_updates_row = adw::SwitchRow::builder()
                .title(tr("Automatic updates"))
                .subtitle(tr("Install available updates when Rufin starts"))
                .active(settings.automatic_updates_enabled)
                .build();
            let automatic_updates_shell = Rc::clone(shell);
            automatic_updates_row.connect_active_notify(move |row| {
                automatic_updates_shell.set_automatic_updates_enabled(row.is_active());
            });
            window_group.add(&automatic_updates_row);
        }
        page.add(&window_group);
    }

    let notifications_group = adw::PreferencesGroup::builder()
        .title(tr("Notifications"))
        .build();
    let control_notifications_row = adw::SwitchRow::builder()
        .title(tr("Control notifications"))
        .active(settings.control_notifications_enabled)
        .build();
    let control_notifications_shell = Rc::clone(shell);
    control_notifications_row.connect_active_notify(move |row| {
        control_notifications_shell.set_control_notifications_enabled(row.is_active());
    });
    notifications_group.add(&control_notifications_row);

    let notifications_row = adw::SwitchRow::builder()
        .title(tr("Now playing notifications"))
        .active(settings.notifications_enabled)
        .build();
    let notifications_shell = Rc::clone(shell);
    notifications_row.connect_active_notify(move |row| {
        notifications_shell.set_notifications_enabled(row.is_active());
    });
    notifications_group.add(&notifications_row);

    let release_notifications_row = adw::SwitchRow::builder()
        .title(tr("Release notifications"))
        .active(settings.release_notifications_enabled)
        .build();
    let release_notifications_shell = Rc::clone(shell);
    release_notifications_row.connect_active_notify(move |row| {
        release_notifications_shell.set_release_notifications_enabled(row.is_active());
    });
    notifications_group.add(&release_notifications_row);
    page.add(&notifications_group);

    let metadata_group = adw::PreferencesGroup::builder()
        .title(tr("Metadata"))
        .build();
    let prefer_server_playlist_row = adw::SwitchRow::builder()
        .title(tr("Prefer server playlist covers"))
        .active(settings.prefer_server_playlist_covers)
        .build();
    let prefer_server_playlist_shell = Rc::clone(shell);
    prefer_server_playlist_row.connect_active_notify(move |row| {
        prefer_server_playlist_shell.set_prefer_server_playlist_covers(row.is_active());
    });

    let external_metadata_row = adw::SwitchRow::builder()
        .title(tr("External metadata lookup"))
        .active(settings.external_metadata_enabled)
        .build();
    let metadata_shell = Rc::clone(shell);
    external_metadata_row.connect_active_notify(move |row| {
        metadata_shell.set_external_metadata_enabled(row.is_active());
    });

    metadata_group.add(&prefer_server_playlist_row);
    metadata_group.add(&external_metadata_row);
    page.add(&metadata_group);

    let external_links = settings.external_site_links.clone();
    let external_links_group = adw::PreferencesGroup::builder()
        .title(tr("External site links"))
        .build();
    let external_links_row = adw::SwitchRow::builder()
        .title(tr("Show external site links"))
        .subtitle(tr("Show external service icons on album and artist pages"))
        .active(external_links.enabled)
        .build();
    let lastfm_links_row = adw::SwitchRow::builder()
        .title(tr("Last.fm"))
        .active(external_links.lastfm)
        .sensitive(external_links.enabled)
        .build();
    let musicbrainz_links_row = adw::SwitchRow::builder()
        .title(tr("MusicBrainz"))
        .active(external_links.musicbrainz)
        .sensitive(external_links.enabled)
        .build();
    let server_links_row = adw::SwitchRow::builder()
        .title(tr("Server"))
        .active(external_links.server)
        .sensitive(external_links.enabled)
        .build();
    let external_links_shell = Rc::clone(shell);
    let lastfm_links_for_master = lastfm_links_row.clone();
    let musicbrainz_links_for_master = musicbrainz_links_row.clone();
    let server_links_for_master = server_links_row.clone();
    external_links_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        lastfm_links_for_master.set_sensitive(enabled);
        musicbrainz_links_for_master.set_sensitive(enabled);
        server_links_for_master.set_sensitive(enabled);
        external_links_shell.set_external_site_links_enabled(enabled);
    });
    let lastfm_links_shell = Rc::clone(shell);
    lastfm_links_row.connect_active_notify(move |row| {
        lastfm_links_shell.set_lastfm_site_links_enabled(row.is_active());
    });
    let musicbrainz_links_shell = Rc::clone(shell);
    musicbrainz_links_row.connect_active_notify(move |row| {
        musicbrainz_links_shell.set_musicbrainz_site_links_enabled(row.is_active());
    });
    let server_links_shell = Rc::clone(shell);
    server_links_row.connect_active_notify(move |row| {
        server_links_shell.set_server_site_links_enabled(row.is_active());
    });
    external_links_group.add(&external_links_row);
    external_links_group.add(&lastfm_links_row);
    external_links_group.add(&musicbrainz_links_row);
    external_links_group.add(&server_links_row);
    page.add(&external_links_group);

    let discord_group = adw::PreferencesGroup::builder()
        .title(tr("Discord"))
        .build();
    let presence_row = adw::SwitchRow::builder()
        .title(tr("Rich presence"))
        .active(settings.rich_presence.enabled)
        .build();
    let presence_shell = Rc::clone(shell);
    presence_row.connect_active_notify(move |row| {
        presence_shell.set_discord_presence_enabled(row.is_active());
    });
    discord_group.add(&presence_row);

    let display_titles = [tr("Application name"), tr("Song title"), tr("Artist name")];
    let display_refs = display_titles
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let display_options = gtk::StringList::new(&display_refs);
    let display_row = adw::ComboRow::builder()
        .title(tr("Status display"))
        .model(&display_options)
        .selected(discord_display_index(settings.rich_presence.display_type))
        .build();
    let display_shell = Rc::clone(shell);
    display_row.connect_selected_notify(move |row| {
        display_shell.set_discord_display_type(discord_display_from_index(row.selected()));
    });
    discord_group.add(&display_row);

    let link_titles = [
        tr("None"),
        tr("Last.fm"),
        tr("MusicBrainz"),
        tr("MusicBrainz + Last.fm"),
    ];
    let link_refs = link_titles.iter().map(String::as_str).collect::<Vec<_>>();
    let link_options = gtk::StringList::new(&link_refs);
    let link_row = adw::ComboRow::builder()
        .title(tr("Metadata source"))
        .subtitle(tr("Source to use for cover images and song/artist links"))
        .model(&link_options)
        .selected(discord_link_index(settings.rich_presence.link_type))
        .build();
    let link_shell = Rc::clone(shell);
    link_row.connect_selected_notify(move |row| {
        link_shell.set_discord_link_type(discord_link_from_index(row.selected()));
    });
    discord_group.add(&link_row);

    let paused_row = adw::SwitchRow::builder()
        .title(tr("Show paused status"))
        .active(settings.rich_presence.show_paused)
        .build();
    let paused_shell = Rc::clone(shell);
    paused_row.connect_active_notify(move |row| {
        paused_shell.set_discord_show_paused(row.is_active());
    });
    discord_group.add(&paused_row);

    let listening_row = adw::SwitchRow::builder()
        .title(tr("Use listening activity"))
        .subtitle(tr("Set the Discord activity type to Listening"))
        .active(settings.rich_presence.show_as_listening)
        .build();
    let listening_shell = Rc::clone(shell);
    listening_row.connect_active_notify(move |row| {
        listening_shell.set_discord_show_as_listening(row.is_active());
    });
    discord_group.add(&listening_row);

    page.add(&discord_group);

    let privacy_group = adw::PreferencesGroup::builder()
        .title(tr("Privacy and Security"))
        .build();
    let private_row = adw::SwitchRow::builder()
        .title(tr("Private mode"))
        .active(settings.private_mode)
        .build();
    let private_shell = Rc::clone(shell);
    private_row.connect_active_notify(move |row| {
        private_shell.set_private_mode(row.is_active());
    });
    privacy_group.add(&private_row);

    let cast_proxy_row = adw::SwitchRow::builder()
        .title(tr("Proxy casting through Rufin"))
        .subtitle(tr(
            "Route server media through this device instead of sharing provider URLs with the renderer",
        ))
        .active(settings.cast_proxy_enabled)
        .build();
    let cast_proxy_shell = Rc::clone(shell);
    cast_proxy_row.connect_active_notify(move |row| {
        cast_proxy_shell.set_cast_proxy_enabled(row.is_active());
    });
    privacy_group.add(&cast_proxy_row);

    let cast_network_row = adw::ActionRow::builder()
        .title(tr("Casting network"))
        .subtitle(tr(
            "Network used to send local and proxied media to casting devices",
        ))
        .build();
    let cast_network_dropdown = casting_network_dropdown(shell, 220);
    cast_network_row.add_suffix(&cast_network_dropdown);
    cast_network_row.set_activatable_widget(Some(&cast_network_dropdown));
    privacy_group.add(&cast_network_row);

    let secret_storage_titles = [tr("Legacy"), tr("Secure storage")];
    let secret_storage_refs = secret_storage_titles
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let secret_storage_row = adw::ComboRow::builder()
        .title(tr("Secret storage"))
        .model(&gtk::StringList::new(&secret_storage_refs))
        .selected(secret_storage_mode_index(settings.secret_storage_mode))
        .build();
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
            .secret_storage_mode;
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
        let window = shell.chrome.window.clone();
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
    privacy_group.add(&secret_storage_row);

    page.add(&privacy_group);

    page
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
fn layout_group(shell: &Rc<Shell>) -> adw::PreferencesGroup {
    let settings = shell.settings.current.borrow().clone();
    let group = adw::PreferencesGroup::builder().title(tr("Layout")).build();

    let default_left_shell = Rc::clone(shell);
    let default_left_row = left_sidebar_row(
        &tr("Default left sidebar"),
        settings.layout.default_profile.left_sidebar,
        move |selected| {
            let mode = left_sidebar_mode_from_index(selected);
            default_left_shell.update_app_settings("layout setting", |settings| {
                if settings.layout.default_profile.left_sidebar == mode {
                    return false;
                }
                settings.layout.default_profile.left_sidebar = mode;
                true
            });
            default_left_shell.update_layout();
        },
    );
    group.add(&default_left_row);

    let default_right_shell = Rc::clone(shell);
    let default_right_row = right_sidebar_row(
        &tr("Default right sidebar"),
        settings.layout.default_profile.right_sidebar,
        move |selected| {
            let mode = right_sidebar_mode_from_index(selected);
            default_right_shell.update_app_settings("layout setting", |settings| {
                if settings.layout.default_profile.right_sidebar == mode {
                    return false;
                }
                settings.layout.default_profile.right_sidebar = mode;
                settings.layout.sanitize();
                true
            });
            default_right_shell.update_layout();
        },
    );
    group.add(&default_right_row);

    let lyrics_panel_row = adw::SwitchRow::builder()
        .title(tr("Show lyrics"))
        .active(settings.lyrics_panel_visible)
        .build();
    let lyrics_panel_shell = Rc::clone(shell);
    lyrics_panel_row.connect_active_notify(move |row| {
        lyrics_panel_shell.set_lyrics_panel_visible(row.is_active());
    });
    group.add(&lyrics_panel_row);

    let visualizer_panel_row = adw::SwitchRow::builder()
        .title(tr("Show visualizer"))
        .active(settings.visualizer_panel_visible)
        .build();
    let visualizer_panel_shell = Rc::clone(shell);
    visualizer_panel_row.connect_active_notify(move |row| {
        visualizer_panel_shell.set_visualizer_panel_visible(row.is_active());
    });
    group.add(&visualizer_panel_row);

    let bottom_bar_rating_row = adw::SwitchRow::builder()
        .title(tr("Show ratings in the bottom bar"))
        .active(settings.show_bottom_bar_rating)
        .build();
    let bottom_bar_rating_shell = Rc::clone(shell);
    bottom_bar_rating_row.connect_active_notify(move |row| {
        bottom_bar_rating_shell.set_show_bottom_bar_rating(row.is_active());
    });
    group.add(&bottom_bar_rating_row);

    let narrow_row = adw::SwitchRow::builder()
        .title(tr("Use different layout below a threshold width"))
        .active(settings.layout.narrow_enabled)
        .build();
    group.add(&narrow_row);

    let threshold_adjustment = gtk::Adjustment::new(
        f64::from(settings.layout.narrow_threshold),
        f64::from(MIN_NARROW_LAYOUT_THRESHOLD),
        f64::from(MAX_NARROW_LAYOUT_THRESHOLD),
        10.0,
        100.0,
        0.0,
    );
    let threshold_row = adw::SpinRow::builder()
        .title(tr("Narrow layout threshold"))
        .adjustment(&threshold_adjustment)
        .digits(0)
        .numeric(true)
        .sensitive(settings.layout.narrow_enabled)
        .build();
    let threshold_unit = gtk::Label::new(Some("px"));
    threshold_unit.add_css_class("dim-label");
    threshold_row.add_suffix(&threshold_unit);
    let threshold_shell = Rc::clone(shell);
    threshold_row.connect_value_notify(move |row| {
        let threshold = row.value().round() as i32;
        threshold_shell.update_app_settings("layout setting", |settings| {
            if settings.layout.narrow_threshold == threshold {
                return false;
            }
            settings.layout.narrow_threshold = threshold;
            settings.layout.sanitize();
            true
        });
        threshold_shell.update_layout();
    });
    group.add(&threshold_row);

    let narrow_left_shell = Rc::clone(shell);
    let narrow_left_row = left_sidebar_row(
        &tr("Narrow left sidebar"),
        settings.layout.narrow_profile.left_sidebar,
        move |selected| {
            let mode = left_sidebar_mode_from_index(selected);
            narrow_left_shell.update_app_settings("layout setting", |settings| {
                if settings.layout.narrow_profile.left_sidebar == mode {
                    return false;
                }
                settings.layout.narrow_profile.left_sidebar = mode;
                true
            });
            narrow_left_shell.update_layout();
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
            narrow_right_shell.update_app_settings("layout setting", |settings| {
                if settings.layout.narrow_profile.right_sidebar == mode {
                    return false;
                }
                settings.layout.narrow_profile.right_sidebar = mode;
                settings.layout.sanitize();
                true
            });
            narrow_right_shell.update_layout();
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
        narrow_shell.update_app_settings("layout setting", |settings| {
            if settings.layout.narrow_enabled == enabled {
                return false;
            }
            settings.layout.narrow_enabled = enabled;
            true
        });
        narrow_shell.update_layout();
    });

    group
}
fn sidebar_items_expander(shell: &Rc<Shell>) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::builder()
        .title(tr("Sidebar Items"))
        .expanded(false)
        .build();
    let pins = adw::SwitchRow::builder()
        .title(tr("Pins"))
        .subtitle(tr("Fixed position"))
        .active(shell.settings.current.borrow().sidebar.pins_visible)
        .build();
    let pins_shell = Rc::clone(shell);
    pins.connect_active_notify(move |row| {
        let visible = row.is_active();
        if pins_shell
            .update_app_settings("Pins visibility setting", |settings| {
                if settings.sidebar.pins_visible == visible {
                    return false;
                }
                settings.sidebar.pins_visible = visible;
                true
            })
            .is_some()
        {
            pins_shell.rebuild_sidebar_navigation();
        }
    });
    let rows = Rc::new(RefCell::new(
        Vec::<gtk::glib::WeakRef<adw::ActionRow>>::new(),
    ));
    populate_sidebar_item_rows(shell, &expander, &rows, &pins);
    expander
}
fn populate_sidebar_item_rows(
    shell: &Rc<Shell>,
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
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
    pins: &adw::SwitchRow,
    entry: SidebarRouteItemSettings,
    position: usize,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(tr(sidebar_route_item_title(entry.item)))
        .subtitle(sidebar_route_item_subtitle(&entry, position))
        .build();

    let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
    drag.add_css_class("dim-label");
    drag.set_tooltip_text(Some(&tr("Drag to reorder")));
    row.add_prefix(&drag);

    let visible = gtk::Switch::new();
    visible.set_active(entry.visible);
    visible.set_valign(gtk::Align::Center);

    let up = gtk::Button::from_icon_name("rufin-go-up-symbolic");
    up.add_css_class("flat");
    up.set_tooltip_text(Some(&tr("Move up")));
    up.set_valign(gtk::Align::Center);
    row.add_suffix(&up);

    let down = gtk::Button::from_icon_name("rufin-go-down-symbolic");
    down.add_css_class("flat");
    down.set_tooltip_text(Some(&tr("Move down")));
    down.set_valign(gtk::Align::Center);
    row.add_suffix(&down);

    row.add_suffix(&visible);
    row.set_activatable_widget(Some(&visible));

    {
        let shell = Rc::clone(shell);
        let expander = expander.downgrade();
        let rows = Rc::clone(rows);
        let pins = pins.clone();
        visible.connect_active_notify(move |switch| {
            let item = entry.item;
            let is_visible = switch.is_active();
            shell.update_app_settings("sidebar setting", |settings| {
                if let Some(stored) = settings
                    .sidebar
                    .route_items
                    .iter_mut()
                    .find(|stored| stored.item == item)
                {
                    if stored.visible == is_visible {
                        return false;
                    }
                    stored.visible = is_visible;
                }
                settings.sidebar.sanitize();
                true
            });
            shell.rebuild_sidebar_navigation();
            let Some(expander) = expander.upgrade() else {
                return;
            };
            populate_sidebar_item_rows(&shell, &expander, &rows, &pins);
        });
    }
    {
        let shell = Rc::clone(shell);
        let expander = expander.downgrade();
        let rows = Rc::clone(rows);
        let pins = pins.clone();
        up.connect_clicked(move |_| {
            move_sidebar_item(&shell, entry.item, -1);
            let Some(expander) = expander.upgrade() else {
                return;
            };
            populate_sidebar_item_rows(&shell, &expander, &rows, &pins);
        });
    }
    {
        let shell = Rc::clone(shell);
        let expander = expander.downgrade();
        let rows = Rc::clone(rows);
        let pins = pins.clone();
        down.connect_clicked(move |_| {
            move_sidebar_item(&shell, entry.item, 1);
            let Some(expander) = expander.upgrade() else {
                return;
            };
            populate_sidebar_item_rows(&shell, &expander, &rows, &pins);
        });
    }

    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let item_id = sidebar_route_item_drag_id(entry.item).to_string();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&item_id.to_value()))
    });
    drag.add_controller(source);

    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let shell = Rc::clone(shell);
    let expander = expander.downgrade();
    let rows = Rc::downgrade(rows);
    let pins = pins.clone();
    let row_for_drop = row.downgrade();
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(source_id) = value.get::<String>() else {
            return false;
        };
        let Some(source_item) = sidebar_drag_route(&source_id) else {
            return false;
        };
        if source_item == entry.item {
            return false;
        }
        let (Some(row), Some(expander), Some(rows)) =
            (row_for_drop.upgrade(), expander.upgrade(), rows.upgrade())
        else {
            return false;
        };
        let after = y > f64::from(row.height()) / 2.0;
        let changed = shell
            .update_app_settings("sidebar setting", |settings| {
                let changed = reorder_sidebar_item_settings(
                    &mut settings.sidebar.route_items,
                    source_item,
                    entry.item,
                    after,
                );
                if changed {
                    settings.sidebar.sanitize();
                }
                changed
            })
            .is_some();
        if changed {
            shell.rebuild_sidebar_navigation();
            populate_sidebar_item_rows(&shell, &expander, &rows, &pins);
        }
        changed
    });
    row.add_controller(drop_target);

    row
}
fn move_sidebar_item(shell: &Rc<Shell>, item: SidebarRouteItem, delta: isize) {
    shell.update_app_settings("sidebar setting", |settings| {
        let Some(index) = settings
            .sidebar
            .route_items
            .iter()
            .position(|entry| entry.item == item)
        else {
            return false;
        };
        let new_index = if delta < 0 {
            index.saturating_sub(1)
        } else {
            (index + 1).min(settings.sidebar.route_items.len().saturating_sub(1))
        };
        if index == new_index {
            return false;
        }
        settings.sidebar.route_items.swap(index, new_index);
        settings.sidebar.sanitize();
        true
    });
    shell.rebuild_sidebar_navigation();
}
fn reorder_sidebar_item_settings(
    items: &mut Vec<SidebarRouteItemSettings>,
    source: SidebarRouteItem,
    target: SidebarRouteItem,
    after: bool,
) -> bool {
    if source == target {
        return false;
    }
    let before = items.clone();
    let Some(source_index) = items.iter().position(|entry| entry.item == source) else {
        return false;
    };
    let entry = items.remove(source_index);
    let Some(mut target_index) = items.iter().position(|entry| entry.item == target) else {
        items.insert(source_index.min(items.len()), entry);
        return false;
    };
    if after {
        target_index += 1;
    }
    items.insert(target_index.min(items.len()), entry);
    *items != before
}
fn sidebar_route_item_drag_id(item: SidebarRouteItem) -> &'static str {
    match item {
        SidebarRouteItem::Home => "Home",
        SidebarRouteItem::Search => "Search",
        SidebarRouteItem::Favorites => "Favorites",
        SidebarRouteItem::History => "History",
        SidebarRouteItem::Albums => "Albums",
        SidebarRouteItem::Tracks => "Tracks",
        SidebarRouteItem::Artists => "Artists",
        SidebarRouteItem::AlbumArtists => "AlbumArtists",
        SidebarRouteItem::Genres => "Genres",
        SidebarRouteItem::Moods => "Moods",
        SidebarRouteItem::Folders => "Folders",
        SidebarRouteItem::Playlists => "Playlists",
        SidebarRouteItem::SmartPlaylists => "SmartPlaylists",
    }
}
fn sidebar_drag_route(id: &str) -> Option<SidebarRouteItem> {
    SidebarRouteItem::all()
        .into_iter()
        .find(|item| sidebar_route_item_drag_id(*item) == id)
}
fn sidebar_route_item_title(item: SidebarRouteItem) -> &'static str {
    match item {
        SidebarRouteItem::Home => msgid("Home"),
        SidebarRouteItem::Search => msgid("Search"),
        SidebarRouteItem::Favorites => msgid("Favorites"),
        SidebarRouteItem::History => msgid("History"),
        SidebarRouteItem::Albums => msgid("Albums"),
        SidebarRouteItem::Tracks => msgid("Tracks"),
        SidebarRouteItem::Artists => msgid("Artists"),
        SidebarRouteItem::AlbumArtists => msgid("Album Artists"),
        SidebarRouteItem::Genres => msgid("Genres"),
        SidebarRouteItem::Moods => msgid("Moods"),
        SidebarRouteItem::Folders => msgid("Folders"),
        SidebarRouteItem::Playlists => msgid("Playlists"),
        SidebarRouteItem::SmartPlaylists => msgid("Smart Playlists"),
    }
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
