use crate::widgets::forms::selection_row;
pub fn populate_home_block_rows(
    shell: &Rc<Shell>,
    expander: &adw::ExpanderRow,
    rows: &Rc<std::cell::RefCell<Vec<gtk::glib::WeakRef<adw::ActionRow>>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        if let Some(row) = row.upgrade() {
            expander.remove(&row);
        }
    }

    let blocks = shell.settings.persistence.home_block_rows();
    let visible_count = blocks.iter().filter(|row| row.visible).count();
    for (position, home_block) in blocks.into_iter().enumerate() {
        let block = home_block.block;
        let visible_index = home_block.visible.then_some(position);
        // Each edit is core's; the rows read again after it lands.
        let commit = {
            let shell = Rc::clone(shell);
            let expander = expander.downgrade();
            let rows = Rc::downgrade(rows);
            move |result| {
                if !shell.commit_home_blocks(result) {
                    return false;
                }
                if let (Some(expander), Some(rows)) = (expander.upgrade(), rows.upgrade()) {
                    populate_home_block_rows(&shell, &expander, &rows);
                }
                true
            }
        };
        let on_visible = {
            let shell = Rc::clone(shell);
            let commit = commit.clone();
            move |requested| {
                commit(
                    shell
                        .settings
                        .persistence
                        .set_home_block_visible(block, requested),
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
            move |source_id: String, after| {
                let Some(source) = home_block_from_drag_id(&source_id) else {
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
                subtitle: visibility_position_subtitle(home_block.visible, position),
                visible: home_block.visible,
                visible_sensitive: home_block.can_toggle,
                up_sensitive: visible_index.is_some_and(|index| index > 0),
                down_sensitive: visible_index.is_some_and(|index| index + 1 < visible_count),
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
pub fn visibility_position_subtitle(visible: bool, position: usize) -> String {
    let visibility = if visible { tr("Visible") } else { tr("Hidden") };
    format!("{visibility} · {} {}", tr("Position"), position + 1)
}
pub fn home_block_drag_id(block: HomeBlockKind) -> &'static str {
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
pub fn home_block_from_drag_id(id: &str) -> Option<HomeBlockKind> {
    HomeBlockKind::all()
        .into_iter()
        .find(|block| home_block_drag_id(*block) == id)
}
pub fn left_sidebar_row<F>(title: &str, mode: LeftSidebarMode, on_selected: F) -> adw::ActionRow
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
pub fn left_sidebar_mode_index(mode: LeftSidebarMode) -> u32 {
    match mode {
        LeftSidebarMode::Full => 0,
        LeftSidebarMode::Compact => 1,
        LeftSidebarMode::Hidden => 2,
    }
}
pub fn left_sidebar_mode_from_index(index: u32) -> LeftSidebarMode {
    match index {
        1 => LeftSidebarMode::Compact,
        2 => LeftSidebarMode::Hidden,
        _ => LeftSidebarMode::Full,
    }
}
pub fn right_sidebar_row<F>(title: &str, mode: RightSidebarMode, on_selected: F) -> adw::ActionRow
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
pub fn right_sidebar_mode_index(mode: RightSidebarMode) -> u32 {
    match mode {
        RightSidebarMode::Hidden => 0,
        RightSidebarMode::Visible => 1,
    }
}
pub fn right_sidebar_mode_from_index(index: u32) -> RightSidebarMode {
    match index {
        1 => RightSidebarMode::Visible,
        _ => RightSidebarMode::Hidden,
    }
}
pub fn discord_display_index(display_type: DisplayType) -> u32 {
    match display_type {
        DisplayType::Application => 0,
        DisplayType::Track => 1,
        DisplayType::Artist => 2,
    }
}
pub fn discord_display_from_index(index: u32) -> DisplayType {
    match index {
        1 => DisplayType::Track,
        2 => DisplayType::Artist,
        _ => DisplayType::Application,
    }
}
pub fn discord_link_index(link_type: LinkType) -> u32 {
    match link_type {
        LinkType::None => 0,
        LinkType::LastFm => 1,
        LinkType::MusicBrainz => 2,
        LinkType::MusicBrainzLastFm => 3,
    }
}
pub fn discord_link_from_index(index: u32) -> LinkType {
    match index {
        1 => LinkType::LastFm,
        2 => LinkType::MusicBrainz,
        3 => LinkType::MusicBrainzLastFm,
        _ => LinkType::None,
    }
}
use std::rc::Rc;

use crate::shell::shell::Shell;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, TemplateChild, glib};
use localization::tr;
use rufin_core::presence::{DisplayType, LinkType};
use rufin_core::settings::HomeBlockKind;
use rufin_core::settings::{LeftSidebarMode, RightSidebarMode};

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

pub struct ReorderRowPresentation {
    pub title: String,
    pub subtitle: String,
    pub visible: bool,
    pub visible_sensitive: bool,
    pub up_sensitive: bool,
    pub down_sensitive: bool,
    pub drag_id: String,
}

pub fn reorder_row(
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
