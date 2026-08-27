use std::{cell::Cell, rc::Rc};

use crate::player::NOW_PLAYING_RAIL_WIDTH;
use crate::{
    LayoutProfile, LayoutSettings, LeftSidebarMode, MAX_RIGHT_SIDEBAR_WIDTH,
    MIN_LEFT_SIDEBAR_WIDTH, MIN_RIGHT_SIDEBAR_WIDTH, RightSidebarMode,
};
use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use super::Shell;

pub(super) const COMPACT_RAIL_WIDTH: i32 = NOW_PLAYING_RAIL_WIDTH;
pub(super) const LEFT_PANE_SEPARATOR_WIDTH: i32 = 1;
pub(super) const RIGHT_PANE_SEPARATOR_WIDTH: i32 = 1;
pub(super) const NORMAL_SIDEBAR_WIDTH: i32 = crate::DEFAULT_LEFT_SIDEBAR_WIDTH;
pub(crate) const MIN_APP_WINDOW_WIDTH: i32 = 450;
pub(crate) const MIN_USEFUL_MAIN_WIDTH: i32 = MIN_APP_WINDOW_WIDTH;
pub(super) const MIN_APP_WINDOW_HEIGHT: i32 = 400;
pub(crate) const WINDOW_CHROME_MARGIN_END: i32 = 8;

pub(crate) const MIN_RESTORED_WINDOW_HEIGHT: i32 = MIN_APP_WINDOW_HEIGHT;
const LEFT_SIDEBAR_COLLAPSE_DETENT: i32 = crate::MIN_LEFT_SIDEBAR_WIDTH - 40;
const LEFT_SIDEBAR_EXPAND_DETENT: i32 = crate::MIN_LEFT_SIDEBAR_WIDTH;
type ShellAllocationCallback = Rc<dyn Fn(i32, i32)>;

mod shell_allocation_owner_imp {
    use std::{cell::RefCell, rc::Rc};

    use gtk::{glib, prelude::*, subclass::prelude::*};

    type AllocationCallback = Rc<dyn Fn(i32, i32)>;

    #[derive(Default)]
    pub struct ShellAllocationOwner {
        pub(super) before_allocate: RefCell<Option<AllocationCallback>>,
        pub(super) after_allocate: RefCell<Option<AllocationCallback>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ShellAllocationOwner {
        const NAME: &'static str = "RufinShellAllocationOwner";
        type Type = super::ShellAllocationOwner;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ShellAllocationOwner {
        fn dispose(&self) {
            self.before_allocate.take();
            self.after_allocate.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ShellAllocationOwner {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.obj()
                .first_child()
                .map(|child| child.request_mode())
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.obj()
                .first_child()
                .map(|child| child.measure(orientation, for_size))
                .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let before_allocate = self.before_allocate.borrow().clone();
            if let Some(before_allocate) = before_allocate {
                before_allocate(width, height);
            }

            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }

            let after_allocate = self.after_allocate.borrow().clone();
            if let Some(after_allocate) = after_allocate {
                after_allocate(width, height);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

gtk::glib::wrapper! {
    pub struct ShellAllocationOwner(ObjectSubclass<shell_allocation_owner_imp::ShellAllocationOwner>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ShellAllocationOwner {
    fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let owner: Self = glib::Object::new();
        owner.set_hexpand(true);
        owner.set_vexpand(true);
        owner.set_halign(gtk::Align::Fill);
        owner.set_valign(gtk::Align::Fill);
        owner.set_accessible_role(gtk::AccessibleRole::Presentation);
        child.set_parent(&owner);
        owner
    }

    fn set_before_allocate(&self, before_allocate: impl Fn(i32, i32) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;

        self.imp()
            .before_allocate
            .replace(Some(Rc::new(before_allocate) as ShellAllocationCallback));
    }

    fn set_after_allocate(&self, after_allocate: impl Fn(i32, i32) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;

        self.imp()
            .after_allocate
            .replace(Some(Rc::new(after_allocate) as ShellAllocationCallback));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LeftSidebarDragPreview {
    mode: LeftSidebarMode,
    width: i32,
}

pub(super) struct ShellLayoutState {
    pub(super) owner: ShellAllocationOwner,
    left_drag_preview: Cell<Option<LeftSidebarDragPreview>>,
    right_drag_width: Cell<Option<i32>>,
}

impl ShellLayoutState {
    pub(super) fn new(root_stack: &gtk::Stack) -> Self {
        Self {
            owner: ShellAllocationOwner::new(root_stack),
            left_drag_preview: Cell::new(None),
            right_drag_width: Cell::new(None),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveLayoutProfile {
    Default,
    Narrow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedLeftSidebarMode {
    Full,
    Compact,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedLayout {
    pub(crate) profile: ActiveLayoutProfile,
    pub(super) left_sidebar: ResolvedLeftSidebarMode,
    pub(super) left_sidebar_width: i32,
    pub(super) right_sidebar: RightSidebarMode,
    pub(super) right_sidebar_width: i32,
    pub(super) main_width: i32,
}

fn configured_sidebar_width(mode: LeftSidebarMode, preferred_width: i32) -> i32 {
    match mode {
        LeftSidebarMode::Full => preferred_width,
        LeftSidebarMode::Compact => COMPACT_RAIL_WIDTH,
        LeftSidebarMode::Hidden => 0,
    }
}

fn fitted_right_sidebar_width(available: i32, requested: i32) -> Option<i32> {
    (available >= MIN_RIGHT_SIDEBAR_WIDTH).then(|| {
        requested
            .clamp(MIN_RIGHT_SIDEBAR_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH)
            .min(available)
    })
}

pub(crate) fn resolve_layout(settings: &LayoutSettings, window_width: i32) -> ResolvedLayout {
    let window_width = window_width.max(MIN_APP_WINDOW_WIDTH);
    let (profile, configured) =
        if settings.narrow_enabled && window_width < settings.narrow_threshold {
            (ActiveLayoutProfile::Narrow, &settings.narrow_profile)
        } else {
            (ActiveLayoutProfile::Default, &settings.default_profile)
        };
    resolve_layout_for_profile(profile, configured, settings, window_width)
}

fn resolve_layout_for_profile(
    profile: ActiveLayoutProfile,
    configured: &LayoutProfile,
    settings: &LayoutSettings,
    window_width: i32,
) -> ResolvedLayout {
    let mut left_sidebar = match configured.left_sidebar {
        LeftSidebarMode::Full => ResolvedLeftSidebarMode::Full,
        LeftSidebarMode::Compact => ResolvedLeftSidebarMode::Compact,
        LeftSidebarMode::Hidden => ResolvedLeftSidebarMode::Hidden,
    };
    let preferred_left_width = settings.preferred_left_sidebar_width;
    let mut left_sidebar_width =
        configured_sidebar_width(configured.left_sidebar, preferred_left_width);
    let mut right_sidebar = configured.right_sidebar;
    let mut right_sidebar_width = 0;

    if right_sidebar.is_visible() {
        let fit_right = |left_sidebar: ResolvedLeftSidebarMode, left_sidebar_width: i32| {
            let left_separator_width = if left_sidebar == ResolvedLeftSidebarMode::Hidden {
                0
            } else {
                LEFT_PANE_SEPARATOR_WIDTH
            };
            let available = window_width
                - left_sidebar_width
                - left_separator_width
                - MIN_USEFUL_MAIN_WIDTH
                - RIGHT_PANE_SEPARATOR_WIDTH;
            fitted_right_sidebar_width(available, settings.preferred_right_sidebar_width)
        };
        if let Some(width) = fit_right(left_sidebar, left_sidebar_width) {
            right_sidebar_width = width;
        } else if left_sidebar == ResolvedLeftSidebarMode::Full
            && let Some(width) = fit_right(ResolvedLeftSidebarMode::Compact, COMPACT_RAIL_WIDTH)
        {
            left_sidebar = ResolvedLeftSidebarMode::Compact;
            left_sidebar_width = COMPACT_RAIL_WIDTH;
            right_sidebar_width = width;
        } else {
            right_sidebar = RightSidebarMode::Hidden;
        }
    }

    if right_sidebar == RightSidebarMode::Hidden && left_sidebar == ResolvedLeftSidebarMode::Full {
        let available = window_width - MIN_APP_WINDOW_WIDTH - LEFT_PANE_SEPARATOR_WIDTH;
        if available >= MIN_LEFT_SIDEBAR_WIDTH {
            left_sidebar_width = preferred_left_width.min(available);
        } else {
            left_sidebar = ResolvedLeftSidebarMode::Compact;
            left_sidebar_width = COMPACT_RAIL_WIDTH;
        }
    }
    if left_sidebar == ResolvedLeftSidebarMode::Compact
        && window_width - left_sidebar_width - LEFT_PANE_SEPARATOR_WIDTH < MIN_APP_WINDOW_WIDTH
    {
        left_sidebar = ResolvedLeftSidebarMode::Hidden;
        right_sidebar = RightSidebarMode::Hidden;
        left_sidebar_width = 0;
        right_sidebar_width = 0;
    }
    let separator_width = if right_sidebar.is_visible() {
        RIGHT_PANE_SEPARATOR_WIDTH
    } else {
        0
    };
    let left_separator_width = if left_sidebar == ResolvedLeftSidebarMode::Hidden {
        0
    } else {
        LEFT_PANE_SEPARATOR_WIDTH
    };
    let main_width = window_width
        - left_sidebar_width
        - left_separator_width
        - right_sidebar_width
        - separator_width;
    ResolvedLayout {
        profile,
        left_sidebar,
        left_sidebar_width,
        right_sidebar,
        right_sidebar_width,
        main_width: main_width.max(1),
    }
}

#[cfg(test)]
fn right_sidebar_allocation_position(split_width: i32, requested_right_width: i32) -> Option<i32> {
    let available_right = split_width - MIN_USEFUL_MAIN_WIDTH - RIGHT_PANE_SEPARATOR_WIDTH;
    let right_width = fitted_right_sidebar_width(available_right, requested_right_width)?;
    Some(split_width - right_width - RIGHT_PANE_SEPARATOR_WIDTH)
}

#[cfg(test)]
fn resolve_left_sidebar_drag_preview(
    settings: &LayoutSettings,
    window_width: i32,
    mode: LeftSidebarMode,
    width: i32,
) -> ResolvedLayout {
    resolve_layout_with_drag_previews(
        settings,
        window_width,
        Some(LeftSidebarDragPreview { mode, width }),
        None,
    )
}

fn resolve_layout_with_drag_previews(
    settings: &LayoutSettings,
    window_width: i32,
    left_drag: Option<LeftSidebarDragPreview>,
    right_drag_width: Option<i32>,
) -> ResolvedLayout {
    let mut settings = settings.clone();
    if let Some(right_drag_width) = right_drag_width {
        settings.preferred_right_sidebar_width = right_drag_width;
    }
    if let Some(left_drag) = left_drag {
        let active_profile = resolve_layout(&settings, window_width).profile;
        let profile = match active_profile {
            ActiveLayoutProfile::Default => &mut settings.default_profile,
            ActiveLayoutProfile::Narrow => &mut settings.narrow_profile,
        };
        profile.left_sidebar = left_drag.mode;
        settings.preferred_left_sidebar_width = left_drag.width;
    }
    settings.sanitize();
    resolve_layout(&settings, window_width)
}

fn right_sidebar_width_after_drag_update(
    settings: &LayoutSettings,
    window_width: i32,
    left_drag: Option<LeftSidebarDragPreview>,
    current_width: f64,
    previous_offset: f64,
    offset: f64,
) -> f64 {
    let maximum = resolve_layout_with_drag_previews(
        settings,
        window_width,
        left_drag,
        Some(MAX_RIGHT_SIDEBAR_WIDTH),
    );
    if !maximum.right_sidebar.is_visible() {
        return current_width;
    }

    let minimum = f64::from(MIN_RIGHT_SIDEBAR_WIDTH);
    let maximum = f64::from(maximum.right_sidebar_width);
    let offset_delta = offset - previous_offset;
    (current_width.clamp(minimum, maximum) - offset_delta).clamp(minimum, maximum)
}

pub(crate) fn route_content_width(shell: &Shell) -> i32 {
    let allocated = shell.route_viewport.route_host.width();
    if allocated > 1 {
        return allocated;
    }

    resolve_layout(
        &shell.settings.current.borrow().layout,
        shell.layout_width(),
    )
    .main_width
    .max(1)
}

impl Shell {
    pub(crate) fn left_sidebar_mode(&self) -> ResolvedLeftSidebarMode {
        if !self.navigation_view.split_view.is_collapsed() {
            ResolvedLeftSidebarMode::Full
        } else if self.navigation_view.compact_nav_slot.get_visible() {
            ResolvedLeftSidebarMode::Compact
        } else {
            ResolvedLeftSidebarMode::Hidden
        }
    }

    pub(crate) fn right_sidebar_visible(&self) -> bool {
        self.right_panel.right_panel_slot.get_visible()
    }

    pub(crate) fn fullscreen_player_visible(&self) -> bool {
        self.player_view.fullscreen_player.visible.get()
    }

    pub(crate) fn update_layout(self: &Rc<Self>) {
        self.layout_state.owner.queue_allocate();
    }

    fn apply_layout_allocation(self: &Rc<Self>, width: i32, _height: i32) {
        let settings = self.settings.current.borrow().layout.clone();
        let left_drag_preview = self.layout_state.left_drag_preview.get();
        let right_drag_width = self.layout_state.right_drag_width.get();
        let resolved = resolve_layout_with_drag_previews(
            &settings,
            width,
            left_drag_preview,
            right_drag_width,
        );
        self.apply_resolved_layout_for_allocation(resolved);
    }

    fn apply_resolved_layout_for_allocation(self: &Rc<Self>, resolved: ResolvedLayout) {
        let login_active = self.source.login_screen_active();
        let presentation = root_presentation(login_active, self.startup.route_revealed.get());
        let previous_left = self.left_sidebar_mode();
        let previous_right_visible = self.right_sidebar_visible();

        let app_active = presentation.app_active;
        let full_sidebar = resolved.left_sidebar == ResolvedLeftSidebarMode::Full;
        let hidden_sidebar = resolved.left_sidebar == ResolvedLeftSidebarMode::Hidden;
        let right_visible = app_active && resolved.right_sidebar.is_visible();
        self.chrome
            .window_controls
            .set_full_controls_allowed(!app_active || full_sidebar, !app_active || right_visible);
        self.chrome.window_controls.set_compact_start_alignment(
            app_active && resolved.left_sidebar == ResolvedLeftSidebarMode::Compact,
        );
        let overlay_sidebar_width = if hidden_sidebar {
            self.settings
                .current
                .borrow()
                .layout
                .preferred_left_sidebar_width
        } else {
            resolved.left_sidebar_width
        };
        self.preview_left_sidebar_width(overlay_sidebar_width);
        if app_active && self.fullscreen_player_visible() {
            self.refresh_fullscreen_player_layout();
        }

        let root_changed =
            self.chrome.root_stack.visible_child_name().as_deref() != Some(presentation.root_page);
        if root_changed {
            self.chrome
                .root_stack
                .set_visible_child_name(presentation.root_page);
        }
        set_widget_visible(
            &self.chrome.startup_loading_host,
            presentation.startup_loading,
        );
        if self
            .chrome
            .app_content_stack
            .visible_child_name()
            .as_deref()
            != Some("main")
        {
            self.chrome.app_content_stack.set_visible_child_name("main");
        }
        self.apply_fullscreen_topology(app_active);

        let collapsed = !app_active || !full_sidebar;
        let show_sidebar = app_active && full_sidebar;
        if show_sidebar {
            if !self.navigation_view.split_view.shows_sidebar() {
                self.navigation_view.split_view.set_show_sidebar(true);
            }
            if self.navigation_view.split_view.is_collapsed() {
                self.navigation_view.split_view.set_collapsed(false);
            }
        } else {
            // While the responsive presentation is hidden, show-sidebar is
            // transient overlay state owned by the floating button and
            // libadwaita. Rewriting it during allocation cancels its reveal.
            if (!app_active || !hidden_sidebar) && self.navigation_view.split_view.shows_sidebar() {
                self.navigation_view.split_view.set_show_sidebar(false);
            }
            if self.navigation_view.split_view.is_collapsed() != collapsed {
                self.navigation_view.split_view.set_collapsed(collapsed);
            }
        }
        set_widget_visible(&self.navigation_view.normal_nav_panel, app_active);
        set_widget_visible(
            &self.navigation_view.compact_nav_slot,
            app_active && resolved.left_sidebar == ResolvedLeftSidebarMode::Compact,
        );
        set_widget_visible(
            &self.navigation_view.tiny_nav_button,
            app_active && hidden_sidebar,
        );
        set_widget_visible(
            &self.navigation_view.left_resize_handle,
            app_active && !hidden_sidebar,
        );
        let right_visibility_changed = previous_right_visible != right_visible;
        set_widget_visible(&self.right_panel.right_panel_slot, right_visible);
        set_widget_visible(&self.right_panel.root, right_visible);
        set_widget_visible(&self.right_panel.right_resize_handle, right_visible);
        if self.right_panel.right_split.position() != resolved.main_width {
            self.right_panel
                .right_split
                .set_position(resolved.main_width);
        }
        set_widget_visible(&self.player_view.player_controls.root, app_active);

        if right_visibility_changed || previous_right_visible != right_visible {
            self.update_right_panel_button();
            self.sync_visualizer_state();
            let lyrics_shell = Rc::clone(self);
            glib::idle_add_local_once(move || {
                if lyrics_shell.right_lyrics_surface_visible() {
                    lyrics_shell.sync_visible_lyrics_surfaces();
                } else {
                    lyrics_shell.update_lyrics_highlight();
                }
            });
        }
        if previous_left != resolved.left_sidebar || previous_right_visible != right_visible {
            debug!(?resolved, "resolved layout changed");
        }
    }

    fn apply_fullscreen_topology(&self, app_active: bool) {
        let fullscreen = &self.player_view.fullscreen_player;
        if !app_active {
            fullscreen.visible.set(false);
            if let Some(tick) = fullscreen.animation_tick.borrow_mut().take() {
                tick.remove();
            }
            if fullscreen.root.margin_top() != 0 {
                fullscreen.root.set_margin_top(0);
            }
            if fullscreen.root.opacity() != 0.0 {
                fullscreen.root.set_opacity(0.0);
            }
            if fullscreen.root.can_target() {
                fullscreen.root.set_can_target(false);
            }
            if fullscreen.root.is_sensitive() {
                fullscreen.root.set_sensitive(false);
            }
            set_widget_visible(&fullscreen.root, false);
            return;
        }

        if fullscreen.animation_tick.borrow().is_some() {
            return;
        }
        let visible = fullscreen.visible.get();
        if fullscreen.root.margin_top() != 0 {
            fullscreen.root.set_margin_top(0);
        }
        let opacity = if visible { 1.0 } else { 0.0 };
        if fullscreen.root.opacity() != opacity {
            fullscreen.root.set_opacity(opacity);
        }
        if fullscreen.root.can_target() != visible {
            fullscreen.root.set_can_target(visible);
        }
        if fullscreen.root.is_sensitive() != visible {
            fullscreen.root.set_sensitive(visible);
        }
        set_widget_visible(&fullscreen.root, visible);
    }

    pub(crate) fn layout_width(&self) -> i32 {
        let root_width = self.chrome.root_stack.width();
        if root_width > 1 {
            return root_width;
        }

        let window_width = self.chrome.window.width();
        if window_width > 1 {
            return window_width;
        }

        self.chrome
            .window
            .surface()
            .map(|surface| surface.width())
            .filter(|width| *width > 1)
            .unwrap_or(1)
    }

    fn set_left_sidebar_drag_preview(self: &Rc<Self>, mode: LeftSidebarMode, width: i32) {
        self.layout_state
            .left_drag_preview
            .set(Some(LeftSidebarDragPreview { mode, width }));
        self.update_layout();
    }

    fn clear_left_sidebar_drag_preview(self: &Rc<Self>) {
        if self.layout_state.left_drag_preview.take().is_some() {
            self.update_layout();
        }
    }

    fn current_right_sidebar_width(&self) -> i32 {
        let split = &self.right_panel.right_split;
        let allocated_width = split
            .width()
            .saturating_sub(split.position())
            .saturating_sub(RIGHT_PANE_SEPARATOR_WIDTH);
        if self.right_sidebar_visible() && allocated_width > 0 {
            return allocated_width.clamp(MIN_RIGHT_SIDEBAR_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH);
        }

        self.settings
            .current
            .borrow()
            .layout
            .preferred_right_sidebar_width
            .clamp(MIN_RIGHT_SIDEBAR_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH)
    }

    pub(crate) fn preview_left_sidebar_width(&self, width: i32) {
        let width = width.max(1);
        self.navigation_view
            .split_view
            .set_min_sidebar_width(f64::from(width));
        self.navigation_view
            .split_view
            .set_max_sidebar_width(f64::from(width));
        position_left_resize_handle(&self.navigation_view.left_resize_handle, width);
    }

    fn sync_left_resize_handle_to_allocation(&self) {
        let width = match self.left_sidebar_mode() {
            ResolvedLeftSidebarMode::Full => self.navigation_view.normal_nav_panel.width(),
            ResolvedLeftSidebarMode::Compact => self.navigation_view.compact_nav_slot.width(),
            ResolvedLeftSidebarMode::Hidden => 0,
        };
        position_left_resize_handle(&self.navigation_view.left_resize_handle, width);
    }
}

fn position_left_resize_handle(handle: &gtk::Box, sidebar_width: i32) {
    handle.set_margin_start((sidebar_width - 4).max(0));
}

fn set_widget_visible(widget: &impl IsA<gtk::Widget>, visible: bool) {
    if widget.get_visible() != visible {
        widget.set_visible(visible);
    }
}

pub(crate) fn startup_loading_screen_active(
    login_active: bool,
    startup_route_revealed: bool,
) -> bool {
    !login_active && !startup_route_revealed
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootPresentation {
    root_page: &'static str,
    app_active: bool,
    startup_loading: bool,
}

fn root_presentation(login_active: bool, startup_route_revealed: bool) -> RootPresentation {
    RootPresentation {
        root_page: if login_active { "login" } else { "app" },
        app_active: !login_active,
        startup_loading: startup_loading_screen_active(login_active, startup_route_revealed),
    }
}

fn connect_left_sidebar_resize(shell: &Rc<Shell>) {
    let start_width = Rc::new(Cell::new(COMPACT_RAIL_WIDTH));
    let start_mode = Rc::new(Cell::new(LeftSidebarMode::Compact));
    let live_width = Rc::new(Cell::new(COMPACT_RAIL_WIDTH));
    let live_mode = Rc::new(Cell::new(LeftSidebarMode::Compact));
    let active = Rc::new(Cell::new(false));
    let drag = gtk::GestureDrag::new();
    drag.set_button(1);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let drag_shell = Rc::clone(shell);
    let drag_start_width = Rc::clone(&start_width);
    let drag_start_mode = Rc::clone(&start_mode);
    let drag_live_width = Rc::clone(&live_width);
    let drag_live_mode = Rc::clone(&live_mode);
    let drag_active = Rc::clone(&active);
    drag.connect_drag_begin(move |gesture, start_x, start_y| {
        drag_active.set(false);
        drag_shell.clear_left_sidebar_drag_preview();
        let handle = &drag_shell.navigation_view.left_resize_handle;
        let handle_start = f64::from(handle.margin_start());
        let handle_end = handle_start + f64::from(handle.width().max(8));
        if !handle.is_mapped()
            || drag_shell.fullscreen_player_visible()
            || start_x < handle_start
            || start_x > handle_end
            || start_y < 0.0
            || start_y > f64::from(handle.height())
        {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }

        drag_active.set(true);
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let width = resolve_layout(
            &drag_shell.settings.current.borrow().layout,
            drag_shell.layout_width(),
        )
        .left_sidebar_width;
        drag_start_width.set(width);
        drag_live_width.set(width);
        let mode = match drag_shell.left_sidebar_mode() {
            ResolvedLeftSidebarMode::Full => LeftSidebarMode::Full,
            ResolvedLeftSidebarMode::Compact => LeftSidebarMode::Compact,
            ResolvedLeftSidebarMode::Hidden => LeftSidebarMode::Hidden,
        };
        drag_start_mode.set(mode);
        drag_live_mode.set(mode);
        drag_shell.set_left_sidebar_drag_preview(mode, width);
    });
    let drag_shell = Rc::clone(shell);
    let drag_start_width = Rc::clone(&start_width);
    let drag_live_width = Rc::clone(&live_width);
    let drag_live_mode = Rc::clone(&live_mode);
    let drag_active = Rc::clone(&active);
    drag.connect_drag_update(move |gesture, offset_x, _| {
        if !drag_active.get() {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let requested = drag_start_width
            .get()
            .saturating_add(offset_x.round() as i32);
        let stays_compact = drag_live_mode.get() == LeftSidebarMode::Compact
            && requested < LEFT_SIDEBAR_EXPAND_DETENT;
        if requested < LEFT_SIDEBAR_COLLAPSE_DETENT || stays_compact {
            drag_live_mode.set(LeftSidebarMode::Compact);
            drag_live_width.set(COMPACT_RAIL_WIDTH);
            drag_shell.set_left_sidebar_drag_preview(LeftSidebarMode::Compact, COMPACT_RAIL_WIDTH);
            return;
        }

        let width = requested.clamp(crate::MIN_LEFT_SIDEBAR_WIDTH, crate::MAX_LEFT_SIDEBAR_WIDTH);
        drag_live_mode.set(LeftSidebarMode::Full);
        drag_live_width.set(width);
        drag_shell.set_left_sidebar_drag_preview(LeftSidebarMode::Full, width);
    });
    let drag_shell = Rc::clone(shell);
    let drag_start_width = Rc::clone(&start_width);
    let drag_start_mode = Rc::clone(&start_mode);
    let drag_live_width = Rc::clone(&live_width);
    let drag_live_mode = Rc::clone(&live_mode);
    let drag_active = Rc::clone(&active);
    drag.connect_drag_end(move |_, _, _| {
        if !drag_active.replace(false) {
            return;
        }
        let end_mode = drag_live_mode.get();
        let end_width = drag_live_width.get();
        drag_shell.layout_state.left_drag_preview.set(None);
        if left_sidebar_drag_changed(
            drag_start_mode.get(),
            drag_start_width.get(),
            end_mode,
            end_width,
        ) {
            drag_shell.save_left_sidebar_drag(end_mode, end_width);
        } else {
            drag_shell.update_layout();
        }
    });
    let cancel_shell = Rc::clone(shell);
    let cancel_active = Rc::clone(&active);
    drag.connect_cancel(move |_, _| {
        if cancel_active.replace(false) {
            cancel_shell.clear_left_sidebar_drag_preview();
        }
    });
    shell
        .navigation_view
        .left_resize_handle
        .parent()
        .expect("left resize handle must be mounted in its stable overlay")
        .add_controller(drag);
}

fn left_sidebar_drag_changed(
    start_mode: LeftSidebarMode,
    start_width: i32,
    end_mode: LeftSidebarMode,
    end_width: i32,
) -> bool {
    start_mode != end_mode || start_width != end_width
}

fn connect_right_sidebar_resize(shell: &Rc<Shell>) {
    let drag_start_width = Rc::new(Cell::new(None));
    let drag_last_offset = Rc::new(Cell::new(0.0));
    let drag_exact_width = Rc::new(Cell::new(None::<f64>));
    let position_shell = Rc::clone(shell);
    shell
        .right_panel
        .right_split
        .connect_position_notify(move |split| {
            position_right_resize_handle(
                &position_shell.right_panel.right_resize_handle,
                split.position(),
            );
        });

    let drag = gtk::GestureDrag::new();
    drag.set_button(1);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let drag_start = Rc::clone(&drag_start_width);
    let drag_offset = Rc::clone(&drag_last_offset);
    let drag_exact = Rc::clone(&drag_exact_width);
    let drag_shell = Rc::clone(shell);
    drag.connect_drag_begin(move |gesture, start_x, start_y| {
        drag_start.set(None);
        drag_offset.set(0.0);
        drag_exact.set(None);
        if drag_shell.layout_state.right_drag_width.take().is_some() {
            drag_shell.update_layout();
        }
        let handle = &drag_shell.right_panel.right_resize_handle;
        let handle_start = f64::from(handle.margin_start());
        let hits_handle = right_sidebar_handle_hit(handle_start, handle.width().max(1), start_x);
        if !handle.is_mapped()
            || !drag_shell.right_sidebar_visible()
            || drag_shell.fullscreen_player_visible()
            || !hits_handle
            || start_y < 0.0
            || start_y > f64::from(handle.height())
        {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }

        gesture.set_state(gtk::EventSequenceState::Claimed);
        let width = drag_shell.current_right_sidebar_width();
        drag_start.set(Some(width));
        drag_exact.set(Some(f64::from(width)));
        drag_shell.layout_state.right_drag_width.set(Some(width));
    });
    let drag_start = Rc::clone(&drag_start_width);
    let drag_offset = Rc::clone(&drag_last_offset);
    let drag_exact = Rc::clone(&drag_exact_width);
    let drag_shell = Rc::clone(shell);
    drag.connect_drag_update(move |_, offset_x, _| {
        let Some(_start_width) = drag_start.get() else {
            return;
        };
        let previous_offset = drag_offset.replace(offset_x);
        let current_width = drag_exact.get().unwrap_or_else(|| {
            f64::from(
                drag_shell
                    .layout_state
                    .right_drag_width
                    .get()
                    .unwrap_or_else(|| drag_shell.current_right_sidebar_width()),
            )
        });
        let exact_width = right_sidebar_width_after_drag_update(
            &drag_shell.settings.current.borrow().layout,
            drag_shell.layout_width(),
            drag_shell.layout_state.left_drag_preview.get(),
            current_width,
            previous_offset,
            offset_x,
        );
        drag_exact.set(Some(exact_width));
        let requested_width = exact_width.round() as i32;
        drag_shell
            .layout_state
            .right_drag_width
            .set(Some(requested_width));
        drag_shell.update_layout();
    });
    let drag_end_start = Rc::clone(&drag_start_width);
    let drag_end_exact = Rc::clone(&drag_exact_width);
    let drag_end_shell = Rc::clone(shell);
    drag.connect_drag_end(move |_, _, _| {
        drag_end_exact.set(None);
        let Some(start_width) = drag_end_start.take() else {
            return;
        };
        let end_width = drag_end_shell
            .layout_state
            .right_drag_width
            .take()
            .unwrap_or(start_width);
        if end_width != start_width {
            drag_end_shell.save_preferred_right_sidebar_width(end_width);
        }
        drag_end_shell.update_layout();
    });
    let cancel_start = Rc::clone(&drag_start_width);
    let cancel_exact = Rc::clone(&drag_exact_width);
    let cancel_shell = Rc::clone(shell);
    drag.connect_cancel(move |_, _| {
        cancel_exact.set(None);
        if cancel_start.take().is_some() {
            cancel_shell.layout_state.right_drag_width.set(None);
            cancel_shell.update_layout();
        }
    });
    shell
        .right_panel
        .right_resize_handle
        .parent()
        .expect("right resize handle must be mounted in its stable overlay")
        .add_controller(drag);
}

fn position_right_resize_handle(handle: &gtk::Box, position: i32) {
    // Keep the locked four-pixel target, including the visible one-pixel
    // separator and three transparent pixels inside the right pane.
    handle.set_margin_start(position.max(0));
}

fn right_sidebar_handle_hit(start: f64, width: i32, x: f64) -> bool {
    start <= x && x < start + f64::from(width.max(1))
}

fn connect_layout_allocation_owner(shell: &Rc<Shell>) {
    let owner = shell.layout_state.owner.clone();
    let before_shell = Rc::downgrade(shell);
    owner.set_before_allocate(move |width, height| {
        if let Some(shell) = before_shell.upgrade() {
            shell.apply_layout_allocation(width, height);
        }
    });
    let after_shell = Rc::downgrade(shell);
    owner.set_after_allocate(move |width, _| {
        if let Some(shell) = after_shell.upgrade() {
            shell.sync_left_resize_handle_to_allocation();
            shell.finish_startup_route_allocation(width);
        }
    });
}

pub(super) fn connect_shell_layout(shell: &Rc<Shell>) {
    connect_layout_allocation_owner(shell);
    connect_left_sidebar_resize(shell);
    connect_right_sidebar_resize(shell);
}

#[cfg(test)]
mod tests {
    use crate::{LayoutSettings, LeftSidebarMode, RightSidebarMode};

    use super::*;

    #[test]
    fn preparing_library_keeps_the_app_allocated_under_the_overlay() {
        assert_eq!(
            root_presentation(false, false),
            RootPresentation {
                root_page: "app",
                app_active: true,
                startup_loading: true,
            }
        );
        assert_eq!(
            root_presentation(false, true),
            RootPresentation {
                root_page: "app",
                app_active: true,
                startup_loading: false,
            }
        );
        assert_eq!(
            root_presentation(true, false),
            RootPresentation {
                root_page: "login",
                app_active: false,
                startup_loading: false,
            }
        );
    }

    #[test]
    fn layout_compacts_left_before_hiding_requested_right_sidebar() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.preferred_right_sidebar_width = 500;

        let fitted = resolve_layout(
            &settings,
            settings.preferred_left_sidebar_width
                + LEFT_PANE_SEPARATOR_WIDTH
                + MIN_USEFUL_MAIN_WIDTH
                + 400
                + RIGHT_PANE_SEPARATOR_WIDTH,
        );
        let compact = resolve_layout(
            &settings,
            settings.preferred_left_sidebar_width + MIN_USEFUL_MAIN_WIDTH + 249,
        );
        let hidden = resolve_layout(
            &settings,
            COMPACT_RAIL_WIDTH
                + LEFT_PANE_SEPARATOR_WIDTH
                + MIN_USEFUL_MAIN_WIDTH
                + MIN_RIGHT_SIDEBAR_WIDTH
                + RIGHT_PANE_SEPARATOR_WIDTH
                - 1,
        );

        assert_eq!(fitted.left_sidebar, ResolvedLeftSidebarMode::Full);
        assert_eq!(fitted.right_sidebar, RightSidebarMode::Visible);
        assert_eq!(fitted.right_sidebar_width, 400);
        assert_eq!(fitted.main_width, MIN_USEFUL_MAIN_WIDTH);
        assert_eq!(compact.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(compact.right_sidebar, RightSidebarMode::Visible);
        assert_eq!(hidden.right_sidebar, RightSidebarMode::Hidden);
        assert_eq!(settings.default_profile.left_sidebar, LeftSidebarMode::Full);
        assert_eq!(settings.preferred_right_sidebar_width, 500);
    }

    #[test]
    fn right_sidebar_drag_preserves_both_usable_panes() {
        let split_width =
            MIN_USEFUL_MAIN_WIDTH + MIN_RIGHT_SIDEBAR_WIDTH + RIGHT_PANE_SEPARATOR_WIDTH + 200;

        assert_eq!(
            right_sidebar_allocation_position(split_width, MAX_RIGHT_SIDEBAR_WIDTH),
            Some(MIN_USEFUL_MAIN_WIDTH)
        );
        assert_eq!(
            right_sidebar_allocation_position(split_width, MIN_RIGHT_SIDEBAR_WIDTH),
            Some(split_width - MIN_RIGHT_SIDEBAR_WIDTH - RIGHT_PANE_SEPARATOR_WIDTH)
        );
        assert_eq!(
            right_sidebar_allocation_position(
                MIN_USEFUL_MAIN_WIDTH + MIN_RIGHT_SIDEBAR_WIDTH + RIGHT_PANE_SEPARATOR_WIDTH - 1,
                MIN_RIGHT_SIDEBAR_WIDTH,
            ),
            None
        );
    }

    #[test]
    fn right_resize_target_is_four_pixels_including_the_visible_separator() {
        let separator_position = 450;
        let handle_start = f64::from(separator_position);
        let handle_width = super::super::chrome::RIGHT_RESIZE_HANDLE_WIDTH;

        assert!(!right_sidebar_handle_hit(
            handle_start,
            handle_width,
            handle_start - 0.1
        ));
        assert!(right_sidebar_handle_hit(
            handle_start,
            handle_width,
            handle_start
        ));
        assert!(right_sidebar_handle_hit(
            handle_start,
            handle_width,
            handle_start + 3.9
        ));
        assert!(!right_sidebar_handle_hit(
            handle_start,
            handle_width,
            handle_start + 4.0
        ));
    }

    #[test]
    fn right_drag_reverses_immediately_after_hitting_the_capacity_limit() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Compact;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;

        let expanded =
            right_sidebar_width_after_drag_update(&settings, 968, None, 300.0, 0.0, -200.0);
        assert_eq!(expanded, 440.0);

        let reversed =
            right_sidebar_width_after_drag_update(&settings, 968, None, expanded, -200.0, -199.0);
        assert_eq!(reversed, 439.0);
    }

    #[test]
    fn right_drag_accumulates_subpixel_motion() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Compact;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;

        let mut previous_offset = 0.0;
        let width = (1..=5).fold(300.0, |width, step| {
            let offset = f64::from(step) * 0.2;
            let width = right_sidebar_width_after_drag_update(
                &settings,
                948,
                None,
                width,
                previous_offset,
                offset,
            );
            previous_offset = offset;
            width
        });
        assert!((width - 299.0).abs() < 1e-9);
        assert_eq!(width.round() as i32, 299);
    }

    #[test]
    fn right_drag_reverses_immediately_after_hitting_the_minimum() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Compact;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;

        let shrunk =
            right_sidebar_width_after_drag_update(&settings, 1_250, None, 300.0, 0.0, 100.0);
        assert_eq!(shrunk, 250.0);

        let reversed =
            right_sidebar_width_after_drag_update(&settings, 1_250, None, shrunk, 100.0, 99.0);
        assert_eq!(reversed, 251.0);
    }

    #[test]
    fn right_drag_discards_overshoot_when_live_capacity_contracts() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Compact;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;

        assert_eq!(
            right_sidebar_width_after_drag_update(&settings, 968, None, 500.0, 0.0, 1.0),
            439.0
        );
    }

    #[test]
    fn left_drag_uses_the_window_capacity_owner_during_the_drag() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            preferred_left_sidebar_width: 400,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Full;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;
        let automatic = resolve_layout(&settings, 948);
        assert_eq!(automatic.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(automatic.right_sidebar, RightSidebarMode::Visible);

        let compact = resolve_left_sidebar_drag_preview(
            &settings,
            948,
            LeftSidebarMode::Compact,
            COMPACT_RAIL_WIDTH,
        );
        let fitted = resolve_left_sidebar_drag_preview(&settings, 948, LeftSidebarMode::Full, 246);
        let restored =
            resolve_left_sidebar_drag_preview(&settings, 948, LeftSidebarMode::Full, 400);

        assert_eq!(compact.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(compact.right_sidebar, RightSidebarMode::Visible);
        assert_eq!(compact.right_sidebar_width, 300);
        assert_eq!(fitted.left_sidebar_width, 246);
        assert_eq!(fitted.right_sidebar_width, MIN_RIGHT_SIDEBAR_WIDTH);
        assert_eq!(fitted.main_width, MIN_USEFUL_MAIN_WIDTH);
        assert_eq!(restored.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(restored.left_sidebar_width, COMPACT_RAIL_WIDTH);
        assert_eq!(restored.right_sidebar, RightSidebarMode::Visible);
    }

    #[test]
    fn right_drag_range_is_the_total_supported_sidebar_width_range() {
        let window_width = 1_250;
        let split_width = window_width - COMPACT_RAIL_WIDTH - LEFT_PANE_SEPARATOR_WIDTH;
        let leftmost = right_sidebar_allocation_position(split_width, MAX_RIGHT_SIDEBAR_WIDTH)
            .expect("right pane fits");
        let rightmost = right_sidebar_allocation_position(split_width, MIN_RIGHT_SIDEBAR_WIDTH)
            .expect("right pane fits");

        assert_eq!(
            split_width - leftmost - RIGHT_PANE_SEPARATOR_WIDTH,
            MAX_RIGHT_SIDEBAR_WIDTH
        );
        assert_eq!(
            split_width - rightmost - RIGHT_PANE_SEPARATOR_WIDTH,
            MIN_RIGHT_SIDEBAR_WIDTH
        );
        assert_eq!(rightmost - leftmost, 250);
    }

    #[test]
    fn right_layout_and_divider_drag_share_the_main_floor() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Hidden;

        settings.preferred_right_sidebar_width = 334;
        let preferred = resolve_layout(&settings, 1_144);
        assert_eq!(preferred.main_width, 809);
        assert_eq!(preferred.right_sidebar_width, 334);
        assert_eq!(right_sidebar_allocation_position(1_144, 334), Some(809));

        settings.preferred_right_sidebar_width = 500;
        let constrained = resolve_layout(&settings, 900);
        assert_eq!(constrained.main_width, MIN_USEFUL_MAIN_WIDTH);
        assert_eq!(constrained.right_sidebar_width, 449);
        assert_eq!(
            right_sidebar_allocation_position(900, 500),
            Some(MIN_USEFUL_MAIN_WIDTH)
        );

        let hidden = resolve_layout(&settings, 700);
        assert_eq!(hidden.right_sidebar, RightSidebarMode::Hidden);
        assert_eq!(right_sidebar_allocation_position(700, 500), None);
    }

    #[test]
    fn held_right_drag_refits_its_desired_width_on_every_allocation() {
        let desired_width = MAX_RIGHT_SIDEBAR_WIDTH;

        assert_eq!(
            right_sidebar_allocation_position(1_144, desired_width),
            Some(643)
        );
        assert_eq!(
            right_sidebar_allocation_position(900, desired_width),
            Some(MIN_USEFUL_MAIN_WIDTH)
        );
        assert_eq!(
            right_sidebar_allocation_position(760, desired_width),
            Some(MIN_USEFUL_MAIN_WIDTH)
        );
        assert_eq!(right_sidebar_allocation_position(700, desired_width), None);
    }

    #[test]
    fn allocation_owner_resolves_both_live_drag_previews_from_the_same_width() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            preferred_left_sidebar_width: 400,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Full;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;

        let resolved = resolve_layout_with_drag_previews(
            &settings,
            1_024,
            Some(LeftSidebarDragPreview {
                mode: LeftSidebarMode::Compact,
                width: COMPACT_RAIL_WIDTH,
            }),
            Some(MAX_RIGHT_SIDEBAR_WIDTH),
        );

        assert_eq!(resolved.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(resolved.left_sidebar_width, COMPACT_RAIL_WIDTH);
        assert_eq!(resolved.right_sidebar_width, 496);
        assert_eq!(resolved.main_width, MIN_USEFUL_MAIN_WIDTH);
    }

    #[test]
    fn no_op_left_drag_does_not_commit_an_automatic_fallback_width() {
        assert!(!left_sidebar_drag_changed(
            LeftSidebarMode::Full,
            250,
            LeftSidebarMode::Full,
            250,
        ));
        assert!(left_sidebar_drag_changed(
            LeftSidebarMode::Full,
            250,
            LeftSidebarMode::Full,
            251,
        ));
        assert!(left_sidebar_drag_changed(
            LeftSidebarMode::Full,
            250,
            LeftSidebarMode::Compact,
            COMPACT_RAIL_WIDTH,
        ));
    }

    #[test]
    fn layout_shrinks_full_left_then_uses_compact_and_hidden_fallbacks() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            preferred_left_sidebar_width: 400,
            ..Default::default()
        };
        settings.default_profile.right_sidebar = RightSidebarMode::Hidden;

        let minimum_full_window_width =
            MIN_APP_WINDOW_WIDTH + LEFT_PANE_SEPARATOR_WIDTH + MIN_LEFT_SIDEBAR_WIDTH;
        let shrunk = resolve_layout(&settings, minimum_full_window_width);
        let compact = resolve_layout(&settings, minimum_full_window_width - 1);
        let hidden = resolve_layout(&settings, 505);

        assert_eq!(shrunk.left_sidebar, ResolvedLeftSidebarMode::Full);
        assert_eq!(shrunk.left_sidebar_width, MIN_LEFT_SIDEBAR_WIDTH);
        assert_eq!(shrunk.main_width, MIN_APP_WINDOW_WIDTH);
        assert_eq!(compact.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(compact.left_sidebar_width, COMPACT_RAIL_WIDTH);
        assert_eq!(hidden.left_sidebar, ResolvedLeftSidebarMode::Hidden);
        assert_eq!(hidden.left_sidebar_width, 0);
    }

    #[test]
    fn layout_hides_configured_left_sidebar() {
        let mut settings = LayoutSettings {
            narrow_enabled: false,
            ..Default::default()
        };
        settings.default_profile.left_sidebar = LeftSidebarMode::Hidden;

        let resolved = resolve_layout(&settings, 1_500);

        assert_eq!(resolved.left_sidebar, ResolvedLeftSidebarMode::Hidden);
        assert_eq!(resolved.right_sidebar, RightSidebarMode::Visible);
        assert_eq!(
            resolved.main_width,
            1_500 - crate::DEFAULT_RIGHT_SIDEBAR_WIDTH - RIGHT_PANE_SEPARATOR_WIDTH
        );
    }

    #[test]
    fn layout_profiles_keep_independent_left_presentation() {
        let mut settings = LayoutSettings::default();
        settings.default_profile.left_sidebar = LeftSidebarMode::Hidden;
        settings.narrow_profile.left_sidebar = LeftSidebarMode::Compact;

        let narrow_width = settings.narrow_threshold - 1;
        let resolved = resolve_layout(&settings, narrow_width);

        assert_eq!(resolved.profile, ActiveLayoutProfile::Narrow);
        assert_eq!(resolved.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(resolved.right_sidebar, RightSidebarMode::Visible);
        assert_eq!(
            resolved.main_width,
            narrow_width
                - COMPACT_RAIL_WIDTH
                - LEFT_PANE_SEPARATOR_WIDTH
                - crate::DEFAULT_RIGHT_SIDEBAR_WIDTH
                - RIGHT_PANE_SEPARATOR_WIDTH
        );
    }

    #[test]
    fn layout_keeps_main_floor_at_window_minimum() {
        let settings = LayoutSettings::default();
        let resolved = resolve_layout(&settings, 1);

        assert_eq!(resolved.left_sidebar, ResolvedLeftSidebarMode::Hidden);
        assert_eq!(resolved.right_sidebar, RightSidebarMode::Hidden);
        assert_eq!(resolved.main_width, MIN_APP_WINDOW_WIDTH);
    }

    #[test]
    fn layout_uses_global_widths_with_profile_presentation() {
        let mut settings = LayoutSettings::default();
        settings.default_profile.left_sidebar = LeftSidebarMode::Compact;
        settings.default_profile.right_sidebar = RightSidebarMode::Visible;
        settings.preferred_left_sidebar_width = 350;
        settings.preferred_right_sidebar_width = 600;

        let window_width = 1_250;
        let resolved = resolve_layout(&settings, window_width);

        assert_eq!(resolved.left_sidebar, ResolvedLeftSidebarMode::Compact);
        assert_eq!(resolved.left_sidebar_width, COMPACT_RAIL_WIDTH);
        assert_eq!(resolved.right_sidebar, RightSidebarMode::Visible);
        assert_eq!(resolved.right_sidebar_width, MAX_RIGHT_SIDEBAR_WIDTH);
        assert_eq!(
            resolved.main_width,
            window_width
                - COMPACT_RAIL_WIDTH
                - LEFT_PANE_SEPARATOR_WIDTH
                - MAX_RIGHT_SIDEBAR_WIDTH
                - RIGHT_PANE_SEPARATOR_WIDTH
        );
    }
}
