use super::{PreferencesPageKind as Page, PreferencesSearchItem};
use crate::shell::shell::Shell;
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

pub(super) fn items(shell: &Shell) -> Vec<PreferencesSearchItem> {
    let mut items = Vec::new();
    for (page, resource) in [
        (Page::General, "preferences/general.ui"),
        (Page::Customization, "preferences/appearance.ui"),
        (Page::Theme, "preferences/theme.ui"),
        (Page::Integrations, "preferences/integrations.ui"),
        (Page::Playback, "preferences/playback.ui"),
        (Page::Equalizer, "player/equalizer.ui"),
        (Page::Library, "preferences/library.ui"),
        (Page::Downloads, "preferences/download_quality.ui"),
        (Page::Connect, "preferences/connect.ui"),
        (Page::Controller, "preferences/controller.ui"),
        (Page::Storage, "preferences/storage.ui"),
        (Page::Storage, "preferences/backup.ui"),
        (Page::Privacy, "preferences/privacy.ui"),
        (Page::Playback, "player/transition_settings.ui"),
    ] {
        let bytes = gtk::gio::resources_lookup_data(
            &format!("{}{resource}", crate::ui_resource::UI_PREFIX),
            gtk::gio::ResourceLookupFlags::NONE,
        )
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
            if resource == "preferences/library.ui" {
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
            // A page inside a page, like each integration's, names where its rows are.
            let nested_page = node
                .ancestors()
                .filter(|parent| parent.attribute("class") == Some("AdwPreferencesPage"))
                .find(|parent| parent.parent_element() != Some(document.root_element()))
                .map(|node| property(node, "title"))
                .filter(|title| !title.is_empty());
            let group = nested_page
                .or(expander)
                .or_else(|| group.map(|node| property(node, "title")))
                .unwrap_or_default();
            add(
                &mut items,
                page,
                title,
                &property(node, "subtitle"),
                &group,
                None,
            );
        }
    }
    for title in [
        super::general::NORMALIZATION_SCOPE_TITLE,
        super::general::NORMALIZATION_TITLE,
        super::general::VOLUME_SCALE_TITLE,
        super::general::STREAM_QUALITY_TITLE,
        crate::player::playback_settings::CROSSFADE_DURATION_TITLE,
        crate::player::playback_settings::PLAYBACK_SPEED_TITLE,
        crate::player::playback_settings::PRESERVE_PITCH_TITLE,
    ] {
        add(&mut items, Page::Playback, tr(title), "", "", None);
    }
    // Sidebar items and Home blocks are edited in their own dialogs; their rows open them.
    for (target, titles) in [
        (
            tr("Sidebar items"),
            std::iter::once(tr("Saved"))
                .chain(
                    rufin_core::settings::SidebarRouteItem::all()
                        .iter()
                        .map(|item| tr(rufin_core::route::sidebar_route_descriptor(*item).title)),
                )
                .collect::<Vec<_>>(),
        ),
        (
            tr("Home blocks"),
            rufin_core::settings::HomeBlockKind::all()
                .iter()
                .map(|item| tr(rufin_core::settings::presentation::home_block_title(*item)))
                .collect(),
        ),
    ] {
        for title in titles {
            add(
                &mut items,
                Page::Customization,
                title,
                "",
                &target,
                Some(target.clone()),
            );
        }
    }
    for title in super::customization::MENU_ENTRIES
        .iter()
        .map(|(_, title, _, _)| *title)
        .chain([super::customization::RATING_ENTRY])
    {
        add(
            &mut items,
            Page::Customization,
            tr(title),
            "",
            &tr("Item menus"),
            None,
        );
    }
    for rule in rufin_core::downloads::DownloadRule::ALL {
        add(
            &mut items,
            Page::Downloads,
            rufin_core::downloads::download_rule_title(rule),
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
            crate::widgets::source_labels::configured_source_display_name(source),
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
    target: Option<String>,
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
        target,
    });
}
