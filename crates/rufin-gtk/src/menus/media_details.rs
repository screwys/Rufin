use std::rc::Rc;

use crate::widgets::artwork::ArtworkTile;
use crate::widgets::interactions::ContextMenuSurface;
use adw::prelude::*;
use gtk::glib;
use rufin_core::metadata::{MediaDetails, MediaDetailsItem};

use crate::shell::shell::Shell;

pub(crate) struct MenuHeader<'a> {
    pub item: MediaDetailsItem,
    pub title: &'a str,
    pub subtitle: &'a str,
    pub artwork: Option<&'a [u8]>,
    /// What the favorite button marks, and whether it is a favorite now.
    pub favorite: (library::FavoriteTarget, bool),
}

pub(crate) fn append_header(
    surface: &ContextMenuSurface,
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    identity: MenuHeader<'_>,
) {
    let resource = "routes/media_menu_header.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        header: gtk::Box, cover: gtk::Box, title_host: gtk::Box, subtitle_host: gtk::Box,
        favorite: gtk::Button, details: gtk::Button,
    });
    let (title_slide, title) = crate::widgets::text_slide::new_label();
    let (subtitle_slide, subtitle) = crate::widgets::text_slide::new_label();
    title_host.append(&title_slide);
    subtitle_host.append(&subtitle_slide);
    title.set_text(identity.title);
    subtitle.set_text(identity.subtitle);
    subtitle_host.set_visible(!identity.subtitle.is_empty());
    bind_cover(&cover, shell, identity.artwork, 48);
    let (favorite_target, favorited) = identity.favorite;
    crate::widgets::favorites::set_favorite_button_active(&favorite, favorited);
    favorite.set_visible(
        shell
            .settings
            .current
            .borrow()
            .application
            .context_menu
            .is_visible(rufin_core::settings::ContextMenuItem::Favorites),
    );
    let favorite_shell = Rc::clone(shell);
    surface.bind_button(&favorite, move || {
        favorite_shell.set_favorite(favorite_target.clone(), !favorited);
    });
    let parent = target
        .root()
        .and_downcast::<gtk::Window>()
        .map(|parent| parent.downgrade());
    let shell = shell.clone();
    surface.bind_button(&details, move || {
        if let Some(parent) = parent.as_ref().and_then(glib::WeakRef::upgrade) {
            present(&parent, &shell, identity.item.clone());
        }
    });
    surface.append_fixed_widget("media-header", &header);
}

fn bind_cover(host: &gtk::Box, shell: &Shell, binding: Option<&[u8]>, size: i32) {
    let tile = ArtworkTile::new(size);
    shell.artwork.bind_artwork_tile(
        &tile,
        crate::widgets::library_fields::opaque_artwork(binding),
        size,
    );
    host.append(&tile.widget());
}

fn present(parent: &gtk::Window, shell: &Rc<Shell>, item: MediaDetailsItem) {
    let resource = "routes/media_details.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        dialog: adw::Dialog, header: adw::HeaderBar, heading: adw::WindowTitle, state: gtk::Stack, edit: gtk::Button,
        cover: gtk::Box, title: gtk::Label, subtitle: gtk::Label, summary: gtk::Box, sections: gtk::Box,
    });
    crate::widgets::controls::configure_dialog_header(&header);
    crate::widgets::popup::present_light_dismiss_dialog(&dialog, parent);
    let weak_dialog = dialog.downgrade();
    let source = shell.products.source.clone();
    let playback = shell.products.playback.clone();
    let shell = shell.clone();
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
                bind_cover(&cover, &shell, details.artwork_binding.as_deref(), 86);
                populate(&summary, &sections, &details, &shell, &parent);
                edit.set_visible(details.can_edit);
                let editor = Rc::clone(&shell);
                let weak = dialog.downgrade();
                edit.connect_clicked(move |_| {
                    if let Some(dialog) = weak.upgrade() {
                        dialog.close();
                    }
                    crate::shell::preferences::dialogs::metadata::present_metadata_dialog(
                        &editor,
                        item.metadata_item(),
                    );
                });
                state.set_visible_child_name("details");
            }
            Err(error) => {
                dialog.close();
                shell
                    .control_feedback
                    .show_feedback_toast(error.to_string());
            }
        }
    });
    dialog.connect_closed(move |_| load.abort());
}

fn populate(
    summary: &gtk::Box,
    sections: &gtk::Box,
    data: &MediaDetails,
    shell: &Rc<Shell>,
    parent: &glib::WeakRef<gtk::Window>,
) {
    summary.set_visible(!data.audio_summary.is_empty());
    for field in &data.audio_summary {
        let resource = "routes/media_detail_metric.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {metric: gtk::Box, label: gtk::Label, value: gtk::Label});
        label.set_text(&field.label);
        value.set_text(&field.value);
        summary.append(&metric);
    }
    for group in &data.sections {
        let resource = "routes/media_detail_section.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {section: adw::PreferencesGroup, fields: gtk::ListBox});
        section.set_title(&group.title);
        for field in &group.fields {
            let resource = "routes/media_detail_field.ui";
            let builder = crate::ui_resource::builder(resource);
            crate::objects!(builder, resource, {row: gtk::ListBoxRow, label: gtk::Label, value: gtk::Label, copy: gtk::Button, copy_icon: gtk::Image, musicbrainz: gtk::Button});
            label.set_text(&field.label);
            value.set_text(&field.value);
            if field.copy {
                copy.set_visible(true);
                let action = crate::widgets::clipboard::copy_action(copy.upcast_ref(), &copy_icon);
                let text = field.value.clone();
                copy.connect_clicked(move |_| action(&text));
            }
            if let Some(url) = &field.musicbrainz_url {
                musicbrainz.set_visible(true);
                let url = url.clone();
                let parent = parent.clone();
                let feedback = Rc::clone(&shell.control_feedback);
                musicbrainz.connect_clicked(move |_| {
                    let Some(parent) = parent.upgrade() else {
                        return;
                    };
                    let launcher = gtk::UriLauncher::new(&url);
                    let feedback = Rc::clone(&feedback);
                    glib::spawn_future_local(async move {
                        if let Err(error) = launcher.launch_future(Some(&parent)).await {
                            feedback.show_feedback_toast(error.to_string());
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
                let shell = shell.clone();
                let parent = parent.clone();
                value.connect_activate_link(move |_, link| {
                    if let Some(item) = link
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| links.get(index))
                        .and_then(|part| part.target.as_ref())
                        && let Some(parent) = parent.upgrade()
                    {
                        present(&parent, &shell, item.clone());
                    }
                    glib::Propagation::Stop
                });
            }
            fields.append(&row);
        }
        sections.append(&section);
    }
}
