use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use localization::{msgid, tr};

use super::layout::{ReorderRowPresentation, reorder_row, visibility_position_subtitle};
use crate::settings::{ContextMenuItem, ContextMenuItemSettings};
use crate::shell::Shell;

pub(crate) fn configure_context_menus_expander(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rating: &adw::ActionRow,
    rating_visible: &gtk::Switch,
) {
    rating.set_activatable_widget(Some(rating_visible));
    rating_visible.set_active(shell.settings.current.borrow().context_menu.rating_visible);
    let rating_shell = Rc::clone(shell);
    rating_visible.connect_active_notify(move |switch| {
        let is_visible = switch.is_active();
        rating_shell.set_app_setting("context menu rating setting", is_visible, |settings| {
            &mut settings.context_menu.rating_visible
        });
    });
    let rows = Rc::new(RefCell::new(
        Vec::<gtk::glib::WeakRef<adw::ActionRow>>::new(),
    ));
    let shell = Rc::clone(shell);
    let rating = rating.clone();
    super::populate_expander_once(&expander, move |expander| {
        populate_context_menu_rows(&shell, expander, &rows, &rating);
    });
}

fn populate_context_menu_rows(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
    rating: &adw::ActionRow,
) {
    for row in rows.borrow_mut().drain(..) {
        if let Some(row) = row.upgrade() {
            expander.remove(&row);
        }
    }

    let items = shell.settings.current.borrow().context_menu.items.clone();
    for (position, entry) in items.into_iter().enumerate() {
        let row = context_menu_item_row(shell, expander, rows, rating, entry, position);
        expander.add_row(&row);
        rows.borrow_mut().push(row.downgrade());
    }
    expander.add_row(rating);
    rows.borrow_mut().push(rating.downgrade());
}

fn context_menu_item_row(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
    rating: &adw::ActionRow,
    entry: ContextMenuItemSettings,
    position: usize,
) -> adw::ActionRow {
    let visible_shell = Rc::clone(shell);
    let visible_expander = expander.downgrade();
    let visible_rows = Rc::clone(rows);
    let visible_rating = rating.clone();
    let on_visible = move |is_visible| {
        visible_shell.update_app_settings("context menu setting", |settings| {
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
        if let Some(expander) = visible_expander.upgrade() {
            populate_context_menu_rows(&visible_shell, &expander, &visible_rows, &visible_rating);
        }
    };

    let move_shell = Rc::clone(shell);
    let move_expander = expander.downgrade();
    let move_rows = Rc::clone(rows);
    let move_rating = rating.clone();
    let on_move = move |delta| {
        move_context_menu_item(&move_shell, entry.item, delta);
        if let Some(expander) = move_expander.upgrade() {
            populate_context_menu_rows(&move_shell, &expander, &move_rows, &move_rating);
        }
    };

    let drop_shell = Rc::clone(shell);
    let drop_expander = expander.downgrade();
    let drop_rows = Rc::downgrade(rows);
    let drop_rating = rating.clone();
    let on_drop = move |source_id: String, after| {
        let Some(source_item) = context_menu_item_from_drag_id(&source_id) else {
            return false;
        };
        let (Some(expander), Some(rows)) = (drop_expander.upgrade(), drop_rows.upgrade()) else {
            return false;
        };
        let changed = drop_shell
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
            populate_context_menu_rows(&drop_shell, &expander, &rows, &drop_rating);
        }
        changed
    };

    reorder_row(
        ReorderRowPresentation {
            title: tr(context_menu_item_title(entry.item)),
            subtitle: visibility_position_subtitle(entry.visible, position),
            visible: entry.visible,
            visible_sensitive: true,
            up_sensitive: true,
            down_sensitive: true,
            drag_id: context_menu_item_drag_id(entry.item).to_string(),
        },
        on_visible,
        on_move,
        on_drop,
    )
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
