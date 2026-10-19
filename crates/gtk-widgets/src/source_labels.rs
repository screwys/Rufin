pub use rufin_core::runtime::source::{source_kind_icon_name, source_kind_title};

use adw::prelude::*;
use app_identity::{DISPLAY_NAME, STABLE_APP_ID};
use gtk::gio;
use localization::{tr, tr_with};
use rufin_core::runtime::source::SourceSummary;
use sources::SourceId;
use std::rc::Rc;

pub fn source_icon_pixel_size(icon: &str, size: i32) -> i32 {
    if icon == STABLE_APP_ID {
        size + 4
    } else {
        size
    }
}

pub fn source_display_label(source: Option<&SourceSummary>) -> (&'static str, String) {
    match source {
        Some(source) => (
            configured_source_icon_name(source),
            configured_source_display_name(source),
        ),
        None => (STABLE_APP_ID, DISPLAY_NAME.to_string()),
    }
}

pub fn configured_source_display_name(source: &SourceSummary) -> String {
    let name = source.name.trim();
    if name.is_empty() {
        source_kind_title(&source.kind).map_or_else(|| source.kind.clone(), tr)
    } else {
        name.to_string()
    }
}
pub fn configured_source_icon_name(source: &SourceSummary) -> &'static str {
    source_kind_icon_name(&source.kind).unwrap_or("rufin-network-server-symbolic")
}
pub fn configure_ownership_toggle(
    button: &gtk::ToggleButton,
    source: Option<&SourceSummary>,
    belongs_to_source: bool,
) {
    let source_icon = source.map(configured_source_icon_name);
    let source_name = source.map(configured_source_display_name);
    button.set_sensitive(source.is_some());
    button.set_active(belongs_to_source && source.is_some());
    let update = move |button: &gtk::ToggleButton| {
        let current = button.is_active() && source_icon.is_some();
        button.set_icon_name(if current {
            source_icon.unwrap_or(STABLE_APP_ID)
        } else {
            STABLE_APP_ID
        });
        if current {
            button.remove_css_class("rufin-ownership-icon");
        } else {
            button.add_css_class("rufin-ownership-icon");
        }
        let tooltip = if current {
            tr_with(
                "This is owned by {source}. Regular playlists are synced to your remote.",
                &[("source", source_name.as_deref().unwrap_or(DISPLAY_NAME))],
            )
        } else {
            tr(
                "This is owned by Rufin. It can include entries from all configured sources but it is not synced to remotes.",
            )
        };
        button.set_tooltip_text(Some(&tooltip));
    };
    update(button);
    button.connect_toggled(update);
}

pub fn source_menu_item(label: &str, action: &str, icon_name: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(action));
    item.set_icon(&gio::ThemedIcon::new(icon_name));
    item
}

pub fn choice_action(name: &str, active: bool, select: impl Fn() + 'static) -> gio::SimpleAction {
    let action = gio::SimpleAction::new_stateful(name, None, &active.to_variant());
    action.connect_activate(move |action, _| {
        action.set_state(&true.to_variant());
        select();
    });
    action
}

pub fn source_order(sources: &[SourceSummary], selected: Option<&SourceId>) -> Vec<usize> {
    let mut order = Vec::with_capacity(sources.len());
    if let Some(selected) = selected
        && let Some(index) = sources.iter().position(|source| &source.id == selected)
    {
        order.push(index);
    }
    order.extend(
        sources
            .iter()
            .enumerate()
            .filter_map(|(index, source)| (Some(&source.id) != selected).then_some(index)),
    );
    order
}

pub fn bind_source_picker(
    button: &gtk::MenuButton,
    sources: Rc<[SourceSummary]>,
    selected: u32,
    include_all: bool,
) -> gtk::SingleSelection {
    let resource = crate::ui_resource::SOURCE_PICKER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, { content: gtk::Box, icon: gtk::Image, label: gtk::Label });
    button.set_child(Some(&content));
    let mut titles = Vec::new();
    if include_all {
        titles.push(tr("All"));
    }
    titles.extend(sources.iter().map(configured_source_display_name));
    let selection = gtk::SingleSelection::new(Some(gtk::StringList::new(
        &titles.iter().map(String::as_str).collect::<Vec<_>>(),
    )));
    selection.set_selected(selected);
    button.set_sensitive(!sources.is_empty());
    let update = {
        let sources = sources.clone();
        let icon = icon.downgrade();
        let label = label.downgrade();
        move |selection: &gtk::SingleSelection| {
            let (Some(icon), Some(label)) = (icon.upgrade(), label.upgrade()) else {
                return;
            };
            if include_all && selection.selected() == 0 {
                icon.set_icon_name(Some("rufin-library-symbolic"));
                label.set_label(&tr("All"));
            } else if let Some(source) = selection
                .selected()
                .checked_sub(u32::from(include_all))
                .and_then(|index| sources.get(index as usize))
            {
                let name = configured_source_icon_name(source);
                icon.set_icon_name(Some(name));
                icon.set_pixel_size(source_icon_pixel_size(name, 20));
                label.set_label(&configured_source_display_name(source));
            }
        }
    };
    update(&selection);
    selection.connect_selected_notify(update);
    let available = !sources.is_empty();
    let selected = selection.clone();
    button.set_create_popup_func(move |button| {
        let menu = gio::Menu::new();
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        let actions = gio::SimpleActionGroup::new();
        let current = selected.selected();
        let count = sources.len() + usize::from(include_all);
        let order = (0..count)
            .filter(|index| *index as u32 == current)
            .chain((0..count).filter(|index| *index as u32 != current));
        for index in order {
            let (title, icon) = if include_all && index == 0 {
                (tr("All"), "rufin-library-symbolic")
            } else {
                let source = &sources[index - usize::from(include_all)];
                (
                    configured_source_display_name(source),
                    configured_source_icon_name(source),
                )
            };
            let name = format!("source-{index}");
            let selection = selected.clone();
            let close = popover.downgrade();
            let action = choice_action(&name, index as u32 == current, move || {
                selection.set_selected(index as u32);
                if let Some(popover) = close.upgrade() {
                    popover.popdown();
                }
            });
            actions.add_action(&action);
            menu.append_item(&source_menu_item(&title, &format!("sources.{name}"), icon));
        }
        popover.insert_action_group("sources", Some(&actions));
        button.set_popover(Some(&popover));
        crate::interactions::show_native_menu_icons(&popover);
        crate::interactions::replace_native_menu_checkmarks(&popover);
    });
    button.set_sensitive(available);
    selection
}
