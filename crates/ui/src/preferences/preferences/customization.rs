//! Customization: the Layout, Player and Menus tabs.
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::{tr, tr_with};
use rufin_core::settings::layout::LayoutSettings;
use rufin_core::settings::{
    ContextMenuItem, HomeBlockKind, LeftSidebarMode, MAX_NARROW_LAYOUT_THRESHOLD,
    MIN_NARROW_LAYOUT_THRESHOLD, RightSidebarMode, Settings, SidebarRouteItem,
};

use super::reorder_row::{ReorderRowPresentation, reorder_row};
use crate::shell::shell::Shell;
use crate::widgets::popup::present_light_dismiss_dialog;

/// The item menu's configurable entries in the menu's order: the setting (Rating's is its own),
/// the label, whether it opens a submenu and whether a separator comes before it.
pub(super) const MENU_ENTRIES: [(ContextMenuItem, &str, bool, bool); 9] = [
    (
        ContextMenuItem::Play,
        localization::msgid("Play"),
        true,
        false,
    ),
    (
        ContextMenuItem::PlayRadio,
        localization::msgid("Play radio"),
        true,
        false,
    ),
    (
        ContextMenuItem::AddToPlaylist,
        localization::msgid("Add to Playlist"),
        true,
        true,
    ),
    (
        ContextMenuItem::Favorites,
        localization::msgid("Favorites"),
        false,
        false,
    ),
    (
        ContextMenuItem::EditMetadata,
        localization::msgid("Edit metadata"),
        false,
        true,
    ),
    (
        ContextMenuItem::GoTo,
        localization::msgid("Go to"),
        true,
        false,
    ),
    (
        ContextMenuItem::Share,
        localization::msgid("Share"),
        false,
        false,
    ),
    (
        ContextMenuItem::Download,
        localization::msgid("Download"),
        false,
        true,
    ),
    (
        ContextMenuItem::Pins,
        localization::msgid("Saved"),
        false,
        true,
    ),
];
pub(super) const RATING_ENTRY: &str = localization::msgid("Rating");

pub(super) fn page(shell: &Rc<Shell>) -> gtk::Widget {
    let resource = "preferences/appearance.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        appearance_page: gtk::Box,
        reduce_motion: adw::SwitchRow,
        default_left: adw::ToggleGroup,
        default_right: adw::ToggleGroup,
        narrow_row: adw::SwitchRow,
        threshold_row: adw::SpinRow,
        narrow_left_row: adw::ActionRow,
        narrow_right_row: adw::ActionRow,
        narrow_left: adw::ToggleGroup,
        narrow_right: adw::ToggleGroup,
        sidebar_items_row: adw::ActionRow,
        home_blocks_row: adw::ActionRow,
        bottom_bar_rating_row: adw::SwitchRow,
        waveform_row: adw::SwitchRow,
        lyrics_panel_row: adw::SwitchRow,
        visualizer_panel_row: adw::SwitchRow,
        combine_panels_row: adw::SwitchRow,
        fullscreen_lyrics: adw::SwitchRow,
        fullscreen_visualizer: adw::SwitchRow,
        dynamic_background: adw::SwitchRow,
        background_image: adw::SwitchRow,
        menu_preview: gtk::Box,
    });
    let settings = shell.settings.current.borrow().clone();

    // Layout
    let motion_shell = Rc::clone(shell);
    shell.settings.bind_switch(
        &reduce_motion,
        "reduced motion",
        |settings| &mut settings.application.reduce_motion,
        move |settings| motion_shell.appearance.apply(settings),
    );
    let layout = &settings.layout;
    bind_left(
        shell,
        &default_left,
        layout.default_profile.left_sidebar,
        |layout| &mut layout.default_profile.left_sidebar,
    );
    bind_right(
        shell,
        &default_right,
        layout.default_profile.right_sidebar,
        |layout| &mut layout.default_profile.right_sidebar,
    );
    bind_left(
        shell,
        &narrow_left,
        layout.narrow_profile.left_sidebar,
        |layout| &mut layout.narrow_profile.left_sidebar,
    );
    bind_right(
        shell,
        &narrow_right,
        layout.narrow_profile.right_sidebar,
        |layout| &mut layout.narrow_profile.right_sidebar,
    );
    threshold_row.set_adjustment(Some(&gtk::Adjustment::new(
        f64::from(layout.narrow_threshold),
        f64::from(MIN_NARROW_LAYOUT_THRESHOLD),
        f64::from(MAX_NARROW_LAYOUT_THRESHOLD),
        10.0,
        100.0,
        0.0,
    )));
    let threshold_shell = Rc::clone(shell);
    threshold_row.connect_value_notify(move |row| {
        let threshold = row.value().round() as i32;
        update_layout(&threshold_shell, |layout| {
            layout.narrow_threshold = threshold
        });
    });
    let narrow_rows: [gtk::Widget; 3] = [
        threshold_row.upcast(),
        narrow_left_row.upcast(),
        narrow_right_row.upcast(),
    ];
    for row in &narrow_rows {
        row.set_sensitive(layout.narrow_enabled);
    }
    narrow_row.set_active(layout.narrow_enabled);
    let narrow_shell = Rc::clone(shell);
    narrow_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        for row in &narrow_rows {
            row.set_sensitive(enabled);
        }
        update_layout(&narrow_shell, |layout| layout.narrow_enabled = enabled);
    });
    let dialog_shell = Rc::clone(shell);
    sidebar_items_row.connect_activated(move |row| present_sidebar_items(&dialog_shell, row));
    let dialog_shell = Rc::clone(shell);
    home_blocks_row.connect_activated(move |row| present_home_blocks(&dialog_shell, row));

    // Player
    let bottom_bar_shell = Rc::clone(shell);
    shell.settings.bind_switch(
        &bottom_bar_rating_row,
        "bottom bar rating setting",
        |settings| &mut settings.show_bottom_bar_rating,
        move |_| bottom_bar_shell.player_ui.update_bottom_player(),
    );
    let waveform_shell = Rc::clone(shell);
    shell.settings.bind_switch(
        &waveform_row,
        "seekbar waveform setting",
        |settings| &mut settings.player_display.seekbar_waveform_enabled,
        move |_| waveform_shell.player_ui.update_bottom_player_transport(),
    );
    lyrics_panel_row.set_active(settings.lyrics_panel_visible);
    let lyrics_panel_shell = Rc::clone(shell);
    lyrics_panel_row.connect_active_notify(move |row| {
        lyrics_panel_shell.set_lyrics_panel_visible(row.is_active());
    });
    visualizer_panel_row.set_active(settings.visualizer_panel_visible);
    let visualizer_panel_shell = Rc::clone(shell);
    visualizer_panel_row.connect_active_notify(move |row| {
        visualizer_panel_shell.set_visualizer_panel_visible(row.is_active());
    });
    combine_panels_row.set_active(settings.right_panel.combined);
    let player = Rc::downgrade(&shell.player_ui);
    combine_panels_row.connect_active_notify(move |row| {
        if let Some(player) = player.upgrade() {
            player.set_right_panel_combined(row.is_active());
        }
    });
    background_image.set_sensitive(settings.player_display.fullscreen_dynamic_background);
    let image_row = background_image.downgrade();
    dynamic_background.connect_active_notify(move |row| {
        if let Some(image_row) = image_row.upgrade() {
            image_row.set_sensitive(row.is_active());
        }
    });
    let fullscreen_fields: [(adw::SwitchRow, fn(&mut Settings) -> &mut bool); 4] = [
        (fullscreen_lyrics, |settings| {
            &mut settings.player_display.fullscreen_lyrics_visible
        }),
        (fullscreen_visualizer, |settings| {
            &mut settings.player_display.fullscreen_visualizer_visible
        }),
        (dynamic_background, |settings| {
            &mut settings.player_display.fullscreen_dynamic_background
        }),
        (background_image, |settings| {
            &mut settings.player_display.fullscreen_background_image
        }),
    ];
    for (row, field) in fullscreen_fields {
        let player = Rc::downgrade(&shell.player_ui);
        shell
            .settings
            .bind_switch(&row, "fullscreen display", field, move |_| {
                if let Some(player) = player.upgrade() {
                    player.apply_fullscreen_display_settings();
                }
            });
    }

    // Menus
    populate_menu_preview(shell, &menu_preview);

    let sidebar_items_row = sidebar_items_row.downgrade();
    let home_blocks_row = home_blocks_row.downgrade();
    super::refresh_while_mapped(
        shell,
        &appearance_page,
        Rc::new(move |settings: &Settings| {
            if let Some(row) = sidebar_items_row.upgrade() {
                let items = &settings.sidebar.route_items;
                row.set_subtitle(&shown_count(
                    items.iter().filter(|item| item.visible).count(),
                    items.len(),
                ));
            }
            if let Some(row) = home_blocks_row.upgrade() {
                row.set_subtitle(&shown_count(
                    settings.application.home_blocks.len(),
                    HomeBlockKind::all().len(),
                ));
            }
        }),
    );
    appearance_page.upcast()
}

fn shown_count(shown: usize, total: usize) -> String {
    tr_with(
        "{shown} of {total} shown",
        &[("shown", &shown.to_string()), ("total", &total.to_string())],
    )
}

fn update_layout(shell: &Rc<Shell>, edit: impl FnOnce(&mut LayoutSettings)) {
    shell
        .settings
        .update_app_settings("layout setting", |settings| {
            let before = settings.layout.clone();
            edit(&mut settings.layout);
            settings.layout.sanitize();
            settings.layout != before
        });
    shell.update_layout();
}

fn bind_left(
    shell: &Rc<Shell>,
    toggles: &adw::ToggleGroup,
    mode: LeftSidebarMode,
    field: fn(&mut LayoutSettings) -> &mut LeftSidebarMode,
) {
    toggles.set_active_name(Some(match mode {
        LeftSidebarMode::Full => "full",
        LeftSidebarMode::Compact => "compact",
        LeftSidebarMode::Hidden => "hidden",
    }));
    let shell = Rc::clone(shell);
    toggles.connect_active_name_notify(move |toggles| {
        let mode = match toggles.active_name().as_deref() {
            Some("compact") => LeftSidebarMode::Compact,
            Some("hidden") => LeftSidebarMode::Hidden,
            _ => LeftSidebarMode::Full,
        };
        update_layout(&shell, |layout| *field(layout) = mode);
    });
}

fn bind_right(
    shell: &Rc<Shell>,
    toggles: &adw::ToggleGroup,
    mode: RightSidebarMode,
    field: fn(&mut LayoutSettings) -> &mut RightSidebarMode,
) {
    toggles.set_active_name(Some(if mode.is_visible() {
        "visible"
    } else {
        "hidden"
    }));
    let shell = Rc::clone(shell);
    toggles.connect_active_name_notify(move |toggles| {
        let mode = if toggles.active_name().as_deref() == Some("hidden") {
            RightSidebarMode::Hidden
        } else {
            RightSidebarMode::Visible
        };
        update_layout(&shell, |layout| *field(layout) = mode);
    });
}

fn navigation_dialog(shell: &Rc<Shell>, dialog: &str, header: &str) -> gtk::Builder {
    let resource = "preferences/navigation_dialogs.ui";
    let builder = crate::ui_resource::builder(resource);
    let dialog: adw::Dialog = crate::ui_resource::object(&builder, resource, dialog);
    let header: adw::HeaderBar = crate::ui_resource::object(&builder, resource, header);
    crate::widgets::controls::configure_dialog_header(&header);
    dialog.set_content_height(crate::widgets::layout::large_popup_content_height(
        shell.chrome.window.height(),
        dialog.content_height(),
    ));
    builder
}

fn present_sidebar_items(shell: &Rc<Shell>, parent: &adw::ActionRow) {
    let resource = "preferences/navigation_dialogs.ui";
    let builder = navigation_dialog(shell, "sidebar_dialog", "sidebar_header");
    crate::objects!(builder, resource, {
        sidebar_dialog: adw::Dialog,
        saved_row: adw::SwitchRow,
        sidebar_group: adw::PreferencesGroup,
    });
    saved_row.set_active(shell.settings.current.borrow().sidebar.pins_visible);
    let saved_shell = Rc::clone(shell);
    saved_row.connect_active_notify(move |row| {
        let visible = row.is_active();
        if saved_shell
            .settings
            .update_app_settings("Pins visibility setting", |settings| {
                if settings.sidebar.pins_visible == visible {
                    return false;
                }
                settings.sidebar.pins_visible = visible;
                true
            })
            .is_some()
        {
            saved_shell.rebuild_sidebar_navigation();
        }
    });
    let rows = Rc::new(RefCell::new(Vec::new()));
    populate_sidebar_items(shell, &sidebar_group, &rows);
    present_light_dismiss_dialog(&sidebar_dialog, parent);
}

type RowList = Rc<RefCell<Vec<adw::ActionRow>>>;

fn populate_sidebar_items(shell: &Rc<Shell>, group: &adw::PreferencesGroup, rows: &RowList) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let items = shell.settings.current.borrow().sidebar.route_items.clone();
    for entry in items {
        let item = entry.item;
        let descriptor = rufin_core::route::sidebar_route_descriptor(item);
        // Each edit lands in the sidebar, then the rows read the order again.
        let commit = {
            let shell = Rc::clone(shell);
            let group = group.downgrade();
            let rows = Rc::downgrade(rows);
            move |edit: &dyn Fn(&mut rufin_core::settings::SidebarSettings) -> bool| {
                let changed = shell
                    .settings
                    .update_app_settings("sidebar setting", |settings| edit(&mut settings.sidebar))
                    .is_some();
                if changed {
                    shell.rebuild_sidebar_navigation();
                    if let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) {
                        populate_sidebar_items(&shell, &group, &rows);
                    }
                }
                changed
            }
        };
        let on_visible = {
            let commit = commit.clone();
            move |visible| {
                commit(&|sidebar| sidebar.set_route_visible(item, visible));
            }
        };
        let on_move = {
            let commit = commit.clone();
            move |delta| {
                commit(&|sidebar| sidebar.move_route(item, delta));
            }
        };
        let on_drop = move |source: String, after| {
            let Some(source) = SidebarRouteItem::from_stable_id(&source) else {
                return false;
            };
            commit(&|sidebar| sidebar.reorder_route(source, item, after))
        };
        let row = reorder_row(
            ReorderRowPresentation {
                title: tr(descriptor.title),
                subtitle: if item == SidebarRouteItem::Moods {
                    tr("Files need Mood/BPM tags written on them. Not supported for Jellyfin")
                } else {
                    String::new()
                },
                icon: Some(descriptor.icon_name),
                visible: entry.visible,
                visible_sensitive: true,
                up_sensitive: true,
                down_sensitive: true,
                drag_id: descriptor.stable_id.to_string(),
            },
            on_visible,
            on_move,
            on_drop,
        );
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

fn present_home_blocks(shell: &Rc<Shell>, parent: &adw::ActionRow) {
    let resource = "preferences/navigation_dialogs.ui";
    let builder = navigation_dialog(shell, "home_dialog", "home_header");
    crate::objects!(builder, resource, {
        home_dialog: adw::Dialog,
        home_group: adw::PreferencesGroup,
    });
    let rows = Rc::new(RefCell::new(Vec::new()));
    populate_home_blocks(shell, &home_group, &rows);
    present_light_dismiss_dialog(&home_dialog, parent);
}

fn populate_home_blocks(shell: &Rc<Shell>, group: &adw::PreferencesGroup, rows: &RowList) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let blocks = shell.settings.persistence.home_block_rows();
    let visible_count = blocks.iter().filter(|row| row.visible).count();
    for (position, home_block) in blocks.into_iter().enumerate() {
        let block = home_block.block;
        let visible_index = home_block.visible.then_some(position);
        // Each edit is core's; the rows read again after it lands.
        let commit = {
            let shell = Rc::clone(shell);
            let group = group.downgrade();
            let rows = Rc::downgrade(rows);
            move |result| {
                if !shell.commit_home_blocks(result) {
                    return false;
                }
                if let (Some(group), Some(rows)) = (group.upgrade(), rows.upgrade()) {
                    populate_home_blocks(&shell, &group, &rows);
                }
                true
            }
        };
        let on_visible = {
            let shell = Rc::clone(shell);
            let commit = commit.clone();
            move |visible| {
                commit(
                    shell
                        .settings
                        .persistence
                        .set_home_block_visible(block, visible),
                );
            }
        };
        let on_move = {
            let shell = Rc::clone(shell);
            let commit = commit.clone();
            move |delta: isize| {
                commit(
                    shell
                        .settings
                        .persistence
                        .move_home_block(block, delta as i32),
                );
            }
        };
        let on_drop = {
            let shell = Rc::clone(shell);
            move |source: String, after| {
                let Some(source) = HomeBlockKind::all()
                    .into_iter()
                    .find(|candidate| format!("{candidate:?}") == source)
                else {
                    return false;
                };
                commit(
                    shell
                        .settings
                        .persistence
                        .reorder_home_block(source, block, after),
                )
            }
        };
        let row = reorder_row(
            ReorderRowPresentation {
                title: tr(&home_block.title),
                subtitle: String::new(),
                icon: None,
                visible: home_block.visible,
                visible_sensitive: home_block.can_toggle,
                up_sensitive: visible_index.is_some_and(|index| index > 0),
                down_sensitive: visible_index.is_some_and(|index| index + 1 < visible_count),
                drag_id: format!("{block:?}"),
            },
            on_visible,
            on_move,
            on_drop,
        );
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

/// The item menu as albums, artists and tracks show it; each entry shows or hides itself.
fn populate_menu_preview(shell: &Rc<Shell>, preview: &gtk::Box) {
    let menu = shell
        .settings
        .current
        .borrow()
        .application
        .context_menu
        .clone();
    for (item, label, submenu, separator) in MENU_ENTRIES {
        if separator {
            preview.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let title = gtk::Label::new(Some(&tr(label)));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        let entry = menu_entry(&title.clone().upcast(), submenu, menu.is_visible(item));
        let settings = Rc::clone(&shell.settings);
        entry.connect_clicked(move |entry| {
            let visible = !entry.has_css_class("menu-preview-shown");
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
                if stored.visible == visible {
                    return false;
                }
                stored.visible = visible;
                true
            });
            show_menu_entry(entry, visible);
        });
        preview.append(&entry);
    }
    preview.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let rating = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    rating.set_hexpand(true);
    for icon in [
        "rufin-starred-symbolic",
        "rufin-starred-symbolic",
        "rufin-starred-symbolic",
        "rufin-non-starred-symbolic",
        "rufin-non-starred-symbolic",
    ] {
        rating.append(&gtk::Image::from_icon_name(icon));
    }
    let label = gtk::Label::new(Some(&tr(RATING_ENTRY)));
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.set_hexpand(true);
    label.set_xalign(1.0);
    rating.append(&label);
    let entry = menu_entry(&rating.upcast(), false, menu.rating_visible);
    let settings = Rc::clone(&shell.settings);
    entry.connect_clicked(move |entry| {
        let visible = !entry.has_css_class("menu-preview-shown");
        settings.set_app_setting("context menu rating setting", visible, |settings| {
            &mut settings.application.context_menu.rating_visible
        });
        show_menu_entry(entry, visible);
    });
    preview.append(&entry);
}

fn menu_entry(content: &gtk::Widget, submenu: bool, visible: bool) -> gtk::Button {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let check = gtk::Image::from_icon_name("rufin-object-select-symbolic");
    check.add_css_class("menu-preview-check");
    row.append(&check);
    row.append(content);
    if submenu {
        let arrow = gtk::Image::from_icon_name("rufin-go-next-symbolic");
        arrow.add_css_class("dim-label");
        row.append(&arrow);
    }
    let entry = gtk::Button::builder().child(&row).build();
    entry.add_css_class("flat");
    entry.add_css_class("menu-preview-entry");
    show_menu_entry(&entry, visible);
    entry
}

/// A shown entry has its check mark; a hidden one is struck through and dimmed.
fn show_menu_entry(entry: &gtk::Button, visible: bool) {
    if visible {
        entry.add_css_class("menu-preview-shown");
    } else {
        entry.remove_css_class("menu-preview-shown");
    }
    entry.update_state(&[gtk::accessible::State::Pressed(if visible {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    })]);
}
