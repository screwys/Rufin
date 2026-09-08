use std::rc::Rc;

use adw::prelude::*;

use crate::shell::Shell;
use rufin_core::settings::ContextMenuItem;

pub(crate) fn configure_context_menus_expander(
    shell: &Rc<Shell>,
    builder: &gtk::Builder,
    expander: &adw::ExpanderRow,
) {
    let resource = crate::ui_resource::APPEARANCE_PREFERENCES_RESOURCE;
    for (item, id) in [
        (ContextMenuItem::Play, "context_menu_play"),
        (ContextMenuItem::PlayNext, "context_menu_play_next"),
        (ContextMenuItem::PlayLater, "context_menu_play_later"),
        (ContextMenuItem::PlayRadio, "context_menu_play_radio"),
        (ContextMenuItem::AddToPlaylist, "context_menu_playlist"),
        (ContextMenuItem::Favorites, "context_menu_favorites"),
        (ContextMenuItem::EditMetadata, "context_menu_metadata"),
        (ContextMenuItem::Pins, "context_menu_pins"),
        (ContextMenuItem::GoTo, "context_menu_go_to"),
        (ContextMenuItem::Download, "context_menu_download"),
    ] {
        let row: adw::SwitchRow = ui_shared::ui_resource::object(builder, resource, id);
        row.set_active(
            shell
                .settings
                .current
                .borrow()
                .context_menu
                .is_visible(item),
        );
        let settings = Rc::clone(&shell.settings);
        row.connect_active_notify(move |row| {
            settings.update_app_settings("context menu setting", |settings| {
                let Some(stored) = settings
                    .context_menu
                    .items
                    .iter_mut()
                    .find(|stored| stored.item == item)
                else {
                    return false;
                };
                if stored.visible == row.is_active() {
                    return false;
                }
                stored.visible = row.is_active();
                true
            });
        });
        expander.add_row(&row);
    }

    let rating: adw::SwitchRow =
        ui_shared::ui_resource::object(builder, resource, "context_menu_rating_row");
    rating.set_active(shell.settings.current.borrow().context_menu.rating_visible);
    let settings = Rc::clone(&shell.settings);
    rating.connect_active_notify(move |row| {
        settings.set_app_setting("context menu rating setting", row.is_active(), |settings| {
            &mut settings.context_menu.rating_visible
        });
    });
    expander.add_row(&rating);
}
