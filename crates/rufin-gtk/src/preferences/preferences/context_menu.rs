use std::rc::Rc;

use crate::preferences::Preferences;
use rufin_core::settings::ContextMenuItem;

pub fn configure_context_menus(shell: &Rc<Preferences>, builder: &gtk::Builder) {
    let resource = "preferences/appearance.ui";
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
        let row: adw::SwitchRow = crate::ui_resource::object(builder, resource, id);
        row.set_active(
            shell
                .settings
                .current
                .borrow()
                .application
                .context_menu
                .is_visible(item),
        );
        let settings = Rc::clone(&shell.settings);
        row.connect_active_notify(move |row| {
            settings.update_app_settings("context menu setting", |settings| {
                let Some(stored) = settings
                    .application
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
    }

    let rating: adw::SwitchRow =
        crate::ui_resource::object(builder, resource, "context_menu_rating_row");
    shell.settings.bind_switch(
        &rating,
        "context menu rating setting",
        |settings| &mut settings.application.context_menu.rating_visible,
        |_| {},
    );
}
