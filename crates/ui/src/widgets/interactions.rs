use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use localization::{msgid, tr};

use crate::widgets::controls::{PLAY_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON};
use rufin_core::menu::Menu;
use rufin_core::playback::QueuePlacement;
use rufin_core::settings::context_menu::ContextMenuItem;

const NATIVE_MENU_SELECTION_CLASS: &str = "rufin-menu-selection";
const NATIVE_MENU_SELECTED_CLASS: &str = "rufin-menu-selected";
const NATIVE_MENU_PARENT_GRAB_CLASS: &str = "rufin-menu-parent-grab";
const CONTEXT_MENU_MAX_WIDTH: i32 = 230;
const CONTEXT_MENU_CASCADE_GAP: i32 = 8;
pub const CONTEXT_MENU_HOVER_OWNER_CLASS: &str = "context-menu-hover-owner";
pub const CONTEXT_MENU_HOVER_HELD_CLASS: &str = "context-menu-hover-held";

pub const ADD_TO_PLAYLIST_ICON: &str = "rufin-playlists-compact-symbolic";
pub const ALBUM_ICON: &str = "rufin-albums-symbolic";
pub const ARTIST_ICON: &str = "rufin-artists-symbolic";
pub const DOWNLOAD_ICON: &str = "rufin-download-symbolic";
pub const GO_TO_ICON: &str = "rufin-external-link-compact-symbolic";
pub const RADIO_ICON: &str = "rufin-audio-only-symbolic";

#[derive(Default)]
struct ButtonRepeat(RefCell<Option<glib::JoinHandle<()>>>);

impl ButtonRepeat {
    fn stop(&self) {
        if let Some(task) = self.0.borrow_mut().take() {
            task.abort();
        }
    }
}

impl Drop for ButtonRepeat {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn connect_repeating_button(button: &gtk::Button, activate: impl Fn() + 'static) {
    let activate: Rc<dyn Fn()> = Rc::new(activate);
    let clicked = Rc::clone(&activate);
    button.connect_clicked(move |_| clicked());

    let repeat = Rc::new(ButtonRepeat::default());
    let press = gtk::GestureLongPress::new();
    press.set_button(1);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let pressed_repeat = Rc::clone(&repeat);
    let weak_button = button.downgrade();
    press.connect_pressed(move |gesture, _, _| {
        pressed_repeat.stop();
        // Claim the hold so releasing it does not also produce a normal click.
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let button = weak_button.clone();
        let activate = Rc::clone(&activate);
        pressed_repeat
            .0
            .replace(Some(glib::spawn_future_local(async move {
                loop {
                    {
                        let Some(button) = button.upgrade() else {
                            break;
                        };
                        if !button.is_mapped() || !button.is_sensitive() {
                            break;
                        }
                        activate();
                    }
                    glib::timeout_future(std::time::Duration::from_millis(100)).await;
                }
            })));
    });
    let ended = Rc::clone(&repeat);
    press.connect_end(move |_, _| ended.stop());
    let cancelled = Rc::clone(&repeat);
    press.connect_cancel(move |_, _| cancelled.stop());
    let cancelled = Rc::clone(&repeat);
    press.connect_cancelled(move |_| cancelled.stop());
    button.add_controller(press);

    let motion = gtk::EventControllerMotion::new();
    let left = Rc::clone(&repeat);
    motion.connect_leave(move |_| left.stop());
    button.add_controller(motion);
    button.connect_unmap(move |_| repeat.stop());
}

pub type ContextMenuOpen = Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>)>;

pub struct ContextMenuSurface {
    target: gtk::Widget,
    group_name: &'static str,
    menu: gio::Menu,
    popover: gtk::PopoverMenu,
    actions: gio::SimpleActionGroup,
    action_activated: Rc<Cell<bool>>,
    position: Option<(f64, f64)>,
    entries: RefCell<Vec<(ContextMenuSection, gio::MenuItem)>>,
    custom_children: RefCell<Vec<ContextMenuCustomChild>>,
    /// Submenu rows whose body runs an action, by label: Play and the radio.
    submenu_actions: RefCell<Vec<(String, String)>>,
    next_action: Cell<usize>,
}

struct ContextMenuCustomChild {
    id: String,
    widget: gtk::Widget,
    primary_click: Option<Rc<dyn Fn()>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ContextMenuSection {
    Playback,
    Collection,
    Details,
    Downloads,
    Fixed,
}

impl ContextMenuSection {
    fn for_item(item: ContextMenuItem) -> Self {
        match item {
            ContextMenuItem::Play
            | ContextMenuItem::PlayNext
            | ContextMenuItem::PlayLater
            | ContextMenuItem::PlayRadio => Self::Playback,
            ContextMenuItem::AddToPlaylist | ContextMenuItem::Favorites => Self::Collection,
            ContextMenuItem::EditMetadata | ContextMenuItem::Pins | ContextMenuItem::GoTo => {
                Self::Details
            }
            ContextMenuItem::Download => Self::Downloads,
        }
    }
}

impl ContextMenuSurface {
    pub fn new(
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
            action_activated: Rc::new(Cell::new(false)),
            position,
            entries: RefCell::new(Vec::new()),
            custom_children: RefCell::new(Vec::new()),
            submenu_actions: RefCell::new(Vec::new()),
            next_action: Cell::new(0),
        }
    }

    pub fn popover(&self) -> &gtk::PopoverMenu {
        &self.popover
    }

    /// The Play submenu: Play, Play Next and Play Later as `menu` offers them, each also
    /// shuffled. Its row body plays.
    pub fn append_play_actions(&self, menu: &Menu, run: impl Fn(QueuePlacement, bool) + 'static) {
        let placements = [
            (menu.play, QueuePlacement::Now, PLAY_ICON),
            (menu.play_next, QueuePlacement::Next, PLAY_NEXT_ICON),
            (menu.play_later, QueuePlacement::Last, PLAY_LATER_ICON),
        ];
        if !placements.iter().any(|(shown, _, _)| *shown) {
            return;
        }
        let run = Rc::new(run);
        let submenu = gio::Menu::new();
        for (shuffled, labels) in [
            (
                false,
                [msgid("Play"), msgid("Play Next"), msgid("Play Later")],
            ),
            (
                true,
                [
                    msgid("Play (shuffled)"),
                    msgid("Play Next (shuffled)"),
                    msgid("Play Later (shuffled)"),
                ],
            ),
        ] {
            let section = gio::Menu::new();
            for ((shown, placement, icon), label) in placements.iter().zip(labels) {
                if *shown {
                    let run = Rc::clone(&run);
                    let placement = *placement;
                    let action = self.action(true, move || run(placement, shuffled));
                    if placement == QueuePlacement::Now && !shuffled {
                        self.submenu_default(&tr(msgid("Play")), &action);
                    }
                    section.append_item(&menu_action_item(&tr(label), &action, icon));
                }
            }
            submenu.append_section(None, &section);
        }
        let item = gio::MenuItem::new_submenu(Some(&tr(msgid("Play"))), &submenu);
        item.set_icon(&gio::ThemedIcon::new(PLAY_ICON));
        self.entries
            .borrow_mut()
            .push((ContextMenuSection::Playback, item));
    }

    /// The radio submenu: Play, Play Next and Play Later of a radio. Its row body plays it.
    pub fn append_radio(&self, label: &str, run: impl Fn(QueuePlacement) + 'static) {
        let run = Rc::new(run);
        let submenu = gio::Menu::new();
        for (placement, title, icon) in [
            (QueuePlacement::Now, msgid("Play"), PLAY_ICON),
            (QueuePlacement::Next, msgid("Play Next"), PLAY_NEXT_ICON),
            (QueuePlacement::Last, msgid("Play Later"), PLAY_LATER_ICON),
        ] {
            let run = Rc::clone(&run);
            let action = self.action(true, move || run(placement));
            if placement == QueuePlacement::Now {
                self.submenu_default(&tr(label), &action);
            }
            submenu.append_item(&menu_action_item(&tr(title), &action, icon));
        }
        self.append_configurable_submenu(ContextMenuItem::PlayRadio, label, &submenu, RADIO_ICON);
    }

    /// An item, for a submenu, that runs `run`. `label` is already translated.
    pub fn item(&self, label: &str, icon_name: &str, run: impl Fn() + 'static) -> gio::MenuItem {
        menu_action_item(label, &self.action(true, run), icon_name)
    }

    pub fn append_fixed_action(&self, label: &str, icon_name: &str, run: impl Fn() + 'static) {
        self.append_fixed_action_enabled(label, icon_name, true, run);
    }

    pub fn append_fixed_action_enabled(
        &self,
        label: &str,
        icon_name: &str,
        enabled: bool,
        run: impl Fn() + 'static,
    ) {
        let item = menu_action_item(&tr(label), &self.action(enabled, run), icon_name);
        self.entries
            .borrow_mut()
            .push((ContextMenuSection::Fixed, item));
    }

    /// An action under a generated name that closes the menu before it runs. Returns its
    /// detailed name in this menu's group.
    fn action(&self, enabled: bool, run: impl Fn() + 'static) -> String {
        let name = format!("item-{}", self.next_action.get());
        self.next_action.set(self.next_action.get() + 1);
        let action = gio::SimpleAction::new(&name, None);
        action.set_enabled(enabled);
        let popover = self.popover.downgrade();
        let action_activated = Rc::clone(&self.action_activated);
        action.connect_activate(move |_, _| {
            action_activated.set(true);
            if let Some(popover) = popover.upgrade() {
                popdown_native_menu(&popover);
            }
            run();
        });
        self.actions.add_action(&action);
        format!("{}.{name}", self.group_name)
    }

    /// The submenu row titled `label` runs `action` when its body is clicked.
    fn submenu_default(&self, label: &str, action: &str) {
        self.submenu_actions
            .borrow_mut()
            .push((label.to_owned(), action.to_owned()));
    }

    pub fn append_fixed_widget(&self, id: &str, widget: &impl IsA<gtk::Widget>) {
        let item = gio::MenuItem::new(None, None);
        item.set_attribute_value("custom", Some(&id.to_variant()));
        self.entries
            .borrow_mut()
            .push((ContextMenuSection::Fixed, item));
        self.custom_children
            .borrow_mut()
            .push(ContextMenuCustomChild {
                id: id.to_string(),
                widget: widget.as_ref().clone(),
                primary_click: None,
            });
    }

    pub fn append_configurable_action(
        &self,
        item: ContextMenuItem,
        label: &str,
        icon_name: &str,
        run: impl Fn() + 'static,
    ) {
        let entry = menu_action_item(&tr(label), &self.action(true, run), icon_name);
        self.entries
            .borrow_mut()
            .push((ContextMenuSection::for_item(item), entry));
    }

    pub fn append_configurable_submenu(
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
            .push((ContextMenuSection::for_item(setting), item));
    }

    pub fn append_fixed_widget_submenu(
        &self,
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
        let item = gio::MenuItem::new_submenu(Some(&tr(label)), &submenu);
        item.set_icon(&gio::ThemedIcon::new(icon_name));
        self.entries
            .borrow_mut()
            .push((ContextMenuSection::Fixed, item));
        self.custom_children
            .borrow_mut()
            .push(ContextMenuCustomChild {
                id: id.to_string(),
                widget: widget.as_ref().clone(),
                primary_click: Some(Rc::new(primary_click)),
            });
    }

    pub fn append_configurable_widget_submenu(
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
                id: id.to_string(),
                widget: widget.as_ref().clone(),
                primary_click: Some(Rc::new(primary_click)),
            });
    }

    pub fn popup(self) {
        let mut previous = None;
        let mut section = gio::Menu::new();
        for (group, item) in self.entries.into_inner() {
            if previous.is_some_and(|previous| previous != group) {
                self.menu.append_section(None, &section);
                section = gio::Menu::new();
            }
            section.append_item(&item);
            previous = Some(group);
        }
        if section.n_items() > 0 {
            self.menu.append_section(None, &section);
        }
        if context_menu_needs_sliding_submenus(&self.popover, &self.target, self.position) {
            self.popover.set_flags(gtk::PopoverMenuFlags::empty());
        }
        for child in self.custom_children.into_inner() {
            match (
                add_native_menu_child(&self.popover, &child.widget, &child.id),
                child.primary_click,
            ) {
                (Ok(Some(owner)), Some(primary_click)) => {
                    let action_activated = Rc::clone(&self.action_activated);
                    install_native_submenu_primary_click(
                        &owner,
                        &self.popover,
                        Rc::new(move || {
                            action_activated.set(true);
                            primary_click();
                        }),
                    );
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
        install_submenu_actions(
            self.popover.upcast_ref(),
            &self.actions,
            &self.submenu_actions.into_inner(),
        );
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
        let action_activated = self.action_activated;
        self.popover.connect_closed(move |popover| {
            popdown_nested_native_menus_from(popover.upcast_ref(), None);
            let popover = popover.clone();
            let target = target.clone();
            let unmap_handler = Rc::clone(&unmap_handler);
            let hover_owners = Rc::clone(&hover_owners);
            let action_activated = Rc::clone(&action_activated);
            glib::idle_add_local_once(move || {
                // GTK closes menu buttons before activating their actions.
                // Outside dismissal also needs to close the enclosing search popup.
                if !action_activated.get() {
                    let _ = target.activate_action("context-menu.dismiss", None);
                }
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

#[cfg(test)]
mod context_menu_tests {
    use super::context_menu_has_submenu_side;

    #[test]
    fn side_submenus_require_one_complete_contiguous_side() {
        assert!(context_menu_has_submenu_side(988, 494.0, 276, 301));
        assert!(context_menu_has_submenu_side(600, 15.0, 276, 301));
        assert!(!context_menu_has_submenu_side(600, 16.0, 276, 301));
        assert!(!context_menu_has_submenu_side(600, 50.0, 276, 301));
        assert!(!context_menu_has_submenu_side(600, 300.0, 276, 301));
        assert!(context_menu_has_submenu_side(600, 590.0, 276, 301));
        assert!(!context_menu_has_submenu_side(490, 245.0, 276, 230));
    }
}

fn menu_action_item(label: &str, action: &str, icon_name: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    item.set_icon(&gio::ThemedIcon::new(icon_name));
    item
}

pub fn show_native_menu_icons(popover: &gtk::PopoverMenu) {
    show_native_menu_icons_from(popover.upcast_ref());
}

pub fn replace_native_menu_checkmarks(popover: &gtk::PopoverMenu) {
    replace_native_menu_checkmarks_from(popover.upcast_ref());
}

pub fn keep_parent_grab_for_nested_native_menus(popover: &gtk::PopoverMenu) {
    keep_parent_grab_for_nested_native_menus_from(popover.upcast_ref());
}

pub fn keep_parent_grab_for_dropdown(dropdown: &gtk::DropDown) {
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

pub fn popdown_native_menu(popover: &gtk::PopoverMenu) {
    popdown_nested_native_menus_from(popover.upcast_ref(), None);
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

fn install_submenu_actions(
    container: &gtk::Widget,
    actions: &gio::SimpleActionGroup,
    defaults: &[(String, String)],
) {
    let mut child = container.first_child();
    while let Some(owner) = child {
        child = owner.next_sibling();
        if owner.type_().name() == "GtkModelButton"
            && generated_menu_button_has_starting_space(&owner)
            && (owner.property::<Option<gtk::Popover>>("popover").is_some()
                || owner.property::<Option<String>>("menu-name").is_some())
            && let Some((_, action)) = defaults
                .iter()
                .find(|(label, _)| *label == owner.property::<String>("text"))
        {
            let actions = actions.clone();
            let action = action
                .split_once('.')
                .map_or(action.as_str(), |(_, name)| name)
                .to_owned();
            let click = owner
                .observe_controllers()
                .iter::<glib::Object>()
                .find_map(|controller| controller.unwrap().downcast::<gtk::GestureClick>().ok())
                .expect("native menu button has no click gesture")
                .downgrade();
            owner.connect_local("clicked", false, move |values| {
                // GTK already opens the submenu. Only the row body runs its default action.
                if let Some(click) = click.upgrade()
                    && let Some((x, y)) = click.current_event().and_then(|event| event.position())
                {
                    let owner = values[0].get::<gtk::Widget>().unwrap();
                    let native = owner.native().unwrap();
                    let (native_x, native_y) = native.surface_transform();
                    let point = native
                        .compute_point(
                            &owner,
                            &gtk::graphene::Point::new(
                                (x - native_x) as f32,
                                (y - native_y) as f32,
                            ),
                        )
                        .unwrap();
                    let mut child = owner.first_child();
                    while let Some(widget) = child {
                        child = widget.next_sibling();
                        if widget.css_name() == "arrow"
                            && widget
                                .compute_bounds(&owner)
                                .is_some_and(|bounds| bounds.contains_point(&point))
                        {
                            return None;
                        }
                    }
                }
                actions.activate_action(&action, None);
                None
            });
        }
        install_submenu_actions(&owner, actions, defaults);
    }
}

pub fn popdown_on_anchor_unmap(
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

fn popdown_nested_native_menus_from(container: &gtk::Widget, keep_open: Option<&gtk::PopoverMenu>) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();

        if widget.type_().name() == "GtkModelButton"
            && let Some(nested_popover) = widget
                .property::<Option<gtk::Popover>>("popover")
                .and_then(|popover| popover.downcast::<gtk::PopoverMenu>().ok())
        {
            if Some(&nested_popover) == keep_open {
                continue;
            }
            popdown_native_menu(&nested_popover);
        }
        popdown_nested_native_menus_from(&widget, keep_open);
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
                nested_popover.connect_map(|popover| {
                    // Non-modal submenus must still close the previous sibling on arrow navigation.
                    if let Some(parent) = popover
                        .parent()
                        .and_then(|owner| owner.ancestor(gtk::PopoverMenu::static_type()))
                    {
                        popdown_nested_native_menus_from(&parent, Some(popover));
                    }
                });
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

pub fn align_popup_in_window(popover: &impl IsA<gtk::Popover>) {
    let popover = popover.as_ref();
    let Some(parent) = popover.parent() else {
        return;
    };
    let Some(root) = parent.root() else { return };
    let Some(bounds) = parent.compute_bounds(&root) else {
        return;
    };
    let (has_anchor, anchor) = popover.pointing_to();
    let anchor = if has_anchor {
        anchor
    } else {
        gtk::gdk::Rectangle::new(0, 0, parent.width(), parent.height())
    };
    let width = popover.measure(gtk::Orientation::Horizontal, -1).1;
    let height = popover.measure(gtk::Orientation::Vertical, width).1;
    let x = bounds.x() as i32 + anchor.x();
    let top = bounds.y() as i32 + anchor.y();
    let bottom = top + anchor.height();
    let left = if popover.direction() == gtk::TextDirection::Rtl {
        x + anchor.width() - width
    } else {
        x
    };
    let y = if bottom + height > root.height() && top > root.height() - bottom {
        popover.set_position(gtk::PositionType::Top);
        top - height
    } else {
        popover.set_position(gtk::PositionType::Bottom);
        bottom
    };
    popover.set_halign(gtk::Align::Start);
    let max_left = (root.width() - width).max(0);
    let popup_left = left.clamp(0, max_left);
    popover.set_offset(
        popup_left - left,
        y.clamp(0, (root.height() - height).max(0)) - y,
    );
}

pub fn align_topbar_popup(popover: &impl IsA<gtk::Popover>) {
    let popover = popover.as_ref();
    let Some(parent) = popover.parent() else {
        return;
    };
    let Some(root) = parent.root() else { return };
    let Some(bounds) = parent.compute_bounds(&root) else {
        return;
    };
    let right = bounds.x() + bounds.width() / 2.0 >= root.width() as f32 / 2.0;
    let rtl = popover.direction() == gtk::TextDirection::Rtl;
    popover.set_halign(if right != rtl {
        gtk::Align::End
    } else {
        gtk::Align::Start
    });
    // Keep the arrow aimed at the button while aligning the body to the window edge.
    popover.set_pointing_to(None);
    popover.set_position(gtk::PositionType::Bottom);
    let offset = if right {
        root.width() - (bounds.x() + bounds.width()).round() as i32
    } else {
        -(bounds.x().round() as i32)
    };
    popover.set_offset(offset, 0);
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
    popover.set_halign(gtk::Align::Start);
    popover.connect_show(align_popup_in_window);
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
    let anchor_x = if popover.direction() == gtk::TextDirection::Rtl {
        anchor_x - root_width as f32
    } else {
        anchor_x
    };
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
    let root_x = (anchor_x.round() as i32).clamp(0, (window_width - root_width).max(0));
    let left = root_x;
    let right = (window_width - root_x - root_width).max(0);
    left.max(right) >= submenu_width + CONTEXT_MENU_CASCADE_GAP
}

pub fn install_context_menu_openers(target: &impl IsA<gtk::Widget>, open: ContextMenuOpen) {
    let target = target.as_ref();
    if let Some(actions) = target.downcast_ref::<crate::widgets::recycled_cells::RowActions>() {
        let menu_open = Rc::clone(&open);
        actions
            .menu()
            .connect_clicked(move |button| menu_open(button.upcast_ref(), None));
    }
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

pub fn add_widget_click(target: &gtk::Widget, callback: impl Fn() + 'static) {
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
