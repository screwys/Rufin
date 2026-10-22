use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk_widgets::artwork::ArtworkTile;
use gtk_widgets::interactions::ContextMenuSurface;
use rufin_core::metadata::{MediaDetails, MediaDetailsItem};

use crate::media_menus::MediaMenus;

pub(crate) struct MenuHeader<'a> {
    pub item: MediaDetailsItem,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub artwork: Option<&'a [u8]>,
    pub favorite: bool,
}

pub(crate) fn append_header(
    surface: &ContextMenuSurface,
    target: &gtk::Widget,
    menus: &Rc<MediaMenus>,
    group: &str,
    identity: MenuHeader<'_>,
) {
    let resource = crate::ui_resource::MEDIA_MENU_HEADER_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        header: gtk::Box, cover: gtk::Box, title_host: gtk::Box, subtitle_host: gtk::Box,
        favorite: gtk::Button, details: gtk::Button,
    });
    let (title_slide, title) = gtk_widgets::text_slide::new_label();
    let (subtitle_slide, subtitle) = gtk_widgets::text_slide::new_label();
    title_host.append(&title_slide);
    subtitle_host.append(&subtitle_slide);
    title.set_text(identity.title);
    subtitle.set_text(identity.subtitle);
    subtitle_host.set_visible(!identity.subtitle.is_empty());
    bind_cover(&cover, menus, identity.artwork, 48);
    gtk_widgets::favorites::set_favorite_button_active(&favorite, identity.favorite);
    favorite.set_visible(
        menus
            .settings
            .current
            .borrow()
            .application
            .context_menu
            .is_visible(rufin_core::settings::ContextMenuItem::Favorites),
    );
    favorite.set_action_name(Some(&format!("{group}.favorite")));
    details.set_action_name(Some(&format!("{group}.details")));
    let parent = target
        .root()
        .and_downcast::<gtk::Window>()
        .map(|parent| parent.downgrade());
    let menus = menus.clone();
    surface.add_action("details", move || {
        if let Some(parent) = parent.as_ref().and_then(glib::WeakRef::upgrade) {
            present(&parent, &menus, identity.item.clone());
        }
    });
    surface.append_fixed_widget("media-header", &header);
}

fn bind_cover(host: &gtk::Box, menus: &MediaMenus, binding: Option<&[u8]>, size: i32) {
    let tile = ArtworkTile::new(size);
    menus.artwork.bind_artwork_tile(
        &tile,
        gtk_widgets::library_fields::opaque_artwork(binding),
        size,
    );
    host.append(&tile.widget());
}

fn present(parent: &gtk::Window, menus: &Rc<MediaMenus>, item: MediaDetailsItem) {
    let resource = crate::ui_resource::MEDIA_DETAILS_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        dialog: adw::Dialog, header: adw::HeaderBar, heading: adw::WindowTitle, state: gtk::Stack, edit: gtk::Button,
        cover: gtk::Box, title: gtk::Label, subtitle: gtk::Label, summary: gtk::Box, sections: gtk::Box,
    });
    gtk_widgets::controls::configure_dialog_header(&header);
    gtk_widgets::popup::present_light_dismiss_dialog(&dialog, parent);
    let weak_dialog = dialog.downgrade();
    let source = menus.source.clone();
    let playback = menus.playback.clone();
    let menus = menus.clone();
    let parent = parent.downgrade();
    let load = glib::spawn_future_local(async move {
        let result = rufin_core::metadata::media_details(source, playback, item.clone()).await;
        let Some(dialog) = weak_dialog.upgrade() else {
            return;
        };
        match result {
            Ok(details) => {
                dialog.set_title(&details.heading);
                heading.set_title(&details.heading);
                title.set_text(&details.title);
                subtitle.set_text(&details.subtitle);
                subtitle.set_visible(!details.subtitle.is_empty());
                bind_cover(&cover, &menus, details.artwork_binding.as_deref(), 86);
                populate(&summary, &sections, &details, &menus, &parent);
                edit.set_visible(details.can_edit);
                let editor = menus.edit_metadata.clone();
                let weak = dialog.downgrade();
                edit.connect_clicked(move |_| {
                    if let Some(dialog) = weak.upgrade() {
                        dialog.close();
                    }
                    editor(item.metadata_item());
                });
                state.set_visible_child_name("details");
            }
            Err(error) => {
                dialog.close();
                (menus.show_error)(error.to_string());
            }
        }
    });
    dialog.connect_closed(move |_| load.abort());
}

fn populate(
    summary: &gtk::Box,
    sections: &gtk::Box,
    data: &MediaDetails,
    menus: &Rc<MediaMenus>,
    parent: &glib::WeakRef<gtk::Window>,
) {
    summary.set_visible(!data.audio_summary.is_empty());
    for field in &data.audio_summary {
        let resource = crate::ui_resource::MEDIA_DETAIL_METRIC_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {metric: gtk::Box, label: gtk::Label, value: gtk::Label});
        label.set_text(&field.label);
        value.set_text(&field.value);
        summary.append(&metric);
    }
    for group in &data.sections {
        let resource = crate::ui_resource::MEDIA_DETAIL_SECTION_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {section: adw::PreferencesGroup, fields: gtk::ListBox});
        section.set_title(&group.title);
        for field in &group.fields {
            let resource = crate::ui_resource::MEDIA_DETAIL_FIELD_RESOURCE;
            let builder = crate::ui_resource::builder(resource);
            gtk_widgets::objects!(builder, resource, {row: gtk::ListBoxRow, label: gtk::Label, value: gtk::Label, copy: gtk::Button, copy_icon: gtk::Image, musicbrainz: gtk::Button});
            label.set_text(&field.label);
            value.set_text(&field.value);
            if field.copy {
                copy.set_visible(true);
                let action = gtk_widgets::clipboard::copy_action(copy.upcast_ref(), &copy_icon);
                let text = field.value.clone();
                copy.connect_clicked(move |_| action(&text));
            }
            if let Some(url) = &field.musicbrainz_url {
                musicbrainz.set_visible(true);
                let url = url.clone();
                let parent = parent.clone();
                let show_error = menus.show_error.clone();
                musicbrainz.connect_clicked(move |_| {
                    let Some(parent) = parent.upgrade() else {
                        return;
                    };
                    let launcher = gtk::UriLauncher::new(&url);
                    let show_error = show_error.clone();
                    glib::spawn_future_local(async move {
                        if let Err(error) = launcher.launch_future(Some(&parent)).await {
                            show_error(error.to_string());
                        }
                    });
                });
            }
            if !field.links.is_empty() {
                let markup: String = field
                    .links
                    .iter()
                    .enumerate()
                    .map(|(index, part)| {
                        let text = glib::markup_escape_text(&part.text);
                        if part.target.is_some() {
                            format!(r#"<a href="{index}">{text}</a>"#)
                        } else {
                            text.to_string()
                        }
                    })
                    .collect();
                value.set_markup(&markup);
                let links = field.links.clone();
                let menus = menus.clone();
                let parent = parent.clone();
                value.connect_activate_link(move |_, link| {
                    if let Some(item) = link
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| links.get(index))
                        .and_then(|part| part.target.as_ref())
                        && let Some(parent) = parent.upgrade()
                    {
                        present(&parent, &menus, item.clone());
                    }
                    glib::Propagation::Stop
                });
            }
            fields.append(&row);
        }
        sections.append(&section);
    }
}
