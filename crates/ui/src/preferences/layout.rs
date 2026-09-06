pub(crate) fn populate_home_block_rows(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<std::cell::RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        if let Some(row) = row.upgrade() {
            expander.remove(&row);
        }
    }

    let visible_blocks = shell.settings.current.borrow().home_blocks.clone();
    let ordered_blocks = home_block_row_order(&visible_blocks);
    for (position, block) in ordered_blocks.into_iter().enumerate() {
        let active = visible_blocks.contains(&block);
        let visible_index = visible_blocks
            .iter()
            .position(|candidate| *candidate == block);
        let visible_shell = Rc::clone(shell);
        let visible_expander = expander.downgrade();
        let visible_rows = Rc::clone(rows);
        let on_visible = move |requested| {
            let mut blocks = visible_shell.settings.current.borrow().home_blocks.clone();
            if requested == blocks.contains(&block) {
                return;
            }
            if requested {
                let order = home_block_row_order(&blocks);
                insert_home_block_in_order(&mut blocks, block, &order);
            } else if blocks.len() > 1 {
                blocks.retain(|candidate| *candidate != block);
            }
            visible_shell.set_home_blocks(blocks);
            if let Some(expander) = visible_expander.upgrade() {
                populate_home_block_rows(&visible_shell, &expander, &visible_rows);
            }
        };

        let move_shell = Rc::clone(shell);
        let move_expander = expander.downgrade();
        let move_rows = Rc::clone(rows);
        let on_move = move |delta: isize| {
            let mut blocks = move_shell.settings.current.borrow().home_blocks.clone();
            let Some(index) = blocks.iter().position(|candidate| *candidate == block) else {
                return;
            };
            let next = if delta < 0 {
                index.saturating_sub(1)
            } else {
                (index + 1).min(blocks.len().saturating_sub(1))
            };
            if next == index {
                return;
            }
            blocks.swap(index, next);
            move_shell.set_home_blocks(blocks);
            if let Some(expander) = move_expander.upgrade() {
                populate_home_block_rows(&move_shell, &expander, &move_rows);
            }
        };

        let drop_shell = Rc::clone(shell);
        let drop_expander = expander.downgrade();
        let drop_rows = Rc::downgrade(rows);
        let on_drop = move |source_id: String, after| {
            let Some(source_block) = home_block_from_drag_id(&source_id) else {
                return false;
            };
            let (Some(expander), Some(rows)) = (drop_expander.upgrade(), drop_rows.upgrade())
            else {
                return false;
            };
            let mut blocks = drop_shell.settings.current.borrow().home_blocks.clone();
            if !reorder_home_blocks(&mut blocks, source_block, block, after) {
                return false;
            }
            drop_shell.set_home_blocks(blocks);
            populate_home_block_rows(&drop_shell, &expander, &rows);
            true
        };

        let row = reorder_row(
            ReorderRowPresentation {
                title: tr(block.title()),
                subtitle: visibility_position_subtitle(active, position),
                visible: active,
                visible_sensitive: !active || visible_blocks.len() > 1,
                up_sensitive: visible_index.is_some_and(|index| index > 0),
                down_sensitive: visible_index.is_some_and(|index| index + 1 < visible_blocks.len()),
                drag_id: home_block_drag_id(block).to_string(),
            },
            on_visible,
            on_move,
            on_drop,
        );

        expander.add_row(&row);
        rows.borrow_mut().push(row.downgrade());
    }
}
pub(crate) fn home_block_row_order(visible_blocks: &[HomeBlockKind]) -> Vec<HomeBlockKind> {
    let mut blocks = visible_blocks.to_vec();
    for block in HomeBlockKind::all() {
        if !blocks.contains(&block) {
            blocks.push(block);
        }
    }
    blocks
}
pub(crate) fn insert_home_block_in_order(
    blocks: &mut Vec<HomeBlockKind>,
    block: HomeBlockKind,
    order: &[HomeBlockKind],
) {
    if blocks.contains(&block) {
        return;
    }
    let target_order = order
        .iter()
        .position(|candidate| *candidate == block)
        .unwrap_or(usize::MAX);
    let insert_at = blocks
        .iter()
        .position(|candidate| {
            order
                .iter()
                .position(|ordered| ordered == candidate)
                .unwrap_or(usize::MAX)
                > target_order
        })
        .unwrap_or(blocks.len());
    blocks.insert(insert_at, block);
}
pub(crate) fn visibility_position_subtitle(visible: bool, position: usize) -> String {
    let visibility = if visible { tr("Visible") } else { tr("Hidden") };
    format!("{visibility} · {} {}", tr("Position"), position + 1)
}
pub(crate) fn reorder_home_blocks(
    blocks: &mut Vec<HomeBlockKind>,
    source: HomeBlockKind,
    target: HomeBlockKind,
    after: bool,
) -> bool {
    if source == target {
        return false;
    }
    let before = blocks.clone();
    let Some(source_index) = blocks.iter().position(|block| *block == source) else {
        return false;
    };
    let block = blocks.remove(source_index);
    let Some(mut target_index) = blocks.iter().position(|block| *block == target) else {
        blocks.insert(source_index.min(blocks.len()), block);
        return false;
    };
    if after {
        target_index += 1;
    }
    blocks.insert(target_index.min(blocks.len()), block);
    *blocks != before
}
pub(crate) fn home_block_drag_id(block: HomeBlockKind) -> &'static str {
    match block {
        HomeBlockKind::Showcase => "Showcase",
        HomeBlockKind::Explore => "Explore",
        HomeBlockKind::MostPlayed => "MostPlayed",
        HomeBlockKind::NewlyAdded => "NewlyAdded",
        HomeBlockKind::RecentlyPlayed => "RecentlyPlayed",
        HomeBlockKind::RecentlyReleased => "RecentlyReleased",
        HomeBlockKind::Genres => "Genres",
    }
}
pub(crate) fn home_block_from_drag_id(id: &str) -> Option<HomeBlockKind> {
    HomeBlockKind::all()
        .into_iter()
        .find(|block| home_block_drag_id(*block) == id)
}
pub(crate) fn left_sidebar_row<F>(
    title: &str,
    mode: LeftSidebarMode,
    on_selected: F,
) -> adw::ActionRow
where
    F: Fn(u32) + 'static,
{
    selection_row(
        title,
        &[tr("Full"), tr("Compact"), tr("Hidden")],
        left_sidebar_mode_index(mode),
        on_selected,
    )
}
pub(crate) fn left_sidebar_mode_index(mode: LeftSidebarMode) -> u32 {
    match mode {
        LeftSidebarMode::Full => 0,
        LeftSidebarMode::Compact => 1,
        LeftSidebarMode::Hidden => 2,
    }
}
pub(crate) fn left_sidebar_mode_from_index(index: u32) -> LeftSidebarMode {
    match index {
        1 => LeftSidebarMode::Compact,
        2 => LeftSidebarMode::Hidden,
        _ => LeftSidebarMode::Full,
    }
}
pub(crate) fn right_sidebar_row<F>(
    title: &str,
    mode: RightSidebarMode,
    on_selected: F,
) -> adw::ActionRow
where
    F: Fn(u32) + 'static,
{
    selection_row(
        title,
        &[tr("Hidden"), tr("Visible")],
        right_sidebar_mode_index(mode),
        on_selected,
    )
}
pub(crate) fn right_sidebar_mode_index(mode: RightSidebarMode) -> u32 {
    match mode {
        RightSidebarMode::Hidden => 0,
        RightSidebarMode::Visible => 1,
    }
}
pub(crate) fn right_sidebar_mode_from_index(index: u32) -> RightSidebarMode {
    match index {
        1 => RightSidebarMode::Visible,
        _ => RightSidebarMode::Hidden,
    }
}
pub(crate) fn discord_display_index(display_type: DisplayType) -> u32 {
    match display_type {
        DisplayType::Application => 0,
        DisplayType::Song => 1,
        DisplayType::Artist => 2,
    }
}
pub(crate) fn discord_display_from_index(index: u32) -> DisplayType {
    match index {
        1 => DisplayType::Song,
        2 => DisplayType::Artist,
        _ => DisplayType::Application,
    }
}
pub(crate) fn discord_link_index(link_type: LinkType) -> u32 {
    match link_type {
        LinkType::None => 0,
        LinkType::LastFm => 1,
        LinkType::MusicBrainz => 2,
        LinkType::MusicBrainzLastFm => 3,
    }
}
pub(crate) fn discord_link_from_index(index: u32) -> LinkType {
    match index {
        1 => LinkType::LastFm,
        2 => LinkType::MusicBrainz,
        3 => LinkType::MusicBrainzLastFm,
        _ => LinkType::None,
    }
}
use std::rc::Rc;

use super::selection_row;
use crate::settings::HomeBlockKind;
use crate::shell::Shell;
use crate::{LeftSidebarMode, RightSidebarMode};
use adw::prelude::*;
use adw::subclass::prelude::*;
use desktop_integration::{DisplayType, LinkType};
use gtk::{CompositeTemplate, TemplateChild, glib};
use localization::tr;

mod reorder_row_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/preferences/reorder_row.ui")]
    pub(super) struct PreferencesReorderRow {
        #[template_child]
        pub(super) drag: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) up: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) down: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) visible: TemplateChild<gtk::Switch>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesReorderRow {
        const NAME: &'static str = "RufinPreferencesReorderRow";
        type Type = super::PreferencesReorderRow;
        type ParentType = adw::ActionRow;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for PreferencesReorderRow {}
    impl WidgetImpl for PreferencesReorderRow {}
    impl ListBoxRowImpl for PreferencesReorderRow {}
    impl PreferencesRowImpl for PreferencesReorderRow {}
    impl ActionRowImpl for PreferencesReorderRow {}
}

glib::wrapper! {
    struct PreferencesReorderRow(ObjectSubclass<reorder_row_imp::PreferencesReorderRow>)
        @extends gtk::Widget, gtk::ListBoxRow, adw::PreferencesRow, adw::ActionRow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

pub(crate) struct ReorderRowPresentation {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) visible: bool,
    pub(crate) visible_sensitive: bool,
    pub(crate) up_sensitive: bool,
    pub(crate) down_sensitive: bool,
    pub(crate) drag_id: String,
}

pub(crate) fn reorder_row(
    presentation: ReorderRowPresentation,
    on_visible: impl Fn(bool) + 'static,
    on_move: impl Fn(isize) + 'static,
    on_drop: impl Fn(String, bool) -> bool + 'static,
) -> adw::ActionRow {
    let row: PreferencesReorderRow = glib::Object::new();
    row.set_title(&presentation.title);
    row.set_subtitle(&presentation.subtitle);
    let imp = row.imp();
    imp.visible.set_active(presentation.visible);
    imp.visible.set_sensitive(presentation.visible_sensitive);
    imp.up.set_sensitive(presentation.up_sensitive);
    imp.down.set_sensitive(presentation.down_sensitive);
    row.set_activatable_widget(Some(&imp.visible.get()));

    imp.visible
        .connect_active_notify(move |visible| on_visible(visible.is_active()));
    let on_move = Rc::new(on_move);
    let move_up = Rc::clone(&on_move);
    imp.up.connect_clicked(move |_| move_up(-1));
    imp.down.connect_clicked(move |_| on_move(1));

    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(
            &presentation.drag_id.to_value(),
        ))
    });
    imp.drag.add_controller(source);

    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let weak_row = row.downgrade();
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(source_id) = value.get::<String>() else {
            return false;
        };
        let Some(row) = weak_row.upgrade() else {
            return false;
        };
        on_drop(source_id, y > f64::from(row.height()) / 2.0)
    });
    row.add_controller(drop_target);
    row.upcast()
}

#[cfg(test)]
mod reorder_row_tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn shared_reorder_row_template_builds() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("compiled interface resources");
        let row = reorder_row(
            ReorderRowPresentation {
                title: "Title".into(),
                subtitle: "Visible · Position 1".into(),
                visible: true,
                visible_sensitive: true,
                up_sensitive: false,
                down_sensitive: true,
                drag_id: "item".into(),
            },
            |_| {},
            |_| {},
            |_, _| true,
        );
        assert_eq!(row.title(), "Title");
    }
}
