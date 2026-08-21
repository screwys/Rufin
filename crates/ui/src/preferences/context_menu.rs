use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use localization::{msgid, tr};

use super::layout::visibility_position_subtitle;
use crate::settings::{ContextMenuItem, ContextMenuItemSettings};
use crate::shell::Shell;

pub(crate) fn context_menus_expander(shell: &Rc<Shell>) -> adw::ExpanderRow {
    let expander = adw::ExpanderRow::builder()
        .title(tr("Context Menus"))
        .expanded(false)
        .build();
    let rows = Rc::new(RefCell::new(
        Vec::<gtk::glib::WeakRef<adw::ActionRow>>::new(),
    ));
    populate_context_menu_rows(shell, &expander, &rows);
    expander
}

fn context_menu_rating_row(shell: &Rc<Shell>) -> adw::ActionRow {
    let visible = gtk::Switch::builder()
        .active(shell.settings.current.borrow().context_menu.rating_visible)
        .valign(gtk::Align::Center)
        .build();
    let row = adw::ActionRow::builder().title(tr(msgid("Rating"))).build();
    row.add_suffix(&visible);
    row.set_activatable_widget(Some(&visible));

    let shell = Rc::clone(shell);
    visible.connect_active_notify(move |switch| {
        let is_visible = switch.is_active();
        shell.update_app_settings("context menu rating setting", |settings| {
            if settings.context_menu.rating_visible == is_visible {
                return false;
            }
            settings.context_menu.rating_visible = is_visible;
            true
        });
    });
    row
}

fn populate_context_menu_rows(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        if let Some(row) = row.upgrade() {
            expander.remove(&row);
        }
    }

    let items = shell.settings.current.borrow().context_menu.items.clone();
    for (position, entry) in items.into_iter().enumerate() {
        let row = context_menu_item_row(shell, expander, rows, entry, position);
        expander.add_row(&row);
        rows.borrow_mut().push(row.downgrade());
    }
    let rating = context_menu_rating_row(shell);
    expander.add_row(&rating);
    rows.borrow_mut().push(rating.downgrade());
}

fn context_menu_item_row(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
    entry: ContextMenuItemSettings,
    position: usize,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(tr(context_menu_item_title(entry.item)))
        .subtitle(visibility_position_subtitle(entry.visible, position))
        .build();

    let drag = gtk::Image::from_icon_name("rufin-list-drag-handle-symbolic");
    drag.add_css_class("dim-label");
    drag.set_tooltip_text(Some(&tr("Drag to reorder")));
    row.add_prefix(&drag);

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

    let visible = gtk::Switch::builder()
        .active(entry.visible)
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&visible);
    row.set_activatable_widget(Some(&visible));

    {
        let shell = Rc::clone(shell);
        let expander = expander.downgrade();
        let rows = Rc::clone(rows);
        visible.connect_active_notify(move |switch| {
            let is_visible = switch.is_active();
            shell.update_app_settings("context menu setting", |settings| {
                let Some(stored) = settings
                    .context_menu
                    .items
                    .iter_mut()
                    .find(|stored| stored.item == entry.item)
                else {
                    return false;
                };
                if stored.visible == is_visible {
                    return false;
                }
                stored.visible = is_visible;
                true
            });
            let Some(expander) = expander.upgrade() else {
                return;
            };
            populate_context_menu_rows(&shell, &expander, &rows);
        });
    }
    {
        let shell = Rc::clone(shell);
        let expander = expander.downgrade();
        let rows = Rc::clone(rows);
        up.connect_clicked(move |_| {
            move_context_menu_item(&shell, entry.item, -1);
            let Some(expander) = expander.upgrade() else {
                return;
            };
            populate_context_menu_rows(&shell, &expander, &rows);
        });
    }
    {
        let shell = Rc::clone(shell);
        let expander = expander.downgrade();
        let rows = Rc::clone(rows);
        down.connect_clicked(move |_| {
            move_context_menu_item(&shell, entry.item, 1);
            let Some(expander) = expander.upgrade() else {
                return;
            };
            populate_context_menu_rows(&shell, &expander, &rows);
        });
    }

    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    let item_id = context_menu_item_drag_id(entry.item).to_string();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&item_id.to_value()))
    });
    drag.add_controller(source);

    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let shell = Rc::clone(shell);
    let expander = expander.downgrade();
    let rows = Rc::downgrade(rows);
    let row_for_drop = row.downgrade();
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(source_id) = value.get::<String>() else {
            return false;
        };
        let Some(source_item) = context_menu_item_from_drag_id(&source_id) else {
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
            .update_app_settings("context menu setting", |settings| {
                reorder_context_menu_items(
                    &mut settings.context_menu.items,
                    source_item,
                    entry.item,
                    after,
                )
            })
            .is_some();
        if changed {
            populate_context_menu_rows(&shell, &expander, &rows);
        }
        changed
    });
    row.add_controller(drop_target);

    row
}

fn move_context_menu_item(shell: &Rc<Shell>, item: ContextMenuItem, delta: isize) {
    shell.update_app_settings("context menu setting", |settings| {
        let Some(index) = settings
            .context_menu
            .items
            .iter()
            .position(|entry| entry.item == item)
        else {
            return false;
        };
        let new_index = if delta < 0 {
            index.saturating_sub(1)
        } else {
            (index + 1).min(settings.context_menu.items.len().saturating_sub(1))
        };
        if index == new_index {
            return false;
        }
        settings.context_menu.items.swap(index, new_index);
        true
    });
}

fn reorder_context_menu_items(
    items: &mut Vec<ContextMenuItemSettings>,
    source: ContextMenuItem,
    target: ContextMenuItem,
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

fn context_menu_item_title(item: ContextMenuItem) -> &'static str {
    match item {
        ContextMenuItem::Play => msgid("Play"),
        ContextMenuItem::PlayNext => msgid("Play Next"),
        ContextMenuItem::PlayLater => msgid("Play Later"),
        ContextMenuItem::PlayRadio => msgid("Play radio"),
        ContextMenuItem::AddToPlaylist => msgid("Add to Playlist"),
        ContextMenuItem::Favorites => msgid("Favorites"),
        ContextMenuItem::EditMetadata => msgid("Edit metadata"),
        ContextMenuItem::Pins => msgid("Pins"),
        ContextMenuItem::GoTo => msgid("Go to"),
        ContextMenuItem::Download => msgid("Download"),
    }
}

fn context_menu_item_drag_id(item: ContextMenuItem) -> &'static str {
    match item {
        ContextMenuItem::Play => "Play",
        ContextMenuItem::PlayNext => "PlayNext",
        ContextMenuItem::PlayLater => "PlayLater",
        ContextMenuItem::PlayRadio => "PlayRadio",
        ContextMenuItem::AddToPlaylist => "AddToPlaylist",
        ContextMenuItem::Favorites => "Favorites",
        ContextMenuItem::EditMetadata => "EditMetadata",
        ContextMenuItem::Pins => "Pins",
        ContextMenuItem::GoTo => "GoTo",
        ContextMenuItem::Download => "Download",
    }
}

fn context_menu_item_from_drag_id(id: &str) -> Option<ContextMenuItem> {
    ContextMenuItem::all()
        .into_iter()
        .find(|item| context_menu_item_drag_id(*item) == id)
}

#[cfg(test)]
mod tests {
    use super::reorder_context_menu_items;
    use crate::settings::{ContextMenuItem, ContextMenuSettings};

    #[test]
    fn drag_reorder_moves_an_item_before_or_after_the_target() {
        let mut items = ContextMenuSettings::default().items;

        assert!(reorder_context_menu_items(
            &mut items,
            ContextMenuItem::Download,
            ContextMenuItem::Play,
            false,
        ));
        assert_eq!(items[0].item, ContextMenuItem::Download);

        assert!(reorder_context_menu_items(
            &mut items,
            ContextMenuItem::Download,
            ContextMenuItem::Play,
            true,
        ));
        assert_eq!(items[1].item, ContextMenuItem::Download);
    }
}
