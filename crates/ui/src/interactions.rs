use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use localization::{msgid, tr};

use crate::settings::{ContextMenuItem, ContextMenuSettings};
use crate::shell::Shell;
use crate::shell::actions::{PLAY_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON};

const NATIVE_MENU_SELECTION_CLASS: &str = "rufin-menu-selection";
const NATIVE_MENU_SELECTED_CLASS: &str = "rufin-menu-selected";
const NATIVE_MENU_PARENT_GRAB_CLASS: &str = "rufin-menu-parent-grab";
const CONTEXT_MENU_MAX_WIDTH: i32 = 230;
const CONTEXT_MENU_CASCADE_GAP: i32 = 8;
pub(crate) const CONTEXT_MENU_HOVER_OWNER_CLASS: &str = "context-menu-hover-owner";
pub(crate) const CONTEXT_MENU_HOVER_HELD_CLASS: &str = "context-menu-hover-held";

pub(crate) const ADD_TO_PLAYLIST_ICON: &str = "rufin-playlists-compact-symbolic";
pub(crate) const ALBUM_ICON: &str = "rufin-albums-symbolic";
pub(crate) const ARTIST_ICON: &str = "rufin-artists-symbolic";
pub(crate) const DOWNLOAD_ICON: &str = "rufin-download-symbolic";
pub(crate) const GO_TO_ICON: &str = "rufin-external-link-compact-symbolic";
pub(crate) const RADIO_ICON: &str = "rufin-audio-radio-symbolic";

pub(crate) type ContextMenuOpen = Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>)>;

pub(crate) fn connect_transient_entry_focus_dismissal(shell: &Shell) {
    install_focus_dismissal(
        &shell.chrome.window,
        vec![
            shell.right_panel.queue_search.clone().upcast(),
            shell.right_panel.lyrics_host.clone().upcast(),
            shell
                .player_view
                .fullscreen_player
                .lyrics_host
                .clone()
                .upcast(),
        ],
    );
}

fn install_focus_dismissal(window: &gtk::ApplicationWindow, targets: Vec<gtk::Widget>) {
    let click_root = window.clone();
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Denied);
        let Some(focus) = gtk::prelude::RootExt::focus(&click_root) else {
            return;
        };
        let Some(target) = targets
            .iter()
            .find(|target| target.has_focus() || focus.is_ancestor(*target))
        else {
            return;
        };
        if target.compute_bounds(&click_root).is_none_or(|bounds| {
            bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
        }) {
            return;
        }
        if let Some(root) = target.root() {
            root.set_focus(None::<&gtk::Widget>);
        }
    });
    window.add_controller(click);
}

pub(crate) struct ContextMenuSurface {
    target: gtk::Widget,
    group_name: &'static str,
    menu: gio::Menu,
    popover: gtk::PopoverMenu,
    actions: gio::SimpleActionGroup,
    position: Option<(f64, f64)>,
    entries: RefCell<Vec<ContextMenuEntry<gio::MenuItem>>>,
    custom_children: RefCell<Vec<ContextMenuCustomChild>>,
}

struct ContextMenuCustomChild {
    setting: Option<ContextMenuItem>,
    id: String,
    widget: gtk::Widget,
    primary_click: Option<Rc<dyn Fn()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContextMenuEntry<T> {
    Configurable(ContextMenuItem, T),
    Fixed(T),
}

impl ContextMenuSurface {
    pub(crate) fn new(
        target: &gtk::Widget,
        group_name: &'static str,
        position: Option<(f64, f64)>,
    ) -> Self {
        let menu = gio::Menu::new();
        Self {
            target: target.clone(),
            group_name,
            popover: context_popover(target, position, &menu),
            menu,
            actions: gio::SimpleActionGroup::new(),
            position,
            entries: RefCell::new(Vec::new()),
            custom_children: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn popover(&self) -> &gtk::PopoverMenu {
        &self.popover
    }

    pub(crate) fn append_fixed_action(&self, label: &str, action: &str, icon_name: &str) {
        self.entries
            .borrow_mut()
            .push(ContextMenuEntry::Fixed(menu_action_item(
                &tr(label),
                &format!("{}.{}", self.group_name, action),
                icon_name,
            )));
    }

    pub(crate) fn append_fixed_widget(&self, id: &str, widget: &impl IsA<gtk::Widget>) {
        let item = gio::MenuItem::new(None, None);
        item.set_attribute_value("custom", Some(&id.to_variant()));
        self.entries
            .borrow_mut()
            .push(ContextMenuEntry::Fixed(item));
        self.custom_children
            .borrow_mut()
            .push(ContextMenuCustomChild {
                setting: None,
                id: id.to_string(),
                widget: widget.as_ref().clone(),
                primary_click: None,
            });
    }

    pub(crate) fn append_configurable_action(
        &self,
        item: ContextMenuItem,
        label: &str,
        action: &str,
        icon_name: &str,
    ) {
        self.entries
            .borrow_mut()
            .push(ContextMenuEntry::Configurable(
                item,
                menu_action_item(
                    &tr(label),
                    &format!("{}.{}", self.group_name, action),
                    icon_name,
                ),
            ));
    }

    pub(crate) fn append_configurable_submenu(
        &self,
        setting: ContextMenuItem,
        label: &str,
        submenu: &gio::Menu,
        icon_name: &str,
    ) {
        let item = gio::MenuItem::new_submenu(Some(&tr(label)), submenu);
        item.set_icon(&gio::ThemedIcon::new(icon_name));
        self.entries
            .borrow_mut()
            .push(ContextMenuEntry::Configurable(setting, item));
    }

    pub(crate) fn append_configurable_widget_submenu(
        &self,
        setting: ContextMenuItem,
        label: &str,
        id: &str,
        widget: &impl IsA<gtk::Widget>,
        icon_name: &str,
        primary_click: impl Fn() + 'static,
    ) {
        let submenu = gio::Menu::new();
        let custom = gio::MenuItem::new(None, None);
        custom.set_attribute_value("custom", Some(&id.to_variant()));
        submenu.append_item(&custom);
        self.append_configurable_submenu(setting, label, &submenu, icon_name);
        self.custom_children
            .borrow_mut()
            .push(ContextMenuCustomChild {
                setting: Some(setting),
                id: id.to_string(),
                widget: widget.as_ref().clone(),
                primary_click: Some(Rc::new(primary_click)),
            });
    }

    pub(crate) fn add_action(&self, name: &str, run: impl Fn() + 'static) {
        self.add_action_enabled(name, true, run);
    }

    pub(crate) fn add_action_enabled(&self, name: &str, enabled: bool, run: impl Fn() + 'static) {
        let action = gio::SimpleAction::new(name, None);
        action.set_enabled(enabled);
        let popover = self.popover.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(popover) = popover.upgrade() {
                popdown_native_menu(&popover);
            }
            run();
        });
        self.actions.add_action(&action);
    }

    pub(crate) fn popup(self, settings: &ContextMenuSettings) {
        for item in resolve_context_menu_entries(self.entries.into_inner(), settings) {
            self.menu.append_item(&item);
        }
        if context_menu_needs_sliding_submenus(&self.popover, &self.target, self.position) {
            self.popover.set_flags(gtk::PopoverMenuFlags::empty());
        }
        for child in self
            .custom_children
            .into_inner()
            .into_iter()
            .filter(|child| child.setting.is_none_or(|item| settings.is_visible(item)))
        {
            match (
                add_native_menu_child(&self.popover, &child.widget, &child.id),
                child.primary_click,
            ) {
                (Ok(Some(owner)), Some(primary_click)) => {
                    install_native_submenu_primary_click(&owner, &self.popover, primary_click);
                }
                (Ok(_), _) => {}
                (Err(()), _) => tracing::error!(
                    custom_child = child.id,
                    "context menu custom child has no matching menu placeholder"
                ),
            }
        }
        if self.menu.n_items() == 0 {
            self.popover.unparent();
            return;
        }
        self.target
            .insert_action_group(self.group_name, Some(&self.actions));
        let hover_owners = Rc::new(context_menu_hover_owners(&self.target));
        for owner in hover_owners.iter() {
            owner.add_css_class(CONTEXT_MENU_HOVER_HELD_CLASS);
        }
        show_native_menu_icons(&self.popover);
        keep_parent_grab_for_nested_native_menus(&self.popover);
        let unmap_handler = Rc::new(RefCell::new(Some(popdown_on_anchor_unmap(
            &self.target,
            &self.popover,
        ))));
        // Keep the anchor alive until its manually parented popover is detached.
        let target = self.target.clone();
        self.popover.connect_closed(move |popover| {
            popdown_nested_native_menus_from(popover.upcast_ref());
            let popover = popover.clone();
            let target = target.clone();
            let unmap_handler = Rc::clone(&unmap_handler);
            let hover_owners = Rc::clone(&hover_owners);
            glib::idle_add_local_once(move || {
                if let Some(handler) = unmap_handler.borrow_mut().take() {
                    target.disconnect(handler);
                }
                popover.unparent();
                for owner in hover_owners.iter() {
                    owner.remove_css_class(CONTEXT_MENU_HOVER_HELD_CLASS);
                }
            });
        });
        self.popover.popup();
    }
}

fn context_menu_hover_owners(target: &gtk::Widget) -> Vec<gtk::Widget> {
    fn append_descendants(widget: &gtk::Widget, owners: &mut Vec<gtk::Widget>) {
        if widget.has_css_class(CONTEXT_MENU_HOVER_OWNER_CLASS)
            && !owners.iter().any(|owner| owner == widget)
        {
            owners.push(widget.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            append_descendants(&widget, owners);
        }
    }

    let mut owners = Vec::new();
    let mut ancestor = Some(target.clone());
    while let Some(widget) = ancestor {
        ancestor = widget.parent();
        if widget.has_css_class(CONTEXT_MENU_HOVER_OWNER_CLASS)
            && !owners.iter().any(|owner| owner == &widget)
        {
            owners.push(widget);
        }
    }
    if owners.is_empty() {
        append_descendants(target, &mut owners);
    }
    owners
}

fn resolve_context_menu_entries<T>(
    entries: Vec<ContextMenuEntry<T>>,
    settings: &ContextMenuSettings,
) -> Vec<T> {
    fn append_segment<T>(
        resolved: &mut Vec<T>,
        segment: &mut Vec<(ContextMenuItem, T)>,
        settings: &ContextMenuSettings,
    ) {
        segment.retain(|(item, _)| settings.is_visible(*item));
        segment.sort_by_key(|(item, _)| settings.position(*item));
        resolved.extend(segment.drain(..).map(|(_, value)| value));
    }

    let mut resolved = Vec::with_capacity(entries.len());
    let mut segment = Vec::new();
    for entry in entries {
        match entry {
            ContextMenuEntry::Configurable(item, value) => segment.push((item, value)),
            ContextMenuEntry::Fixed(value) => {
                append_segment(&mut resolved, &mut segment, settings);
                resolved.push(value);
            }
        }
    }
    append_segment(&mut resolved, &mut segment, settings);
    resolved
}

#[cfg(test)]
mod context_menu_tests {
    use super::{
        ContextMenuEntry, context_menu_has_submenu_side, go_to_context_submenu,
        resolve_context_menu_entries,
    };
    use crate::settings::{ContextMenuItem, ContextMenuItemSettings, ContextMenuSettings};
    use gio::prelude::MenuModelExt;
    use glib::prelude::IsA;
    use gtk::{gio, glib};

    fn settings(order: &[(ContextMenuItem, bool)]) -> ContextMenuSettings {
        ContextMenuSettings {
            items: order
                .iter()
                .map(|(item, visible)| ContextMenuItemSettings {
                    item: *item,
                    visible: *visible,
                })
                .collect(),
            rating_visible: true,
        }
    }

    #[test]
    fn configurable_items_follow_saved_order_and_visibility() {
        let settings = settings(&[
            (ContextMenuItem::Download, true),
            (ContextMenuItem::Play, false),
            (ContextMenuItem::Favorites, true),
        ]);
        let entries = vec![
            ContextMenuEntry::Configurable(ContextMenuItem::Play, "play"),
            ContextMenuEntry::Configurable(ContextMenuItem::Favorites, "favorite"),
            ContextMenuEntry::Configurable(ContextMenuItem::Download, "download"),
            ContextMenuEntry::Configurable(ContextMenuItem::Download, "remove-downloads"),
        ];

        assert_eq!(
            resolve_context_menu_entries(entries, &settings),
            ["download", "remove-downloads", "favorite"]
        );
    }

    #[test]
    fn fixed_items_anchor_configurable_segments() {
        let settings = settings(&[
            (ContextMenuItem::Download, true),
            (ContextMenuItem::Play, true),
            (ContextMenuItem::Favorites, true),
        ]);
        let entries = vec![
            ContextMenuEntry::Configurable(ContextMenuItem::Play, "play"),
            ContextMenuEntry::Configurable(ContextMenuItem::Favorites, "favorite"),
            ContextMenuEntry::Fixed("remove"),
            ContextMenuEntry::Configurable(ContextMenuItem::Download, "download"),
        ];

        assert_eq!(
            resolve_context_menu_entries(entries, &settings),
            ["play", "favorite", "remove", "download"]
        );
    }

    #[test]
    fn hidden_configurable_items_can_resolve_to_an_empty_menu() {
        let settings = settings(&[(ContextMenuItem::Play, false)]);
        let entries = vec![ContextMenuEntry::Configurable(
            ContextMenuItem::Play,
            "play",
        )];

        assert!(resolve_context_menu_entries(entries, &settings).is_empty());
    }

    #[test]
    fn side_submenus_require_one_complete_contiguous_side() {
        assert!(context_menu_has_submenu_side(988, 494.0, 276, 301));
        assert!(context_menu_has_submenu_side(600, 50.0, 276, 301));
        assert!(!context_menu_has_submenu_side(600, 300.0, 276, 301));
        assert!(!context_menu_has_submenu_side(490, 245.0, 276, 230));
    }

    #[test]
    fn go_to_submenu_flattens_named_artists_and_keeps_album_destination() {
        let menu = go_to_context_submenu(
            "track",
            &["First Artist".to_string(), "Second Artist".to_string()],
            true,
        );
        assert_eq!(menu.n_items(), 3);
        assert_eq!(menu_label(&menu, 0).as_deref(), Some("Go to First Artist"));
        assert_eq!(menu_action(&menu, 0).as_deref(), Some("track.go-artist-0"));
        assert_eq!(menu_label(&menu, 1).as_deref(), Some("Go to Second Artist"));
        assert_eq!(menu_action(&menu, 1).as_deref(), Some("track.go-artist-1"));
        assert_eq!(menu_action(&menu, 2).as_deref(), Some("track.go-album"));
    }

    fn menu_label(model: &impl IsA<gio::MenuModel>, index: i32) -> Option<String> {
        model
            .item_attribute_value(index, "label", Some(glib::VariantTy::STRING))
            .and_then(|value| value.str().map(str::to_string))
    }

    fn menu_action(model: &impl IsA<gio::MenuModel>, index: i32) -> Option<String> {
        model
            .item_attribute_value(index, "action", Some(glib::VariantTy::STRING))
            .and_then(|value| value.str().map(str::to_string))
    }
}

fn menu_action_item(label: &str, action: &str, icon_name: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    item.set_icon(&gio::ThemedIcon::new(icon_name));
    item
}

fn append_menu_action(menu: &gio::Menu, label: &str, action: &str, icon_name: &str) {
    menu.append_item(&menu_action_item(&tr(label), action, icon_name));
}

pub(crate) fn radio_context_submenu(group: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    append_menu_action(&menu, "Play", &format!("{group}.play-radio"), PLAY_ICON);
    append_menu_action(
        &menu,
        "Play Next",
        &format!("{group}.play-radio-next"),
        PLAY_NEXT_ICON,
    );
    append_menu_action(
        &menu,
        "Play Later",
        &format!("{group}.play-radio-last"),
        PLAY_LATER_ICON,
    );
    menu
}

pub(crate) fn go_to_context_submenu(
    group: &str,
    artist_names: &[String],
    has_album: bool,
) -> gio::Menu {
    let menu = gio::Menu::new();
    for (index, artist_name) in artist_names.iter().enumerate() {
        append_menu_action(
            &menu,
            &format!("{} {artist_name}", tr("Go to")),
            &if artist_names.len() == 1 {
                format!("{group}.go-artist")
            } else {
                format!("{group}.go-artist-{index}")
            },
            ARTIST_ICON,
        );
    }
    if has_album {
        append_menu_action(
            &menu,
            msgid("Go to Album"),
            &format!("{group}.go-album"),
            ALBUM_ICON,
        );
    }
    menu
}

pub(crate) fn show_native_menu_icons(popover: &gtk::PopoverMenu) {
    show_native_menu_icons_from(popover.upcast_ref());
}

pub(crate) fn replace_native_menu_checkmarks(popover: &gtk::PopoverMenu) {
    replace_native_menu_checkmarks_from(popover.upcast_ref());
}

pub(crate) fn keep_parent_grab_for_nested_native_menus(popover: &gtk::PopoverMenu) {
    keep_parent_grab_for_nested_native_menus_from(popover.upcast_ref());
}

pub(crate) fn keep_parent_grab_for_dropdown(dropdown: &gtk::DropDown) {
    let mut child = dropdown.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(popover) = widget.downcast::<gtk::Popover>() {
            // GTK can lose the parent's input grab when an autohide child closes.
            popover.set_autohide(false);
            break;
        }
    }
}

pub(crate) fn popdown_native_menu(popover: &gtk::PopoverMenu) {
    popdown_nested_native_menus_from(popover.upcast_ref());
    popover.popdown();
}

fn add_native_menu_child(
    popover: &gtk::PopoverMenu,
    child: &gtk::Widget,
    id: &str,
) -> Result<Option<gtk::Widget>, ()> {
    if let Some(owner) = add_nested_native_menu_child_from(popover.upcast_ref(), child, id) {
        return Ok(Some(owner));
    }
    popover.add_child(child, id).then_some(None).ok_or(())
}

fn add_nested_native_menu_child_from(
    container: &gtk::Widget,
    child: &gtk::Widget,
    id: &str,
) -> Option<gtk::Widget> {
    let mut widget = container.first_child();
    while let Some(current) = widget {
        widget = current.next_sibling();
        if current.type_().name() == "GtkModelButton"
            && let Some(nested) = current
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
        {
            if nested.add_child(child, id) {
                return Some(current);
            }
            if let Some(owner) = add_nested_native_menu_child_from(nested.upcast_ref(), child, id) {
                return Some(owner);
            }
        }
        if let Some(owner) = add_nested_native_menu_child_from(&current, child, id) {
            return Some(owner);
        }
    }
    None
}

fn install_native_submenu_primary_click(
    owner: &gtk::Widget,
    popover: &gtk::PopoverMenu,
    run: Rc<dyn Fn()>,
) {
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let popover = popover.downgrade();
    click.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(popover) = popover.upgrade() {
            popdown_native_menu(&popover);
        }
        run();
    });
    owner.add_controller(click);
}

pub(crate) fn popdown_on_anchor_unmap(
    anchor: &impl IsA<gtk::Widget>,
    popover: &impl IsA<gtk::Popover>,
) -> glib::SignalHandlerId {
    let popover = popover.as_ref().downgrade();
    anchor.as_ref().connect_unmap(move |_| {
        if let Some(popover) = popover.upgrade() {
            popdown_popover(&popover);
        }
    })
}

fn popdown_popover(popover: &gtk::Popover) {
    if let Ok(menu) = popover.clone().downcast::<gtk::PopoverMenu>() {
        popdown_native_menu(&menu);
    } else {
        popover.popdown();
    }
}

fn popdown_nested_native_menus_from(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();

        if widget.type_().name() == "GtkModelButton"
            && let Some(nested_popover) = widget
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
        {
            popdown_native_menu(&nested_popover);
        }
        popdown_nested_native_menus_from(&widget);
    }
}

fn keep_parent_grab_for_nested_native_menus_from(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();

        if widget.type_().name() == "GtkModelButton"
            && let Some(nested_popover) = widget
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
        {
            // Keep the parent menu's modal grab while its native child is open.
            nested_popover.set_autohide(false);
            nested_popover.set_width_request(CONTEXT_MENU_MAX_WIDTH);
            if !widget.has_css_class(NATIVE_MENU_PARENT_GRAB_CLASS) {
                widget.add_css_class(NATIVE_MENU_PARENT_GRAB_CLASS);
                let nested_popover = nested_popover.downgrade();
                widget.connect_unmap(move |_| {
                    if let Some(nested_popover) = nested_popover.upgrade() {
                        popdown_native_menu(&nested_popover);
                    }
                });
            }
        }
        keep_parent_grab_for_nested_native_menus_from(&widget);
    }
}

fn replace_native_menu_checkmarks_from(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();

        if widget.type_().name() == "GtkModelButton" {
            if generated_menu_button_has_starting_space(&widget) {
                hide_native_menu_indicator_box(&widget);
            }
            if generated_menu_button_is_selection(&widget) {
                style_native_menu_selection(&widget);
            }
            if let Some(nested_popover) = widget
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
            {
                replace_native_menu_checkmarks(&nested_popover);
            }
        }
        replace_native_menu_checkmarks_from(&widget);
    }
}

fn generated_menu_button_has_starting_space(widget: &gtk::Widget) -> bool {
    let role = widget.property_value("role");
    glib::EnumValue::from_value(&role).is_some_and(|(_, role)| role.nick() != "title")
}

fn generated_menu_button_is_selection(widget: &gtk::Widget) -> bool {
    if widget.type_().name() != "GtkModelButton" {
        return false;
    }

    let role = widget.property_value("role");
    glib::EnumValue::from_value(&role)
        .is_some_and(|(_, role)| matches!(role.nick(), "check" | "radio"))
}

fn style_native_menu_selection(button: &gtk::Widget) {
    if button.has_css_class(NATIVE_MENU_SELECTION_CLASS) {
        return;
    }
    button.add_css_class(NATIVE_MENU_SELECTION_CLASS);

    update_native_menu_selection(button);
    button.connect_notify_local(Some("active"), |button, _| {
        update_native_menu_selection(button);
    });
}

fn hide_native_menu_indicator_box(button: &gtk::Widget) {
    let mut child = button.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.type_().name() == "GtkBox" {
            widget.set_visible(false);
            break;
        }
    }
}

fn update_native_menu_selection(button: &gtk::Widget) {
    if button.property::<bool>("active") {
        button.add_css_class(NATIVE_MENU_SELECTED_CLASS);
    } else {
        button.remove_css_class(NATIVE_MENU_SELECTED_CLASS);
    }
}

fn show_native_menu_icons_from(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();

        if widget.type_().name() == "GtkModelButton" {
            show_native_menu_button_icon(&widget);
            if let Some(nested_popover) = widget
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
            {
                show_native_menu_icons(&nested_popover);
            }
        }
        show_native_menu_icons_from(&widget);
    }
}

fn show_native_menu_button_icon(button: &gtk::Widget) {
    let mut child = button.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();

        if let Ok(image) = widget.clone().downcast::<gtk::Image>() {
            image.set_visible(true);
            image.set_margin_end(8);
        } else if let Ok(label) = widget.downcast::<gtk::Label>() {
            label.set_hexpand(true);
        }
    }
}

fn context_popover(
    target: &gtk::Widget,
    position: Option<(f64, f64)>,
    menu: &gio::Menu,
) -> gtk::PopoverMenu {
    let popover = gtk::PopoverMenu::from_model_full(menu, gtk::PopoverMenuFlags::NESTED);
    popover.set_autohide(true);
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_width_request(CONTEXT_MENU_MAX_WIDTH);
    popover.set_parent(target);
    if let Some((x, y)) = position {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    }
    popover
}

fn context_menu_needs_sliding_submenus(
    popover: &gtk::PopoverMenu,
    target: &gtk::Widget,
    position: Option<(f64, f64)>,
) -> bool {
    let Some((anchor_x, window_width)) = context_menu_anchor(target, position) else {
        return false;
    };
    let root_width = popover
        .measure(gtk::Orientation::Horizontal, -1)
        .1
        .max(CONTEXT_MENU_MAX_WIDTH);
    let submenu_width = widest_nested_menu_width(popover.upcast_ref());
    submenu_width > 0
        && !context_menu_has_submenu_side(
            window_width,
            anchor_x,
            root_width,
            submenu_width.max(CONTEXT_MENU_MAX_WIDTH),
        )
}

fn context_menu_anchor(target: &gtk::Widget, position: Option<(f64, f64)>) -> Option<(f32, i32)> {
    let (x, y) = position?;
    let root = target.root()?;
    let point = target.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))?;
    Some((point.x(), root.width()))
}

fn widest_nested_menu_width(container: &gtk::Widget) -> i32 {
    let mut widest = 0;
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.type_().name() == "GtkModelButton"
            && let Some(nested) = widget
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
        {
            widest = widest.max(
                nested
                    .measure(gtk::Orientation::Horizontal, -1)
                    .1
                    .max(CONTEXT_MENU_MAX_WIDTH),
            );
        }
        widest = widest.max(widest_nested_menu_width(&widget));
    }
    widest
}

fn context_menu_has_submenu_side(
    window_width: i32,
    anchor_x: f32,
    root_width: i32,
    submenu_width: i32,
) -> bool {
    if window_width <= 0 || root_width <= 0 || submenu_width <= 0 {
        return false;
    }
    let root_x =
        (anchor_x.round() as i32 - root_width / 2).clamp(0, (window_width - root_width).max(0));
    let left = root_x;
    let right = (window_width - root_x - root_width).max(0);
    left.max(right) >= submenu_width + CONTEXT_MENU_CASCADE_GAP
}

pub(crate) fn install_context_menu_openers(target: &impl IsA<gtk::Widget>, open: ContextMenuOpen) {
    let target = target.as_ref();
    let target_weak = target.downgrade();
    let click_open = Rc::clone(&open);
    let click = gtk::GestureClick::new();
    click.set_button(3);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |click, _, x, y| {
        click.set_state(gtk::EventSequenceState::Claimed);
        if let Some(target) = target_weak.upgrade() {
            click_open(&target, Some((x, y)));
        }
    });
    target.add_controller(click);

    let long_open = Rc::clone(&open);
    let target_weak = target.downgrade();
    let press = gtk::GestureLongPress::new();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    press.connect_pressed(move |press, x, y| {
        press.set_state(gtk::EventSequenceState::Claimed);
        if let Some(target) = target_weak.upgrade() {
            long_open(&target, Some((x, y)));
        }
    });
    target.add_controller(press);
}

pub(crate) fn add_widget_click(target: &gtk::Widget, callback: impl Fn() + 'static) {
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.connect_released(move |gesture, press_count, _, _| {
        if press_count == 1 {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            callback();
        }
    });
    target.add_controller(click);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_menu_actions_store_their_icon() {
        let item = menu_action_item("Play", "track.play", PLAY_ICON);
        let serialized = item
            .attribute_value("icon", None)
            .expect("menu item should have an icon");
        let icon = gio::Icon::deserialize(&serialized).expect("menu icon should deserialize");

        assert!(icon.equal(Some(&gio::ThemedIcon::new(PLAY_ICON))));
    }
}
