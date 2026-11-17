//! Customization: the Layout, Home, Player and Menus tabs.
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::tr;
use rufin_core::settings::layout::{DisplaySize, LayoutSettings};
use rufin_core::settings::{
    ContextMenuItem, HomeBlockKind, LeftSidebarMode, MAX_HOME_BLOCK_ITEMS, MAX_HOME_BLOCK_ROWS,
    MAX_NARROW_LAYOUT_THRESHOLD, MIN_NARROW_LAYOUT_THRESHOLD, RightSidebarMode, Settings,
    SidebarRouteItem,
};

use super::reorder_row::{ReorderRowPresentation, reorder_row};
use crate::shell::shell::Shell;
use crate::widgets::reorder::{ChildReorder, install_child_reorder};
use rufin_core::settings::quick_mix::{QuickMixChoice, QuickMixChoiceKind};
use std::cell::Cell;
use tracing::warn;

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
        sidebar_group: adw::PreferencesGroup,
        saved_row: adw::SwitchRow,
        grid_favorite_badges_row: adw::SwitchRow,
        grid_star_ratings_row: adw::SwitchRow,
        home_size: adw::ToggleGroup,
        home_spacing: adw::ToggleGroup,
        home_blocks: gtk::ListBox,
        quick_mix_group: adw::PreferencesGroup,
        quick_mix_choices: adw::WrapBox,
        quick_mix_playlists_part: gtk::Box,
        quick_mix_playlists: adw::WrapBox,
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
    let badges_shell = Rc::clone(shell);
    shell.settings.bind_switch(
        &grid_favorite_badges_row,
        "grid favorite badges",
        |settings| &mut settings.application.grid_favorite_badges,
        move |settings| badges_shell.appearance.apply(settings),
    );
    let ratings_shell = Rc::clone(shell);
    shell.settings.bind_switch(
        &grid_star_ratings_row,
        "grid star ratings",
        |settings| &mut settings.application.grid_star_ratings,
        move |settings| ratings_shell.appearance.apply(settings),
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
    saved_row.set_active(settings.sidebar.pins_visible);
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
    let sidebar_items = Rc::new(SidebarItems {
        shell: Rc::clone(shell),
        group: sidebar_group,
        rows: RefCell::default(),
        shown: RefCell::default(),
    });
    sidebar_items.show(&settings.sidebar.route_items);

    // Home
    let home = &settings.application.home;
    for (toggles, size, spacing) in [
        (&home_size, home.size, false),
        (&home_spacing, home.spacing, true),
    ] {
        toggles.set_active(match size {
            DisplaySize::Compact => 0,
            DisplaySize::Default => 1,
            DisplaySize::Large => 2,
        });
        let shell = Rc::clone(shell);
        toggles.connect_active_notify(move |toggles| {
            let size = match toggles.active() {
                0 => DisplaySize::Compact,
                2 => DisplaySize::Large,
                _ => DisplaySize::Default,
            };
            let persistence = &shell.settings.persistence;
            shell.commit_home_blocks(if spacing {
                persistence.set_home_spacing(size)
            } else {
                persistence.set_home_size(size)
            });
        });
    }
    let home_blocks = HomeBlocks::new(shell, home_blocks);
    let quick_mix = QuickMixEditor::new(
        shell,
        quick_mix_group,
        quick_mix_choices,
        quick_mix_playlists_part,
        quick_mix_playlists,
    );

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

    super::refresh_while_mapped(
        shell,
        &appearance_page,
        Rc::new(move |settings: &Settings| {
            // The page holds the sidebar's and the blocks' rows, which follow every writer's
            // edits.
            sidebar_items.show(&settings.sidebar.route_items);
            home_blocks.sync();
            quick_mix.refresh();
        }),
    );
    appearance_page.upcast()
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

/// The sidebar's pages in their order, under the Saved switch. Each edit lands in the
/// sidebar, then the rows show the order again.
struct SidebarItems {
    shell: Rc<Shell>,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    /// The entries the rows show.
    shown: RefCell<Vec<rufin_core::settings::SidebarRouteItemSettings>>,
}

impl SidebarItems {
    /// Lists `items`, unless the rows show them already.
    fn show(self: &Rc<Self>, items: &[rufin_core::settings::SidebarRouteItemSettings]) {
        if *self.shown.borrow() == items {
            return;
        }
        self.shown.replace(items.to_vec());
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
        for entry in items {
            let item = entry.item;
            let descriptor = rufin_core::route::sidebar_route_descriptor(item);
            let commit = {
                let weak = Rc::downgrade(self);
                move |edit: &dyn Fn(&mut rufin_core::settings::SidebarSettings) -> bool| {
                    let Some(items) = weak.upgrade() else {
                        return false;
                    };
                    let committed = items
                        .shell
                        .settings
                        .update_app_settings("sidebar setting", |settings| {
                            edit(&mut settings.sidebar)
                        });
                    let Some(committed) = committed else {
                        return false;
                    };
                    items.shell.rebuild_sidebar_navigation();
                    items.show(&committed.sidebar.route_items);
                    true
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
                    movable: true,
                    drag_id: descriptor.stable_id.to_string(),
                },
                on_visible,
                on_move,
                on_drop,
            );
            self.group.add(&row);
            self.rows.borrow_mut().push(row);
        }
    }
}

/// One Home block's row: its place and whether it shows, and under them how many items it
/// lists and the rows its columns stack.
struct HomeBlockWidgets {
    block: HomeBlockKind,
    row: adw::PreferencesRow,
    drag: gtk::Image,
    up: gtk::Button,
    down: gtk::Button,
    visible: gtk::Switch,
    options: gtk::Revealer,
    items: gtk::SpinButton,
    rows: adw::ToggleGroup,
}

/// Home's blocks in their order. Every edit is core's; the rows then show what it committed.
struct HomeBlocks {
    shell: Rc<Shell>,
    list: gtk::ListBox,
    rows: Vec<HomeBlockWidgets>,
    /// Set while the rows take the committed values, which is not an edit.
    syncing: std::cell::Cell<bool>,
}

impl HomeBlocks {
    fn new(shell: &Rc<Shell>, list: gtk::ListBox) -> Rc<Self> {
        let blocks = Rc::new_cyclic(|weak: &std::rc::Weak<Self>| Self {
            shell: Rc::clone(shell),
            rows: HomeBlockKind::all()
                .into_iter()
                .map(|block| Self::row(weak, &list, block))
                .collect(),
            list,
            syncing: std::cell::Cell::new(false),
        });
        blocks.sync();
        blocks
    }

    fn row(
        weak: &std::rc::Weak<Self>,
        list: &gtk::ListBox,
        block: HomeBlockKind,
    ) -> HomeBlockWidgets {
        let resource = "preferences/home_block_row.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            block_row: adw::PreferencesRow,
            drag: gtk::Image,
            title: gtk::Label,
            up: gtk::Button,
            down: gtk::Button,
            visible: gtk::Switch,
            options: gtk::Revealer,
            items: gtk::SpinButton,
            rows: adw::ToggleGroup,
        });
        let name = tr(rufin_core::settings::presentation::home_block_title(block));
        block_row.set_title(&name);
        title.set_label(&name);
        visible.update_property(&[gtk::accessible::Property::Label(&name)]);
        items.set_adjustment(&gtk::Adjustment::new(
            f64::from(MAX_HOME_BLOCK_ITEMS),
            1.0,
            f64::from(MAX_HOME_BLOCK_ITEMS),
            1.0,
            6.0,
            0.0,
        ));
        list.append(&block_row);

        // An edit made by the rows themselves while they sync is none.
        let edit = {
            let weak = weak.clone();
            move |edit: &dyn Fn(
                &rufin_core::settings::SettingsOwner,
            )
                -> Result<bool, rufin_core::settings::SettingsError>| {
                let Some(blocks) = weak.upgrade().filter(|blocks| !blocks.syncing.get()) else {
                    return false;
                };
                let changed = blocks
                    .shell
                    .commit_home_blocks(edit(&blocks.shell.settings.persistence));
                blocks.sync();
                changed
            }
        };
        let on_visible = edit.clone();
        visible.connect_active_notify(move |visible| {
            let shown = visible.is_active();
            on_visible(&|settings| settings.set_home_block_visible(block, shown));
        });
        for (button, delta) in [(&up, -1), (&down, 1)] {
            let on_move = edit.clone();
            button.connect_clicked(move |_| {
                on_move(&|settings| settings.move_home_block(block, delta));
            });
        }
        let on_items = edit.clone();
        items.connect_value_changed(move |items| {
            let count = items
                .value_as_int()
                .clamp(1, i32::from(MAX_HOME_BLOCK_ITEMS)) as u8;
            on_items(&|settings| settings.set_home_block_items(block, count));
        });
        let on_rows = edit.clone();
        rows.connect_active_notify(move |rows| {
            let stacked =
                Some(rows.active() as u8).filter(|rows| (1..=MAX_HOME_BLOCK_ROWS).contains(rows));
            on_rows(&|settings| settings.set_home_block_rows(block, stacked));
        });
        crate::widgets::reorder::install_row_reorder(
            &block_row,
            &drag,
            format!("{block:?}"),
            move |source, after| {
                let Some(source) = HomeBlockKind::all()
                    .into_iter()
                    .find(|candidate| format!("{candidate:?}") == source)
                else {
                    return false;
                };
                edit(&|settings| settings.reorder_home_block(source, block, after))
            },
        );
        HomeBlockWidgets {
            block,
            row: block_row,
            drag,
            up,
            down,
            visible,
            options,
            items,
            rows,
        }
    }

    /// Shows the blocks as they are committed: the shown ones in their order, then the hidden
    /// ones, each shown one with its options under it.
    fn sync(&self) {
        let blocks = self.shell.settings.persistence.home_block_rows();
        let shown = blocks.iter().filter(|block| block.visible).count();
        self.syncing.set(true);
        let mut order = Vec::with_capacity(blocks.len());
        for (position, block) in blocks.iter().enumerate() {
            let Some(widgets) = self.rows.iter().find(|row| row.block == block.block) else {
                continue;
            };
            widgets.visible.set_active(block.visible);
            widgets.visible.set_sensitive(block.can_toggle);
            // A hidden block has no place among the shown ones.
            widgets.drag.set_sensitive(block.visible);
            widgets.up.set_sensitive(block.visible && position > 0);
            widgets
                .down
                .set_sensitive(block.visible && position + 1 < shown);
            widgets
                .options
                .set_reveal_child(block.visible && block.has_options);
            widgets.items.set_value(f64::from(block.items));
            widgets.rows.set_active(u32::from(block.rows.unwrap_or(0)));
            order.push(widgets.row.clone().upcast::<gtk::ListBoxRow>());
        }
        crate::widgets::reorder::order_rows(&self.list, &order);
        self.syncing.set(false);
    }
}

/// Quick mix's entries in their Home order, then the templates no entry was copied from, as
/// chips, and under them the user's smart playlists. Every edit is core's; the chips then show
/// what it committed.
struct QuickMixEditor {
    shell: Rc<Shell>,
    group: adw::PreferencesGroup,
    choices: adw::WrapBox,
    playlists_part: gtk::Box,
    playlists: adw::WrapBox,
    /// The entries' chips, by their place in Quick mix.
    entries: RefCell<Vec<gtk::Widget>>,
    /// The entries' chips as a drag moves them.
    order: ChildReorder,
    /// The newest read of the editor; an older one that answers later is dropped.
    read: Cell<u64>,
}

impl QuickMixEditor {
    fn new(
        shell: &Rc<Shell>,
        group: adw::PreferencesGroup,
        choices: adw::WrapBox,
        playlists_part: gtk::Box,
        playlists: adw::WrapBox,
    ) -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let weak = weak.clone();
            let order = install_child_reorder(
                &choices,
                |choices: &adw::WrapBox, order| {
                    let mut after = None::<&gtk::Widget>;
                    for chip in order {
                        choices.reorder_child_after(chip, after);
                        after = Some(chip);
                    }
                },
                move |dragged, order| {
                    let Some(editor) = weak.upgrade() else {
                        return;
                    };
                    let source = editor
                        .entries
                        .borrow()
                        .iter()
                        .position(|chip| chip == dragged);
                    let target = order.iter().position(|chip| chip == dragged);
                    if let (Some(source), Some(target)) = (source, target) {
                        // It lands after the entry it passed going on, or before it going back.
                        editor.commit(editor.shell.settings.persistence.reorder_quick_mix(
                            source as u32,
                            target as u32,
                            target > source,
                        ));
                    }
                },
            );
            Self {
                shell: Rc::clone(shell),
                group,
                choices,
                playlists_part,
                playlists,
                entries: RefCell::new(Vec::new()),
                order,
                read: Cell::new(0),
            }
        })
    }

    /// Reads Quick mix as its editor lists it, and shows it.
    fn refresh(self: &Rc<Self>) {
        let read = self.read.get() + 1;
        self.read.set(read);
        let answer = self.shell.products.source.quick_mix_editor();
        let weak = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let answer = answer.recv().await;
            let Some(editor) = weak.upgrade().filter(|editor| editor.read.get() == read) else {
                return;
            };
            match answer {
                Ok(Ok(shown)) => editor.show(shown),
                Ok(Err(error)) => warn!(%error, "failed to read Quick mix"),
                Err(error) => warn!(%error, "failed to read Quick mix"),
            }
        });
    }

    fn show(self: &Rc<Self>, editor: rufin_core::settings::quick_mix::QuickMixEditor) {
        for wrap in [&self.choices, &self.playlists] {
            while let Some(chip) = wrap.first_child() {
                wrap.remove(&chip);
            }
        }
        let mut entries = Vec::new();
        for choice in editor.choices {
            let entry = matches!(choice.kind, QuickMixChoiceKind::Entry { .. });
            let chip = self.chip(choice, editor.can_add, entry);
            if entry {
                entries.push(chip.clone());
            }
            self.choices.append(&chip);
        }
        self.order.set_order(entries.clone());
        self.entries.replace(entries);
        self.playlists_part
            .set_visible(!editor.playlists.is_empty());
        for choice in editor.playlists {
            self.playlists
                .append(&self.chip(choice, editor.can_add, false));
        }
        self.group.set_visible(true);
    }

    /// A chip for `choice`: an entry's check removes it, a + adds the template or the playlist
    /// while another entry fits. A `movable` one is dragged by the handle that takes its glyph's
    /// place under the pointer.
    fn chip(self: &Rc<Self>, choice: QuickMixChoice, can_add: bool, movable: bool) -> gtk::Widget {
        let resource = "preferences/quick_mix_chip.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            chip: gtk::Box,
            glyph: gtk::Image,
            drag: gtk::Image,
            title: gtk::Label,
            remove: gtk::Button,
            add: gtk::Button,
        });
        glyph.set_icon_name(Some(&choice.icon));
        title.set_label(&choice.title);
        if !choice.added {
            chip.add_css_class("faded");
        }
        if movable {
            chip.add_css_class("movable");
            drag.set_visible(true);
        }
        remove.set_visible(choice.added);
        add.set_visible(!choice.added);
        add.set_sensitive(can_add);
        let weak = Rc::downgrade(self);
        match choice.kind {
            QuickMixChoiceKind::Entry { index } => {
                remove.connect_clicked(move |_| {
                    if let Some(editor) = weak.upgrade() {
                        editor.commit(editor.shell.settings.persistence.remove_quick_mix(index));
                    }
                });
            }
            QuickMixChoiceKind::Template { name } => {
                add.connect_clicked(move |_| {
                    if let Some(editor) = weak.upgrade() {
                        let added = editor
                            .shell
                            .settings
                            .persistence
                            .add_quick_mix_template(name.clone());
                        editor.commit(added);
                    }
                });
            }
            QuickMixChoiceKind::Playlist { object_id } => {
                add.connect_clicked(move |_| {
                    let Some(editor) = weak.upgrade() else {
                        return;
                    };
                    let answer = editor
                        .shell
                        .products
                        .source
                        .add_quick_mix_playlist(object_id.clone());
                    let weak = Rc::downgrade(&editor);
                    gtk::glib::spawn_future_local(async move {
                        // Core saves it; Home and the chips follow the saved settings.
                        if let Ok(Err(error)) = answer.recv().await {
                            warn!(%error, "failed to add a smart playlist to Quick mix");
                            if let Some(editor) = weak.upgrade() {
                                editor.refresh();
                            }
                        }
                    });
                });
            }
        }
        chip.upcast()
    }

    /// Commits an edit: Home and the chips follow it, or the chips show Quick mix again when it
    /// changed nothing.
    fn commit(self: &Rc<Self>, result: Result<bool, rufin_core::settings::SettingsError>) {
        if !self.shell.commit_home_blocks(result) {
            self.refresh();
        }
    }
}

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
        "rufin-star-outline-symbolic",
        "rufin-star-outline-symbolic",
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
