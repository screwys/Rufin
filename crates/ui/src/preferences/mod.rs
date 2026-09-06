pub(crate) mod backup;
use crate::layout::{
    LARGE_POPUP_BASE_HEIGHT, LARGE_POPUP_BASE_WIDTH, large_popup_content_height,
    large_popup_content_width, width_allocation_owner,
};
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::shell::Shell;
use crate::{
    MAX_NARROW_LAYOUT_THRESHOLD, MIN_NARROW_LAYOUT_THRESHOLD, SidebarRouteItem,
    SidebarRouteItemSettings,
};
use adw::prelude::*;
use localization::{language_option_index, language_options, tr};
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
use general::{ScrobblingCredentialDrafts, appearance_page, playback_page, scrobbling_page};
pub(crate) use general::{
    transition_from_index, transition_index, volume_scale_from_index, volume_scale_index,
};
use layout::{
    ReorderRowPresentation, discord_display_from_index, discord_display_index,
    discord_link_from_index, discord_link_index, left_sidebar_mode_from_index, left_sidebar_row,
    reorder_row, right_sidebar_mode_from_index, right_sidebar_row, visibility_position_subtitle,
};
pub(crate) use library::locate_local_folder;

const LASTFM_API_CREATE_URL: &str = "https://www.last.fm/api/account/create";
const LISTENBRAINZ_TOKEN_URL: &str = "https://listenbrainz.org/settings/";
const INTEGRATIONS_ICON_NAME: &str = "rufin-network-workgroup-symbolic";

pub(crate) struct PreferencesState {
    pub(crate) dialog: gtk::glib::WeakRef<adw::Dialog>,
    pub(crate) release_history: RefCell<crate::runtime::ReleaseHistory>,
    pub(crate) release_history_view: RefCell<Option<gtk::glib::WeakRef<gtk::Box>>>,
    pub(crate) release_notification_toast: RefCell<Option<adw::Toast>>,
    pub(crate) release_updating: RefCell<Option<String>>,
}

impl PreferencesState {
    pub(crate) fn active_dialog(&self) -> Option<adw::Dialog> {
        self.dialog.upgrade()
    }

    pub(crate) fn set_active_dialog(&self, dialog: &adw::Dialog) {
        self.dialog.set(Some(dialog));
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

pub(super) fn row_action_content(title: &str, icon_name: &str) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name(icon_name));
    let label = gtk::Label::new(Some(&tr(title)));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_width_chars(0);
    label.set_max_width_chars(18);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_lines(2);
    content.append(&label);
    content
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
    title_label.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
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
    if shell.source.configured.borrow().sources.is_empty() {
        shell.present_onboarding();
    } else {
        present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, true, false);
    }
}
pub(crate) fn present_downloads_preferences_dialog(shell: &Rc<Shell>) {
    present_preferences_dialog_with_page(shell, PreferencesPageKind::Library, false, true);
}

impl Shell {
    pub(crate) fn set_external_metadata_enabled(self: &Rc<Self>, enabled: bool) {
        if self
            .set_app_setting("metadata setting", enabled, |settings| {
                &mut settings.external_metadata_enabled
            })
            .is_some()
        {
            if enabled {
                self.retry_external_artwork("metadata setting");
            } else {
                self.update_media_controls();
            }
        }
    }

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreferencesTabLayout {
    Top,
    BottomLabels,
    BottomIcons,
}

fn preferences_tab_layout_for(
    width: i32,
    top_tabs_width: i32,
    top_start_width: i32,
    top_end_width: i32,
    bottom_tabs_width: i32,
) -> PreferencesTabLayout {
    const TOP_BAR_HORIZONTAL_PADDING: i32 = 24;
    let top_required = top_tabs_width
        .saturating_add(top_start_width.max(top_end_width).saturating_mul(2))
        .saturating_add(TOP_BAR_HORIZONTAL_PADDING);
    if width >= top_required {
        PreferencesTabLayout::Top
    } else if width >= bottom_tabs_width {
        PreferencesTabLayout::BottomLabels
    } else {
        PreferencesTabLayout::BottomIcons
    }
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
            Self::Playback => "rufin-music-queue-symbolic",
            Self::Library => "rufin-library-music-symbolic",
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
pub(crate) struct PreferencesNavigationControls {
    back: gtk::Button,
    navigation: Rc<gtk::glib::WeakRef<adw::NavigationView>>,
}

impl PreferencesNavigationControls {
    fn new(back: gtk::Button) -> Self {
        back.update_property(&[gtk::accessible::Property::Label(&tr("Back"))]);

        let controls = Self {
            back,
            navigation: Rc::new(gtk::glib::WeakRef::new()),
        };
        let navigation = Rc::clone(&controls.navigation);
        controls.back.connect_clicked(move |button| {
            if let Some(navigation) = navigation.upgrade() {
                navigation.pop();
            }
            button.set_visible(false);
        });
        controls
    }

    pub(crate) fn set_navigation(&self, navigation: &adw::NavigationView) {
        self.navigation.set(Some(navigation));
    }

    pub(crate) fn set_nested_page_visible(&self, visible: bool) {
        self.back.set_visible(visible);
    }

    fn return_to_root(&self) {
        if let Some(navigation) = self.navigation.upgrade() {
            while navigation.pop() {}
        }
        self.back.set_visible(false);
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
        .content_width(large_popup_content_width(LARGE_POPUP_BASE_WIDTH))
        .content_height(large_popup_content_height(
            shell.chrome.window.height(),
            LARGE_POPUP_BASE_HEIGHT,
        ))
        .build();
    dialog.add_css_class("preferences");
    shell.preferences.set_active_dialog(&dialog);
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
    let credential_drafts = Rc::new(RefCell::new(shell.products.scrobbling.preferences()));
    let resource = crate::ui_resource::PREFERENCES_DIALOG_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        toolbar: adw::ToolbarView,
        stack: adw::ViewStack,
        switcher: adw::ViewSwitcher,
        bottom_tab_bar: gtk::CenterBox,
        bottom_switcher: adw::ViewSwitcher,
        top_start_controls: gtk::Box,
        back: gtk::Button,
        search_button: gtk::ToggleButton,
        close_button: gtk::Button,
        switcher_bar: gtk::CenterBox,
        search_entry: gtk::SearchEntry,
        search_bar: gtk::SearchBar,
        search_results: adw::PreferencesPage,
        search_results_group: adw::PreferencesGroup,
        content_stack: gtk::Stack,
    });
    let navigation_controls = PreferencesNavigationControls::new(back);
    search_button.update_property(&[gtk::accessible::Property::Label(&tr("Search"))]);
    close_button.update_property(&[gtk::accessible::Property::Label(&tr("Close"))]);
    let dialog_for_close = dialog.downgrade();
    close_button.connect_clicked(move |_| {
        if let Some(dialog) = dialog_for_close.upgrade() {
            dialog.close();
        }
    });
    toolbar.add_top_bar(&switcher_bar);
    toolbar.add_bottom_bar(&bottom_tab_bar);

    search_bar.connect_entry(&search_entry);
    toolbar.add_top_bar(&search_bar);

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
    let search_navigation = navigation_controls.clone();
    let search_drafts = credential_drafts.clone();
    let search_items_for_button = Rc::clone(&search_items);
    let search_entry_for_button = search_entry.downgrade();
    let search_bar_for_button = search_bar.downgrade();
    let page_stack_for_button = stack.downgrade();
    search_button.connect_toggled(move |button| {
        let Some(search_entry) = search_entry_for_button.upgrade() else {
            return;
        };
        let Some(search_bar) = search_bar_for_button.upgrade() else {
            return;
        };
        let enabled = button.is_active();
        search_bar.set_search_mode(enabled);
        if !enabled {
            search_entry.set_text("");
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
                &search_navigation,
                &search_drafts,
                retained,
            );
            *search_items_for_button.borrow_mut() = items;
        }
        search_entry.grab_focus();
    });

    let search_button_for_bar = search_button.downgrade();
    let content_stack_for_bar = content_stack.downgrade();
    search_bar.connect_search_mode_enabled_notify(move |bar| {
        let Some(search_button) = search_button_for_bar.upgrade() else {
            return;
        };
        search_button.set_active(bar.is_search_mode());
        if !bar.is_search_mode() {
            if let Some(content_stack) = content_stack_for_bar.upgrade() {
                content_stack.set_visible_child_name("pages");
            }
        }
    });

    let search_items_for_entry = Rc::clone(&search_items);
    let rendered_rows_for_entry = Rc::clone(&rendered_search_rows);
    let results_group_for_entry = search_results_group.clone();
    let page_stack_for_entry = stack.downgrade();
    let content_stack_for_entry = content_stack.downgrade();
    let search_bar_for_entry = search_bar.downgrade();
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
            let search_bar = search_bar_for_entry.clone();
            let navigation = navigation_for_entry.clone();
            row.connect_activated(move |_| {
                let Some(page_stack) = page_stack.upgrade() else {
                    return;
                };
                let Some(search_bar) = search_bar.upgrade() else {
                    return;
                };
                navigation.return_to_root();
                page_stack.set_visible_child_name(page.name());
                search_bar.set_search_mode(false);
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

    let top_tabs_width = Rc::new(Cell::new(0));
    let bottom_tabs_min_width = Rc::new(Cell::new(0));
    let applied_tab_layout = Rc::new(Cell::new(None));
    let responsive_switcher = switcher.clone();
    let responsive_bottom_bar = bottom_tab_bar.clone();
    let responsive_bottom_switcher = bottom_switcher.clone();
    let responsive_toolbar_view = toolbar.clone();
    let responsive_top_start = top_start_controls.clone();
    let responsive_top_end = close_button.clone();
    let responsive_top_tabs_width = Rc::clone(&top_tabs_width);
    let responsive_bottom_tabs_min_width = Rc::clone(&bottom_tabs_min_width);
    let responsive_applied_layout = Rc::clone(&applied_tab_layout);
    let responsive_toolbar = width_allocation_owner(&toolbar, move |width| {
        let top_tabs_width =
            cached_widget_natural_width(&responsive_switcher, &responsive_top_tabs_width);
        let bottom_tabs_width = cached_widget_minimum_width(
            &responsive_bottom_switcher,
            &responsive_bottom_tabs_min_width,
        );
        let (_, top_start_width, _, _) =
            responsive_top_start.measure(gtk::Orientation::Horizontal, -1);
        let (_, top_end_width, _, _) = responsive_top_end.measure(gtk::Orientation::Horizontal, -1);
        let layout = preferences_tab_layout_for(
            width,
            top_tabs_width,
            top_start_width,
            top_end_width,
            bottom_tabs_width,
        );
        if responsive_applied_layout.replace(Some(layout)) == Some(layout) {
            return;
        }
        responsive_switcher.set_visible(layout == PreferencesTabLayout::Top);
        responsive_bottom_bar.set_visible(layout != PreferencesTabLayout::Top);
        if layout == PreferencesTabLayout::Top {
            responsive_toolbar_view.remove_css_class("preferences-tabs-bottom");
        } else {
            responsive_toolbar_view.add_css_class("preferences-tabs-bottom");
        }
        if layout == PreferencesTabLayout::BottomIcons {
            responsive_bottom_switcher.add_css_class("preferences-tabs-icon-only");
            responsive_bottom_bar.add_css_class("preferences-tabs-icon-only");
        } else {
            responsive_bottom_switcher.remove_css_class("preferences-tabs-icon-only");
            responsive_bottom_bar.remove_css_class("preferences-tabs-icon-only");
        }
    });
    dialog.set_child(Some(&responsive_toolbar));
}

fn cached_widget_natural_width(widget: &impl IsA<gtk::Widget>, cache: &Cell<i32>) -> i32 {
    let cached = cache.get();
    if cached > 0 {
        return cached;
    }
    let (_, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    cache.set(natural);
    natural
}

fn cached_widget_minimum_width(widget: &impl IsA<gtk::Widget>, cache: &Cell<i32>) -> i32 {
    let cached = cache.get();
    if cached > 0 {
        return cached;
    }
    let (minimum, _, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    cache.set(minimum);
    minimum
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
    shell: &Rc<Shell>,
    dialog: &adw::Dialog,
    page_slots: &[(PreferencesPageKind, gtk::Box)],
    navigation_controls: &PreferencesNavigationControls,
    credential_drafts: &ScrobblingCredentialDrafts,
    retained: PreferencesPageKind,
) -> Vec<PreferencesSearchItem> {
    let mut items = Vec::new();
    for kind in PreferencesPageKind::ALL {
        mount_preferences_page(
            shell,
            dialog,
            page_slots,
            navigation_controls,
            credential_drafts,
            kind,
            false,
            false,
        );
        let Some((_, slot)) = page_slots.iter().find(|(page, _)| *page == kind) else {
            continue;
        };
        materialize_preferences_expanders(slot.upcast_ref());
        collect_preferences_search_items(slot.upcast_ref(), kind, "", None, &mut items);
    }
    mount_preferences_page(
        shell,
        dialog,
        page_slots,
        navigation_controls,
        credential_drafts,
        retained,
        false,
        false,
    );
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
    use super::{
        PreferencesNavigationControls, PreferencesTabLayout, populate_expander_once,
        preferences_search_matches, preferences_tab_layout_for,
    };
    use adw::prelude::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn preference_search_matches_terms_across_row_context() {
        let searchable = "show tray icon app window general";

        assert!(preferences_search_matches(searchable, "tray general"));
        assert!(preferences_search_matches(searchable, "GENERAL ICON"));
        assert!(!preferences_search_matches(searchable, "tray playback"));
    }

    #[test]
    fn preference_tabs_move_then_drop_labels_as_width_contracts() {
        assert_eq!(
            preferences_tab_layout_for(600, 400, 60, 40, 330),
            PreferencesTabLayout::Top
        );
        assert_eq!(
            preferences_tab_layout_for(500, 400, 60, 40, 330),
            PreferencesTabLayout::BottomLabels
        );
        assert_eq!(
            preferences_tab_layout_for(329, 400, 60, 40, 330),
            PreferencesTabLayout::BottomIcons
        );
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn navigation_controls_do_not_retain_their_dialog_widgets() {
        gtk::init().expect("GTK display");
        let back = gtk::Button::new();
        let navigation = adw::NavigationView::new();
        let back_weak = back.downgrade();
        let navigation_weak = navigation.downgrade();
        let controls = PreferencesNavigationControls::new(back);
        controls.set_navigation(&navigation);
        drop((controls, navigation));
        while gtk::glib::MainContext::default().iteration(false) {}
        assert!(back_weak.upgrade().is_none());
        assert!(navigation_weak.upgrade().is_none());
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn collapsed_preferences_expander_builds_only_on_first_open() {
        gtk::init().expect("GTK display");
        let expander = adw::ExpanderRow::new();
        let builds = Rc::new(Cell::new(0));
        let observed = Rc::clone(&builds);
        populate_expander_once(&expander, move |_| observed.set(observed.get() + 1));
        assert_eq!(builds.get(), 0);
        expander.set_expanded(true);
        assert_eq!(builds.get(), 1);
        expander.set_expanded(false);
        expander.set_expanded(true);
        assert_eq!(builds.get(), 1);
    }
}

fn mount_preferences_page(
    shell: &Rc<Shell>,
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
        if *slot_kind == PreferencesPageKind::Library {
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
    let page: gtk::Widget = match kind {
        PreferencesPageKind::General => general_page(shell, dialog).upcast(),
        PreferencesPageKind::Appearance => appearance_page(shell).upcast(),
        PreferencesPageKind::Integrations => scrobbling_page(shell, credential_drafts).upcast(),
        PreferencesPageKind::Playback => playback_page(shell).upcast(),
        PreferencesPageKind::Library => library::library_page(
            shell,
            dialog,
            navigation_controls,
            open_add_server,
            focus_download_queue,
        ),
    };
    slot.append(&page);
}
fn general_page(shell: &Rc<Shell>, dialog: &adw::Dialog) -> adw::PreferencesPage {
    let resource = crate::ui_resource::GENERAL_PREFERENCES_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        page: adw::PreferencesPage,
        language_row: adw::ComboRow,
        window_group: adw::PreferencesGroup,
        tray_row: adw::SwitchRow,
        keep_running_row: adw::SwitchRow,
        start_minimized_row: adw::SwitchRow,
        type_to_search_row: adw::SwitchRow,
        automatic_updates_row: adw::SwitchRow,
        control_notifications_row: adw::SwitchRow,
        notifications_row: adw::SwitchRow,
        release_notifications_row: adw::SwitchRow,
        prefer_server_playlist_row: adw::SwitchRow,
        prefer_distinct_track_covers_row: adw::SwitchRow,
        external_metadata_row: adw::SwitchRow,
        external_lyrics_row: adw::SwitchRow,
        external_links_row: adw::SwitchRow,
        lastfm_links_row: adw::SwitchRow,
        musicbrainz_links_row: adw::SwitchRow,
        server_links_row: adw::SwitchRow,
        presence_row: adw::SwitchRow,
        display_row: adw::ComboRow,
        link_row: adw::ComboRow,
        paused_row: adw::SwitchRow,
        listening_row: adw::SwitchRow,
        private_row: adw::SwitchRow,
        cast_proxy_row: adw::SwitchRow,
        cast_network_row: adw::ActionRow,
        secret_storage_row: adw::ComboRow,
    });
    let settings = shell.settings.current.borrow().clone();
    let language_options = Rc::new(language_options());
    let language_titles = language_options
        .iter()
        .map(|option| option.title.as_str())
        .collect::<Vec<_>>();
    let language_model = gtk::StringList::new(&language_titles);
    language_row.set_model(Some(&language_model));
    language_row.set_selected(language_option_index(
        language_options.as_ref(),
        &settings.language,
    ));
    let language_shell = Rc::clone(shell);
    language_row.connect_selected_notify(move |row| {
        let Some(option) = language_options.get(row.selected() as usize) else {
            return;
        };
        language_shell.set_app_setting("language setting", option.id.clone(), |settings| {
            &mut settings.language
        });
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
            type_to_search_shell.set_app_setting(
                "type to search setting",
                row.is_active(),
                |settings| &mut settings.type_to_search_enabled,
            );
        });
        if shell
            .preferences
            .release_history
            .borrow()
            .automatic_updates_supported
        {
            automatic_updates_row.set_active(settings.automatic_updates_enabled);
            let automatic_updates_shell = Rc::clone(shell);
            automatic_updates_row.connect_active_notify(move |row| {
                automatic_updates_shell.set_app_setting(
                    "automatic update setting",
                    row.is_active(),
                    |settings| &mut settings.automatic_updates_enabled,
                );
            });
        } else {
            window_group.remove(&automatic_updates_row);
        }
    }

    control_notifications_row.set_active(settings.control_notifications_enabled);
    let control_notifications_shell = Rc::clone(shell);
    control_notifications_row.connect_active_notify(move |row| {
        control_notifications_shell.set_app_setting(
            "control notification setting",
            row.is_active(),
            |settings| &mut settings.control_notifications_enabled,
        );
    });
    notifications_row.set_active(settings.notifications_enabled);
    let notifications_shell = Rc::clone(shell);
    notifications_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        if notifications_shell
            .set_app_setting("notification setting", enabled, |settings| {
                &mut settings.notifications_enabled
            })
            .is_some()
            && !enabled
        {
            notifications_shell.withdraw_now_playing_notification();
        }
    });
    release_notifications_row.set_active(settings.release_notifications_enabled);
    let release_notifications_shell = Rc::clone(shell);
    release_notifications_row.connect_active_notify(move |row| {
        release_notifications_shell.set_app_setting(
            "release notification setting",
            row.is_active(),
            |settings| &mut settings.release_notifications_enabled,
        );
    });
    prefer_server_playlist_row.set_active(settings.prefer_server_playlist_covers);
    let prefer_server_playlist_shell = Rc::clone(shell);
    prefer_server_playlist_row.connect_active_notify(move |row| {
        if prefer_server_playlist_shell
            .set_app_setting("playlist cover setting", row.is_active(), |settings| {
                &mut settings.prefer_server_playlist_covers
            })
            .is_some()
        {
            prefer_server_playlist_shell.reconcile_mounted_route();
            crate::routes::playlist_picker::refresh_context_playlist_picker(
                &prefer_server_playlist_shell,
            );
        }
    });

    prefer_distinct_track_covers_row.set_active(settings.prefer_distinct_track_covers);
    let distinct_track_covers_shell = Rc::clone(shell);
    prefer_distinct_track_covers_row.connect_active_notify(move |row| {
        if distinct_track_covers_shell
            .set_app_setting("track cover setting", row.is_active(), |settings| {
                &mut settings.prefer_distinct_track_covers
            })
            .is_some()
        {
            if let Some(source) = distinct_track_covers_shell.selected_source_operations() {
                source.refresh_library(crate::runtime::LibraryRefreshTrigger::GlobalAction);
            }
        }
    });

    external_metadata_row.set_active(settings.external_metadata_enabled);
    let metadata_shell = Rc::clone(shell);
    external_metadata_row.connect_active_notify(move |row| {
        metadata_shell.set_external_metadata_enabled(row.is_active());
    });

    external_lyrics_row.set_active(settings.lyrics.external_lyrics_enabled);
    let weak = Rc::downgrade(shell);
    external_lyrics_row.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_external_lyrics_enabled(row.is_active());
        }
    });

    let external_links = settings.external_site_links.clone();
    external_links_row.set_active(external_links.enabled);
    lastfm_links_row.set_active(external_links.lastfm);
    lastfm_links_row.set_sensitive(external_links.enabled);
    musicbrainz_links_row.set_active(external_links.musicbrainz);
    musicbrainz_links_row.set_sensitive(external_links.enabled);
    server_links_row.set_active(external_links.server);
    server_links_row.set_sensitive(external_links.enabled);
    let external_links_shell = Rc::clone(shell);
    let lastfm_links_for_master = lastfm_links_row.clone();
    let musicbrainz_links_for_master = musicbrainz_links_row.clone();
    let server_links_for_master = server_links_row.clone();
    external_links_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        lastfm_links_for_master.set_sensitive(enabled);
        musicbrainz_links_for_master.set_sensitive(enabled);
        server_links_for_master.set_sensitive(enabled);
        if external_links_shell
            .set_app_setting("external site links setting", enabled, |settings| {
                &mut settings.external_site_links.enabled
            })
            .is_some()
        {
            external_links_shell.reconcile_mounted_route();
        }
    });
    let lastfm_links_shell = Rc::clone(shell);
    lastfm_links_row.connect_active_notify(move |row| {
        if lastfm_links_shell
            .set_app_setting("Last.fm site links setting", row.is_active(), |settings| {
                &mut settings.external_site_links.lastfm
            })
            .is_some()
        {
            lastfm_links_shell.reconcile_mounted_route();
        }
    });
    let musicbrainz_links_shell = Rc::clone(shell);
    musicbrainz_links_row.connect_active_notify(move |row| {
        if musicbrainz_links_shell
            .set_app_setting(
                "MusicBrainz site links setting",
                row.is_active(),
                |settings| &mut settings.external_site_links.musicbrainz,
            )
            .is_some()
        {
            musicbrainz_links_shell.reconcile_mounted_route();
        }
    });
    let server_links_shell = Rc::clone(shell);
    server_links_row.connect_active_notify(move |row| {
        if server_links_shell
            .set_app_setting("server site links setting", row.is_active(), |settings| {
                &mut settings.external_site_links.server
            })
            .is_some()
        {
            server_links_shell.reconcile_mounted_route();
        }
    });
    presence_row.set_active(settings.rich_presence.enabled);
    let presence_shell = Rc::clone(shell);
    presence_row.connect_active_notify(move |row| {
        presence_shell.set_app_setting("Discord presence setting", row.is_active(), |settings| {
            &mut settings.rich_presence.enabled
        });
    });
    let display_titles = [tr("Application name"), tr("Song title"), tr("Artist name")];
    let display_refs = display_titles
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let display_options = gtk::StringList::new(&display_refs);
    display_row.set_model(Some(&display_options));
    display_row.set_selected(discord_display_index(settings.rich_presence.display_type));
    let display_shell = Rc::clone(shell);
    display_row.connect_selected_notify(move |row| {
        display_shell.set_app_setting(
            "Discord display setting",
            discord_display_from_index(row.selected()),
            |settings| &mut settings.rich_presence.display_type,
        );
    });
    let link_titles = [
        tr("None"),
        tr("Last.fm"),
        tr("MusicBrainz"),
        tr("MusicBrainz + Last.fm"),
    ];
    let link_refs = link_titles.iter().map(String::as_str).collect::<Vec<_>>();
    let link_options = gtk::StringList::new(&link_refs);
    link_row.set_model(Some(&link_options));
    link_row.set_selected(discord_link_index(settings.rich_presence.link_type));
    let link_shell = Rc::clone(shell);
    link_row.connect_selected_notify(move |row| {
        link_shell.set_app_setting(
            "Discord link setting",
            discord_link_from_index(row.selected()),
            |settings| &mut settings.rich_presence.link_type,
        );
    });
    paused_row.set_active(settings.rich_presence.show_paused);
    let paused_shell = Rc::clone(shell);
    paused_row.connect_active_notify(move |row| {
        paused_shell.set_app_setting("Discord paused setting", row.is_active(), |settings| {
            &mut settings.rich_presence.show_paused
        });
    });
    listening_row.set_active(settings.rich_presence.show_as_listening);
    let listening_shell = Rc::clone(shell);
    listening_row.connect_active_notify(move |row| {
        listening_shell.set_app_setting(
            "Discord activity type setting",
            row.is_active(),
            |settings| &mut settings.rich_presence.show_as_listening,
        );
    });
    private_row.set_active(settings.private_mode);
    let private_shell = Rc::clone(shell);
    private_row.connect_active_notify(move |row| {
        private_shell.set_private_mode(row.is_active());
    });
    cast_proxy_row.set_active(settings.cast_proxy_enabled);
    let cast_proxy_shell = Rc::clone(shell);
    cast_proxy_row.connect_active_notify(move |row| {
        cast_proxy_shell.set_app_setting("cast proxy setting", row.is_active(), |settings| {
            &mut settings.cast_proxy_enabled
        });
    });
    let cast_network_dropdown = casting_network_dropdown(shell, 220);
    cast_network_row.add_suffix(&cast_network_dropdown);
    cast_network_row.set_activatable_widget(Some(&cast_network_dropdown));

    let secret_storage_titles = [tr("Legacy"), tr("Secure storage")];
    let secret_storage_refs = secret_storage_titles
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    secret_storage_row.set_model(Some(&gtk::StringList::new(&secret_storage_refs)));
    secret_storage_row.set_selected(secret_storage_mode_index(settings.secret_storage_mode));
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
fn populate_layout_group(
    shell: &Rc<Shell>,
    group: &adw::PreferencesGroup,
    lyrics_panel_row: adw::SwitchRow,
    visualizer_panel_row: adw::SwitchRow,
    bottom_bar_rating_row: adw::SwitchRow,
    narrow_row: adw::SwitchRow,
    threshold_row: adw::SpinRow,
) {
    let settings = shell.settings.current.borrow().clone();

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

    lyrics_panel_row.set_active(settings.lyrics_panel_visible);
    let lyrics_panel_shell = Rc::clone(shell);
    lyrics_panel_row.connect_active_notify(move |row| {
        lyrics_panel_shell.set_lyrics_panel_visible(row.is_active());
    });
    group.add(&lyrics_panel_row);

    visualizer_panel_row.set_active(settings.visualizer_panel_visible);
    let visualizer_panel_shell = Rc::clone(shell);
    visualizer_panel_row.connect_active_notify(move |row| {
        visualizer_panel_shell.set_visualizer_panel_visible(row.is_active());
    });
    group.add(&visualizer_panel_row);

    bottom_bar_rating_row.set_active(settings.show_bottom_bar_rating);
    let bottom_bar_rating_shell = Rc::clone(shell);
    bottom_bar_rating_row.connect_active_notify(move |row| {
        if bottom_bar_rating_shell
            .set_app_setting("bottom bar rating setting", row.is_active(), |settings| {
                &mut settings.show_bottom_bar_rating
            })
            .is_some()
        {
            bottom_bar_rating_shell.update_bottom_player();
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
}
fn configure_sidebar_items_expander(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    pins: &adw::SwitchRow,
) {
    pins.set_active(shell.settings.current.borrow().sidebar.pins_visible);
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
    let shell = Rc::clone(shell);
    let pins = pins.clone();
    populate_expander_once(&expander, move |expander| {
        populate_sidebar_item_rows(&shell, expander, &rows, &pins);
    });
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
    let descriptor = entry.item.descriptor();
    let visible_shell = Rc::clone(shell);
    let visible_expander = expander.downgrade();
    let visible_rows = Rc::clone(rows);
    let visible_pins = pins.clone();
    let on_visible = move |is_visible| {
        visible_shell.update_app_settings("sidebar setting", |settings| {
            let Some(stored) = settings
                .sidebar
                .route_items
                .iter_mut()
                .find(|stored| stored.item == entry.item)
            else {
                return false;
            };
            if stored.visible == is_visible {
                return false;
            }
            stored.visible = is_visible;
            settings.sidebar.sanitize();
            true
        });
        visible_shell.rebuild_sidebar_navigation();
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
            drop_shell.rebuild_sidebar_navigation();
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

fn open_folder(shell: &Rc<Shell>, folder: gtk::gio::File) {
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
                .launch_future(Some(&shell.chrome.window))
                .await
        }
        .await;
        if let Err(error) = result {
            if !error.matches(gtk::DialogError::Dismissed)
                && !error.matches(gtk::DialogError::Cancelled)
            {
                shell.show_feedback_toast(localization::tr_with(
                    "Could not open folder: {error}",
                    &[("error", &error.to_string())],
                ));
            }
        }
    });
}
