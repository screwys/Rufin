use super::{PreferencesPageKind as Page, PreferencesSearchItem};
use crate::{Preferences, ui_resource::*};
use localization::tr;

fn property(node: roxmltree::Node<'_, '_>, name: &str) -> String {
    node.children()
        .find(|child| child.has_tag_name("property") && child.attribute("name") == Some(name))
        .and_then(|property| {
            property.text().map(|text| {
                if property.attribute("translatable") == Some("yes") {
                    tr(text)
                } else {
                    text.into()
                }
            })
        })
        .unwrap_or_default()
}

pub(super) fn items(shell: &Preferences) -> Vec<PreferencesSearchItem> {
    let mut items = Vec::new();
    for (page, resource) in [
        (Page::General, GENERAL_PREFERENCES_RESOURCE),
        (Page::Customization, APPEARANCE_PREFERENCES_RESOURCE),
        (Page::Theme, THEME_PREFERENCES_RESOURCE),
        (Page::Integrations, INTEGRATIONS_RESOURCE),
        (Page::Playback, PLAYBACK_PREFERENCES_RESOURCE),
        (Page::Equalizer, gtk_player::ui_resource::EQUALIZER_RESOURCE),
        (Page::Library, LIBRARY_PREFERENCES_RESOURCE),
        (Page::Downloads, DOWNLOAD_QUALITY_RESOURCE),
        (Page::Connect, CONNECT_RESOURCE),
        (Page::Controller, CONTROLLER_PREFERENCES_RESOURCE),
        (Page::Storage, STORAGE_PREFERENCES_RESOURCE),
        (Page::Storage, BACKUP_PREFERENCES_RESOURCE),
        (Page::Privacy, PRIVACY_PREFERENCES_RESOURCE),
        (
            Page::Playback,
            gtk_player::ui_resource::TRANSITION_SETTINGS_RESOURCE,
        ),
    ] {
        let bytes = gtk::gio::resources_lookup_data(resource, gtk::gio::ResourceLookupFlags::NONE)
            .expect("compiled preferences resource");
        let document = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap())
            .expect("preferences resource XML");
        for node in document.descendants().filter(|node| {
            node.attribute("class")
                .is_some_and(|class| class.starts_with("Adw") && class.ends_with("Row"))
        }) {
            let title = property(node, "title");
            if title.is_empty() {
                continue;
            }
            if node
                .ancestors()
                .any(|parent| parent.attribute("id") == Some("settings_page"))
            {
                continue;
            }
            let mut page = page;
            if resource == LIBRARY_PREFERENCES_RESOURCE {
                let root = node
                    .ancestors()
                    .find(|parent| parent.parent_element() == Some(document.root_element()))
                    .unwrap();
                if matches!(
                    root.attribute("id"),
                    Some("local_page" | "local_group" | "empty_local_row")
                ) {
                    continue;
                }
                if root
                    .attribute("id")
                    .is_some_and(|id| id.contains("download") || id == "empty_remote_row")
                {
                    page = Page::Downloads;
                }
            }
            let group = node.ancestors().find(|parent| {
                matches!(
                    parent.attribute("class"),
                    Some("AdwPreferencesGroup" | "AdwExpanderRow")
                )
            });
            let expander = node
                .ancestors()
                .find(|parent| parent.attribute("class") == Some("AdwExpanderRow"))
                .map(|node| property(node, "title"));
            let group = expander
                .clone()
                .or_else(|| group.map(|node| property(node, "title")))
                .unwrap_or_default();
            add(
                &mut items,
                page,
                title,
                &property(node, "subtitle"),
                &group,
                expander,
            );
        }
    }
    for title in [
        super::DEFAULT_LEFT_TITLE,
        super::DEFAULT_RIGHT_TITLE,
        super::NARROW_LEFT_TITLE,
        super::NARROW_RIGHT_TITLE,
    ] {
        add(&mut items, Page::Customization, tr(title), "", "", None);
    }
    for title in [
        super::general::NORMALIZATION_SCOPE_TITLE,
        super::general::NORMALIZATION_TITLE,
        super::general::VOLUME_SCALE_TITLE,
        super::general::STREAM_QUALITY_TITLE,
        gtk_player::playback_settings::CROSSFADE_DURATION_TITLE,
        gtk_player::playback_settings::PLAYBACK_SPEED_TITLE,
        gtk_player::playback_settings::PRESERVE_PITCH_TITLE,
    ] {
        add(&mut items, Page::Playback, tr(title), "", "", None);
    }
    for (id, titles) in [
        (
            "sidebar_items",
            crate::SidebarRouteItem::all()
                .iter()
                .map(|item| tr(gtk_widgets::route::sidebar_route_descriptor(*item).title))
                .collect::<Vec<_>>(),
        ),
        (
            "home_blocks",
            rufin_core::settings::HomeBlockKind::all()
                .iter()
                .map(|item| tr(gtk_widgets::settings::home_block_title(*item)))
                .collect(),
        ),
    ] {
        let bytes = gtk::gio::resources_lookup_data(
            APPEARANCE_PREFERENCES_RESOURCE,
            gtk::gio::ResourceLookupFlags::NONE,
        )
        .unwrap();
        let document = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let group = property(
            document
                .descendants()
                .find(|node| node.attribute("id") == Some(id))
                .unwrap(),
            "title",
        );
        for title in titles {
            add(
                &mut items,
                Page::Customization,
                title,
                "",
                &group,
                Some(group.clone()),
            );
        }
    }
    for rule in downloads::DownloadRule::ALL {
        add(
            &mut items,
            Page::Downloads,
            tr(super::library::download_rule_title(rule)),
            &tr(super::library::download_rule_subtitle(rule)),
            "",
            None,
        );
    }
    for source in shell
        .source
        .configured
        .borrow()
        .sources
        .iter()
        .filter(|source| source.kind != "local")
    {
        add(
            &mut items,
            Page::Library,
            gtk_widgets::source_labels::configured_source_display_name(source),
            "",
            "",
            None,
        );
    }
    items
}

fn add(
    items: &mut Vec<PreferencesSearchItem>,
    page: Page,
    title: String,
    subtitle: &str,
    group: &str,
    expander_title: Option<String>,
) {
    let context = if group.is_empty() {
        page.title()
    } else {
        format!("{} · {group}", page.title())
    };
    items.push(PreferencesSearchItem {
        page,
        searchable_text: format!("{title} {subtitle} {context}").to_lowercase(),
        title,
        context,
        expander_title,
    });
}
