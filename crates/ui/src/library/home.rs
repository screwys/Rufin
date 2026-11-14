use crate::widgets::media_drag::{MediaDragSource, install_compact_media_drag_source};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use library::{HomeAlbumRow, HomeProviderSection};
use localization::{msgid, track_count_text};

use crate::library::CatalogUi;
use crate::widgets::interactions::ContextMenuOpen;
use crate::widgets::layout::{AllocationOwner, allocation_owner, configure_fill_width_clip};
use crate::widgets::mounted_route::MountedRoute;
use rufin_core::settings::HomeBlockKind;
use rufin_core::settings::presentation::format_duration_units;
use rufin_core::view::{HomeItem, HomeSection, View};
use std::sync::Arc;

use super::collections::library_route_inset;
use super::detail_showcase::{
    DetailShowcaseView, ShowcaseCover, ShowcaseMount, ShowcaseShape, home_album_cover_projection,
};
use super::grid_cells::{
    AlbumGridCell, ArtistGridCell, MediaPlayingBinding, ReusableCollectionGridCell,
};
use crate::library::home_layout::home_section_header;
use crate::menus::media_menus::present_row_menu;

use crate::library::route_layout::{ROUTE_TOP_MARGIN, route_scroller_widget};
use crate::widgets::controls::install_row_transport_controls;
use crate::widgets::detail_links::DetailLinkBinding;
use crate::widgets::interactions::{CONTEXT_MENU_HOVER_OWNER_CLASS, install_context_menu_openers};
use crate::widgets::library_fields::COLLECTION_GRID_CARD_MARGIN;
use crate::widgets::selection::PlayTarget;
use rufin_core::artwork::ArtworkBinding;
use rufin_core::playback::QueuePlacement;
use rufin_core::route::Route;
use rufin_core::route::detail_links::album_row_artist_links;

type HomeRowBinding = Rc<dyn Fn(library::Row)>;

#[derive(Clone)]
struct MountedHomeSection {
    shell: std::rc::Weak<CatalogUi>,
    data: Rc<RefCell<Vec<library::Row>>>,
    /// The scrolling row: the columns built, then `rest`.
    row: gtk::Box,
    /// As wide as the columns not built yet, so the row scrolls its whole length from the start.
    rest: gtk::Box,
    /// The scroller's position, which says how far the row shows.
    adjustment: glib::WeakRef<gtk::Adjustment>,
    /// Each built entry's binding, in the order of `data`.
    cells: Rc<RefCell<Vec<HomeRowBinding>>>,
    /// The rows each column shows; 1 for cards.
    column_rows: usize,
}

impl MountedHomeSection {
    fn replace(&self, items: Vec<library::Row>) {
        let same_order = {
            let previous = self.data.borrow();
            previous.len() == items.len()
                && previous.iter().zip(&items).all(|(a, b)| match (a, b) {
                    (library::Row::Track(a), library::Row::Track(b)) => a.media_uri == b.media_uri,
                    (library::Row::Album(a), library::Row::Album(b)) => a.media_uri == b.media_uri,
                    (library::Row::Artist(a), library::Row::Artist(b)) => {
                        a.media_uri == b.media_uri
                    }
                    (library::Row::Tag(a), library::Row::Tag(b)) => a.key == b.key,
                    _ => false,
                })
        };
        if same_order {
            let mut incoming = items.into_iter();
            self.update(|item| {
                let next = incoming.next().expect("matching Home section lengths");
                if *item == next {
                    return false;
                }
                *item = next;
                true
            });
            return;
        }
        self.data.replace(items);
        self.render();
    }

    /// Starts the row over.
    fn render(&self) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        self.cells.borrow_mut().clear();
        self.row.append(&self.rest);
        self.fill();
    }

    /// Builds the columns, of cards or of rows, as far as the scroller shows the row. The ones
    /// past it are built when the row scrolls or widens to them.
    fn fill(&self) {
        let (Some(shell), Some(adjustment)) = (self.shell.upgrade(), self.adjustment.upgrade())
        else {
            return;
        };
        let (width, gap) = column_size(self.column_rows);
        let data = self.data.borrow();
        let columns = data.len().div_ceil(self.column_rows);
        // Before its first allocation the scroller shows no more than the route's width.
        let end = match adjustment.page_size() {
            page if page > 0.0 => scrolled(&adjustment, &self.row) + page,
            _ => f64::from(shell.route_width()),
        };
        let shown = (end / f64::from(width + gap)).ceil() as usize;
        let mut cells = self.cells.borrow_mut();
        let built = cells.len().div_ceil(self.column_rows);
        // One column past the ones shown, so keyboard focus moving on scrolls the row to it.
        let wanted = (shown + 1).min(columns).max(built);
        for items in data.chunks(self.column_rows).take(wanted).skip(built) {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.set_width_request(width);
            for item in items.iter().cloned() {
                let (cell, bind) = match item {
                    library::Row::Album(album) if self.column_rows == 1 => {
                        home_album_card(&shell, album)
                    }
                    library::Row::Artist(artist) if self.column_rows == 1 => {
                        home_artist_card(&shell, artist)
                    }
                    item => home_row_widget(&shell, item),
                };
                cells.push(bind);
                column.append(&cell);
            }
            // A column stays its width: cards expand to fill grid cells and a long title asks for
            // more.
            let clamp = adw::Clamp::builder()
                .maximum_size(width)
                .tightening_threshold(width)
                .unit(adw::LengthUnit::Px)
                .hexpand(false)
                .child(&column)
                .build();
            self.row
                .insert_child_after(&clamp, self.rest.prev_sibling().as_ref());
        }
        let rest = (columns - wanted) as i32;
        self.rest.set_visible(rest > 0);
        self.rest
            .set_width_request((rest * (width + gap) - gap).max(0));
    }

    fn update(&self, mut update: impl FnMut(&mut library::Row) -> bool) {
        let changed = {
            let mut data = self.data.borrow_mut();
            data.iter_mut()
                .enumerate()
                .filter_map(|(position, item)| update(item).then(|| (position, item.clone())))
                .collect::<Vec<_>>()
        };
        for (position, item) in changed {
            self.bind(position, item);
        }
    }

    /// Shows `item` at `position`.
    fn set(&self, position: usize, item: library::Row) {
        match self.data.borrow_mut().get_mut(position) {
            Some(shown) => *shown = item.clone(),
            None => return,
        }
        self.bind(position, item);
    }

    /// Binds `item` to the entry that shows `position`.
    fn bind(&self, position: usize, item: library::Row) {
        let bind = self.cells.borrow().get(position).cloned();
        if let Some(bind) = bind {
            bind(item);
        }
    }
}

const CARD_WIDTH: i32 = 160;
const CARD_GAP: i32 = 10;
const ROW_COLUMN_WIDTH: i32 = 300;
const ROW_COLUMN_GAP: i32 = 20;
const ROW_COVER_SIZE: i32 = 52;

/// The rows each column of `section` shows: two rows, or one card.
fn column_rows(section: &HomeSection) -> usize {
    if section.shows_rows() { 2 } else { 1 }
}

/// The width of a column of `rows` rows and the gap to the next one.
fn column_size(rows: usize) -> (i32, i32) {
    if rows == 1 {
        (CARD_WIDTH, CARD_GAP)
    } else {
        (ROW_COLUMN_WIDTH, ROW_COLUMN_GAP)
    }
}

/// How far `adjustment` has scrolled `row` from its start, which is its right end in a
/// right-to-left layout.
fn scrolled(adjustment: &gtk::Adjustment, row: &impl IsA<gtk::Widget>) -> f64 {
    if row.direction() == gtk::TextDirection::Rtl {
        adjustment.upper() - adjustment.page_size() - adjustment.value()
    } else {
        adjustment.value() - adjustment.lower()
    }
}

/// Scrolls `scroller` forward or back by the whole columns, `stride` apart, that fit its width, as
/// Home's ‹ › do.
fn scroll_page(scroller: &gtk::ScrolledWindow, stride: f64, forward: bool) {
    let adjustment = scroller.hadjustment();
    let columns = (adjustment.page_size() / stride).floor().max(1.0);
    let first = (scrolled(&adjustment, scroller) / stride).round();
    let length = adjustment.upper() - adjustment.lower() - adjustment.page_size();
    let from_start =
        ((first + if forward { columns } else { -columns }) * stride).clamp(0.0, length.max(0.0));
    let to = if scroller.direction() == gtk::TextDirection::Rtl {
        adjustment.upper() - adjustment.page_size() - from_start
    } else {
        adjustment.lower() + from_start
    };
    let target = adw::PropertyAnimationTarget::new(&adjustment, "value");
    let scroll = adw::TimedAnimation::new(scroller, adjustment.value(), to, 250, target);
    scroll.set_easing(adw::Easing::EaseOutCubic);
    scroll.play();
}

#[derive(Clone)]
struct HomeSectionView {
    root: gtk::Widget,
    header: crate::library::home_layout::HomeSectionHeaderView,
    mounted: MountedHomeSection,
}

/// Which section a shown Home section is, so a new read updates it in place.
#[derive(Clone, PartialEq)]
enum HomeSectionId {
    Showcase,
    Genres,
    Rows(HomeBlockKind),
    Provider(String),
}

fn home_section_id(section: &HomeSection) -> HomeSectionId {
    match section {
        HomeSection::Showcase(_) => HomeSectionId::Showcase,
        HomeSection::Genres(_) => HomeSectionId::Genres,
        HomeSection::Rows { block, .. } => HomeSectionId::Rows(*block),
        HomeSection::Provider(provider) => HomeSectionId::Provider(provider.section_id.clone()),
    }
}

/// A shown Home section: a page of items, or the showcase.
#[derive(Clone)]
enum ShownHomeSection {
    Rows(HomeSectionView),
    Showcase(HomeShowcaseView),
}

impl ShownHomeSection {
    fn root(&self) -> &gtk::Widget {
        match self {
            Self::Rows(view) => &view.root,
            Self::Showcase(view) => &view.0.root,
        }
    }
}

/// Home's showcase: one header at a time, sliding to the album before or after it.
#[derive(Clone)]
struct HomeShowcaseView(Rc<HomeShowcaseState>);

struct HomeShowcaseState {
    shell: std::rc::Weak<CatalogUi>,
    root: gtk::Widget,
    stack: gtk::Stack,
    shown: RefCell<library::HomeShowcase>,
    /// The visible header.
    page: RefCell<DetailShowcaseView>,
    /// A step sent to core whose read has not come back, and whether it went forward.
    pending: Cell<Option<bool>>,
}

impl HomeShowcaseView {
    fn new(shell: &Rc<CatalogUi>, showcase: library::HomeShowcase) -> Self {
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vhomogeneous(false);
        stack.set_interpolate_size(true);
        stack.set_transition_duration(260);
        // A finished slide leaves only the header it slid to.
        stack.connect_transition_running_notify(remove_hidden_pages);
        Self(Rc::new_cyclic(
            |state: &std::rc::Weak<HomeShowcaseState>| {
                let (widget, page) = home_showcase_page(shell, &showcase.current, step_with(state));
                stack.add_child(&widget);
                let root = allocation_owner(&stack, |_, _| {});
                let step = step_with(state);
                install_touch_slide(&root, move |forward| step(forward));
                HomeShowcaseState {
                    shell: Rc::downgrade(shell),
                    root: root.upcast(),
                    stack,
                    shown: RefCell::new(showcase),
                    page: RefCell::new(page),
                    pending: Cell::new(None),
                }
            },
        ))
    }

    /// The album shown.
    fn current(&self) -> HomeAlbumRow {
        self.0.shown.borrow().current.clone()
    }

    /// Shows a committed change of the shown album: its favorite and downloads.
    fn show_current(&self, row: HomeAlbumRow) {
        self.0
            .page
            .borrow()
            .show_row(library::Row::Album(row.album.clone()));
        self.0.shown.borrow_mut().current = row;
    }

    /// Shows a new read: the album shown keeps its header, another one slides in.
    fn show(&self, next: library::HomeShowcase) {
        let state = &self.0;
        let pending = state.pending.take();
        let uri = |row: &HomeAlbumRow| row.album.media_uri.clone();
        let (same, beside) = {
            let shown = state.shown.borrow();
            let beside = if uri(&shown.next) == uri(&next.current) {
                Some(true)
            } else if uri(&shown.previous) == uri(&next.current) {
                Some(false)
            } else {
                None
            };
            (uri(&shown.current) == uri(&next.current), beside)
        };
        if same {
            self.show_current(next.current.clone());
            state.shown.replace(next);
            return;
        }
        let current = next.current.clone();
        state.shown.replace(next);
        self.slide(&current, pending.or(beside));
    }

    /// Steps the showcase: the album beside it slides in at once, and the showcase core
    /// answers follows.
    fn step(&self, forward: bool) {
        let state = &self.0;
        let Some(source_id) = state.shell.upgrade().and_then(|shell| {
            shell
                .selected_library()
                .map(|selected| selected.source_id.clone())
        }) else {
            return;
        };
        if state.pending.get().is_none() {
            let beside = {
                let shown = state.shown.borrow();
                let beside = if forward {
                    &shown.next
                } else {
                    &shown.previous
                };
                (beside.album.media_uri != shown.current.album.media_uri).then(|| beside.clone())
            };
            if let Some(album) = beside {
                state.shown.borrow_mut().current = album.clone();
                self.slide(&album, Some(forward));
            }
        }
        state.pending.set(Some(forward));
        let Some(shell) = state.shell.upgrade() else {
            return;
        };
        let answer = shell.products.source.step_showcase(source_id, forward);
        let view = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            if let Ok(Ok(Some(showcase))) = answer.recv().await
                && let Some(state) = view.upgrade()
            {
                HomeShowcaseView(state).show(showcase);
            }
        });
    }

    /// Slides `album`'s header in: from the right going forward, from the left going back.
    fn slide(&self, album: &HomeAlbumRow, forward: Option<bool>) {
        let state = &self.0;
        let Some(shell) = state.shell.upgrade() else {
            return;
        };
        let (widget, page) = home_showcase_page(&shell, album, step_with(&Rc::downgrade(&self.0)));
        state.stack.set_transition_type(match forward {
            Some(true) => gtk::StackTransitionType::SlideLeft,
            Some(false) => gtk::StackTransitionType::SlideRight,
            None => gtk::StackTransitionType::Crossfade,
        });
        state.stack.add_child(&widget);
        state.stack.set_visible_child(&widget);
        state.page.replace(page);
        if !state.stack.is_transition_running() {
            remove_hidden_pages(&state.stack);
        }
    }
}

/// The step a header's ‹ › call.
/// Moves the showcase in `owner` with a horizontal touch drag and settles when the finger lifts:
/// past a fifth of its width it calls `step(true)` for a drag to the left, as › does, or
/// `step(false)`, whose header slides in; otherwise the header slides back. It takes the touch once
/// it moves sideways, before a drag of the cover starts; the mouse acts as before.
fn install_touch_slide(owner: &AllocationOwner, step: impl Fn(bool) + 'static) {
    owner.set_overflow(gtk::Overflow::Hidden);
    let animation = Rc::new(RefCell::new(None::<adw::TimedAnimation>));
    let sliding = Rc::new(Cell::new(false));
    let slide = gtk::GestureDrag::new();
    slide.set_touch_only(true);
    slide.set_propagation_phase(gtk::PropagationPhase::Capture);
    let moved = owner.downgrade();
    let held = Rc::clone(&animation);
    let started = Rc::clone(&sliding);
    slide.connect_drag_update(move |gesture, x, y| {
        if !started.get() {
            if x.abs() <= 6.0 || x.abs() <= y.abs() {
                return;
            }
            started.set(true);
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(animation) = held.take() {
                animation.skip();
            }
        }
        if let Some(owner) = moved.upgrade() {
            owner.set_offset_x(x as f32);
        }
    });
    let settled = owner.downgrade();
    slide.connect_drag_end(move |_, x, _| {
        let (true, Some(owner)) = (sliding.replace(false), settled.upgrade()) else {
            return;
        };
        if x.abs() > f64::from(owner.width()) / 5.0 {
            owner.set_offset_x(0.0);
            step(x < 0.0);
            return;
        }
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak]
            owner,
            move |offset| owner.set_offset_x(offset as f32)
        ));
        let slide = adw::TimedAnimation::new(&owner, x, 0.0, 220, target);
        slide.set_easing(adw::Easing::EaseOutCubic);
        slide.play();
        animation.replace(Some(slide));
    });
    owner.add_controller(slide);
}

fn step_with(state: &std::rc::Weak<HomeShowcaseState>) -> Rc<dyn Fn(bool)> {
    let state = state.clone();
    Rc::new(move |forward| {
        if let Some(state) = state.upgrade() {
            HomeShowcaseView(state).step(forward);
        }
    })
}

fn remove_hidden_pages(stack: &gtk::Stack) {
    if stack.is_transition_running() {
        return;
    }
    let visible = stack.visible_child();
    let mut child = stack.first_child();
    while let Some(page) = child {
        child = page.next_sibling();
        if Some(&page) != visible.as_ref() {
            stack.remove(&page);
        }
    }
}

#[derive(Clone)]
struct HomeRouteProjection {
    shell: std::rc::Weak<CatalogUi>,
    /// The showcase, for its album's state and download counts.
    showcase: Rc<RefCell<Option<HomeShowcaseView>>>,
    shown: Rc<RefCell<Vec<(HomeSectionId, ShownHomeSection)>>>,
    /// The page whose sections Home shows, which plays them.
    view: Rc<RefCell<Arc<View>>>,
    content: gtk::Box,
    empty: gtk::Widget,
    /// The committed changes Home shows.
    changes: rufin_core::shown::ShownChanges,
}

impl HomeRouteProjection {
    /// Shows a committed change in the showcase album and the rows Home shows,
    /// through core's [`rufin_core::shown::ShownChanges`]: only the rows it changes.
    fn show_changes(
        &self,
        owner: &rufin_core::source::SourceOwner,
        changed: &rufin_core::source::feed::Changed,
        view: Option<&rufin_core::view::View>,
    ) {
        let sections = self.sections();
        let showcase = self
            .showcase
            .borrow()
            .as_ref()
            .map(|view| home_row(HomeItem::Album(Box::new(view.current()))));
        let shown = {
            let data = sections
                .iter()
                .map(|view| view.mounted.data.borrow())
                .collect::<Vec<_>>();
            let rows = showcase
                .iter()
                .chain(data.iter().flat_map(|items| items.iter()))
                .collect::<Vec<_>>();
            self.changes.show(owner, &rows, changed, view)
        };
        // Where the row at each index shows: the showcase, then each section's rows.
        let at = showcase
            .iter()
            .map(|_| None)
            .chain(sections.iter().enumerate().flat_map(|(section, view)| {
                (0..view.mounted.data.borrow().len()).map(move |position| Some((section, position)))
            }))
            .collect::<Vec<_>>();
        for (index, row) in shown.states {
            self.show_row(&sections, at[index], row);
        }
        let Some(downloads) = shown.downloads else {
            return;
        };
        let home = self.clone();
        glib::spawn_future_local(async move {
            use library::ShowsState;
            let Some(read) = downloads.await else {
                return;
            };
            // The downloads go into the row Home shows there now when it is the same row.
            for (index, read) in read {
                let current = match at[index] {
                    None => home
                        .showcase
                        .borrow()
                        .as_ref()
                        .map(|view| home_row(HomeItem::Album(Box::new(view.current())))),
                    Some((section, position)) => sections[section]
                        .mounted
                        .data
                        .borrow()
                        .get(position)
                        .cloned(),
                };
                if let Some(mut current) = current
                    && current.show_downloads(read)
                {
                    home.show_row(&sections, at[index], current);
                }
            }
        });
    }

    /// Shows `row` where `at` says: the showcase, or a section's position.
    fn show_row(
        &self,
        sections: &[HomeSectionView],
        at: Option<(usize, usize)>,
        row: library::Row,
    ) {
        match (at, row) {
            (None, library::Row::Album(album)) => {
                if let Some(showcase) = self.showcase.borrow().as_ref() {
                    showcase.show_current(HomeAlbumRow {
                        title: album.title.clone(),
                        album,
                    });
                }
            }
            (Some((section, position)), row) => sections[section].mounted.set(position, row),
            (None, _) => {}
        }
    }

    /// Shows the sections of Home's page `view` in the order core composed them.
    fn apply(&self, view: Arc<View>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let sections = view.home_sections().unwrap_or_default();
        self.view.replace(view);
        let mut previous = self.shown.take();
        let mut shown = Vec::with_capacity(sections.len());
        let mut after = None::<gtk::Widget>;
        self.showcase.replace(None);
        for section in sections {
            let id = home_section_id(&section);
            let kept = previous
                .iter()
                .position(|(shown, _)| *shown == id)
                .map(|index| previous.remove(index).1);
            let next = match (kept, section) {
                (Some(ShownHomeSection::Showcase(view)), HomeSection::Showcase(showcase)) => {
                    view.show(*showcase);
                    ShownHomeSection::Showcase(view)
                }
                (Some(ShownHomeSection::Rows(view)), section)
                    if !matches!(section, HomeSection::Showcase(_))
                        && view.mounted.column_rows == column_rows(&section) =>
                {
                    if let HomeSection::Provider(provider) = &section {
                        view.header.set_title(&home_provider_title(provider));
                    }
                    view.mounted.replace(home_rows(&section));
                    ShownHomeSection::Rows(view)
                }
                (kept, section) => {
                    if let Some(kept) = kept {
                        self.content.remove(kept.root());
                    }
                    let next = self.build(&shell, id.clone(), section);
                    self.content.append(next.root());
                    next
                }
            };
            if let ShownHomeSection::Showcase(view) = &next {
                self.showcase.replace(Some(view.clone()));
            }
            self.content
                .reorder_child_after(next.root(), after.as_ref());
            after = Some(next.root().clone());
            shown.push((id, next));
        }
        for (_, stale) in previous {
            self.content.remove(stale.root());
        }
        self.empty.set_visible(shown.is_empty());
        self.content
            .reorder_child_after(&self.empty, after.as_ref());
        self.shown.replace(shown);
    }

    fn build(
        &self,
        shell: &Rc<CatalogUi>,
        id: HomeSectionId,
        section: HomeSection,
    ) -> ShownHomeSection {
        if let HomeSection::Showcase(showcase) = section {
            return ShownHomeSection::Showcase(HomeShowcaseView::new(shell, *showcase));
        }
        let view =
            shell.home_section_view(&section.title(), home_rows(&section), column_rows(&section));
        for (button, shuffled) in [(view.header.play(), false), (view.header.shuffle(), true)] {
            let shell = Rc::downgrade(shell);
            let shown = Rc::downgrade(&self.shown);
            let page = Rc::downgrade(&self.view);
            let id = id.clone();
            button.connect_clicked(move |_| {
                let (Some(shell), Some(shown), Some(page)) =
                    (shell.upgrade(), shown.upgrade(), page.upgrade())
                else {
                    return;
                };
                // The section's place among the page's sections, as core plays it.
                let index = shown.borrow().iter().position(|(shown, _)| *shown == id);
                if let Some(request) = index.and_then(|index| {
                    page.borrow()
                        .play_section(index, QueuePlacement::Now, shuffled)
                }) {
                    shell.products.playback.play_selection(request);
                }
            });
        }
        ShownHomeSection::Rows(view)
    }

    fn sections(&self) -> Vec<HomeSectionView> {
        self.shown
            .borrow()
            .iter()
            .filter_map(|(_, shown)| match shown {
                ShownHomeSection::Rows(view) => Some(view.clone()),
                ShownHomeSection::Showcase(_) => None,
            })
            .collect()
    }
}

/// The rows `section` shows, each with the title Home gives it.
fn home_rows(section: &HomeSection) -> Vec<library::Row> {
    if let HomeSection::Genres(genres) = section {
        return genres.iter().cloned().map(library::Row::Tag).collect();
    }
    section.items().into_iter().map(home_row).collect()
}

fn home_row(item: HomeItem) -> library::Row {
    match item {
        HomeItem::Track(item) => {
            let library::HomeTrackRow { mut track, title } = *item;
            track.title = title;
            library::Row::Track(track)
        }
        HomeItem::Album(item) => {
            let HomeAlbumRow { mut album, title } = *item;
            album.title = title;
            library::Row::Album(album)
        }
        HomeItem::Artist(artist) => library::Row::Artist(*artist),
    }
}

fn home_provider_title(provider: &HomeProviderSection) -> String {
    rufin_core::settings::presentation::home_provider_title(
        &provider.section_id,
        provider.title.as_deref(),
    )
}

impl CatalogUi {
    pub fn home_route(self: &Rc<Self>, view: Arc<View>) -> MountedRoute {
        let scroller = gtk::ScrolledWindow::new();
        configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("route-content");
        content.set_hexpand(true);
        content.set_halign(gtk::Align::Fill);
        content.set_width_request(1);
        content.set_margin_top(ROUTE_TOP_MARGIN);
        let empty = crate::library::route_layout::route_empty_view(msgid("Nothing here yet"));
        content.append(&empty);
        let inset_content = library_route_inset(content.clone().upcast());
        scroller.set_child(Some(&inset_content));

        let projection = HomeRouteProjection {
            shell: Rc::downgrade(self),
            showcase: Rc::new(RefCell::new(None)),
            shown: Rc::new(RefCell::new(Vec::new())),
            view: Rc::new(RefCell::new(Arc::clone(&view))),
            content,
            empty,
            changes: Default::default(),
        };
        projection.apply(view);
        let widget = route_scroller_widget(scroller);
        let apply = projection.clone();
        let states = projection.clone();
        let owner = self.products.source.clone();
        MountedRoute::new(widget, Rc::new(|| {}))
            .with_home_page_apply(Rc::new(move |view| apply.apply(view)))
            .with_states(Rc::new(move |changed, view| {
                states.show_changes(&owner, changed, view)
            }))
    }

    fn home_section_view(
        self: &Rc<Self>,
        title: &str,
        items: Vec<library::Row>,
        rows: usize,
    ) -> HomeSectionView {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        section.set_hexpand(true);
        let header = home_section_header(title);
        header.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        section.append(&header);

        let (width, gap) = column_size(rows);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, gap);
        row.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        row.set_margin_end(COLLECTION_GRID_CARD_MARGIN);
        let scroller = gtk::ScrolledWindow::new();
        // Scrolls by wheel, touchpad, touch and ‹ ›, with no scrollbar over the cards' last line.
        scroller.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        scroller.set_propagate_natural_height(true);
        scroller.set_hexpand(true);
        scroller.set_child(Some(&row));
        section.append(&scroller);
        let adjustment = scroller.hadjustment();
        let mounted = MountedHomeSection {
            shell: Rc::downgrade(self),
            data: Rc::new(RefCell::new(items)),
            row,
            rest: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            adjustment: adjustment.downgrade(),
            cells: Rc::new(RefCell::new(Vec::new())),
            column_rows: rows,
        };
        mounted.render();
        // The row builds as far as it shows; ‹ › scroll it and rest at its ends.
        let ends = {
            let previous = header.previous();
            let next = header.next();
            let mounted = mounted.clone();
            move |adjustment: &gtk::Adjustment| {
                mounted.fill();
                let from_start = scrolled(adjustment, &mounted.row);
                let length = adjustment.upper() - adjustment.lower() - adjustment.page_size();
                previous.set_sensitive(from_start > 0.0);
                next.set_sensitive(from_start < length - 1.0);
            }
        };
        ends(&adjustment);
        adjustment.connect_changed(ends.clone());
        adjustment.connect_value_changed(ends);
        for (button, forward) in [(header.previous(), false), (header.next(), true)] {
            let scroller = scroller.clone();
            button.connect_clicked(move |_| {
                scroll_page(&scroller, f64::from(width + gap), forward);
            });
        }
        HomeSectionView {
            root: section.upcast(),
            header,
            mounted,
        }
    }
}

/// A header of Home's showcase for `item`; its ‹ › call `step`.
fn home_showcase_page(
    shell: &Rc<CatalogUi>,
    item: &HomeAlbumRow,
    step: Rc<dyn Fn(bool)>,
) -> (gtk::Widget, DetailShowcaseView) {
    let width = crate::library::route_layout::home_album_content_width_for(shell.route_width());
    let mut album = item.album.clone();
    album.title.clone_from(&item.title);
    let view = DetailShowcaseView::new(&album.title);
    let menu: ContextMenuOpen = {
        let shell = Rc::downgrade(shell);
        let view = view.downgrade();
        Rc::new(move |target, position| {
            let (Some(shell), Some(row)) =
                (shell.upgrade(), view.upgrade().and_then(|view| view.row()))
            else {
                return;
            };
            present_row_menu(target, shell.shell(), row, None, position);
        })
    };
    let cover = home_album_cover_projection(shell, &album, ShowcaseShape::Home.cover_size(width));
    let drag_album = album.clone();
    let drag_target: gtk::Widget = cover.button().upcast();
    let drag_artwork = cover.drag_paintable_source();
    install_compact_media_drag_source(&drag_target, &drag_artwork, move || {
        let source = MediaDragSource::capture_target(
            crate::widgets::selection::PlayTarget::of_album(&drag_album),
        );
        Some((source, drag_album.title.clone()))
    });

    let artist = view.set_detail(&album.artist);
    DetailLinkBinding::new(&artist, shell.route_navigation()).bind(album_row_artist_links(&album));
    view.replace_summary(&[
        (
            "rufin-x-office-calendar-symbolic",
            rufin_core::settings::presentation::album_year(&album),
        ),
        (
            "rufin-tracks-symbolic",
            track_count_text(album.summary.track_count.max(0) as u64),
        ),
        (
            "rufin-preferences-system-time-symbolic",
            format_duration_units((album.summary.duration_millis.max(0) / 1_000) as u32),
        ),
    ]);
    let track_count = album.summary.track_count.max(0) as u64;
    view.bind_summary_text_with(1, move || track_count_text(track_count));
    view.enable_steps(step);

    let play_shell = Rc::clone(shell);
    let play_target = crate::widgets::selection::PlayTarget::of_album(&album);
    let play: CollectionPlay = Rc::new(move |placement, shuffled| {
        crate::menus::media_menus::play_target(
            &play_shell.products,
            &play_target,
            placement,
            shuffled,
        );
    });
    let widget = view.mount(
        shell,
        library::Row::Album(album),
        ShowcaseMount {
            shape: ShowcaseShape::Home,
            initial_width: width,
            cover: ShowcaseCover::Single(cover),
            play_label: msgid("Play album"),
            play,
            menu,
            download: false,
        },
    );
    (widget, view)
}

fn home_album_card(
    shell: &Rc<CatalogUi>,
    album: library::AlbumRow,
) -> (gtk::Widget, HomeRowBinding) {
    let cell = AlbumGridCell::new(shell, &rufin_core::settings::layout::HOME_ALBUM_FIELDS);
    cell.enable_card_activation();
    cell.set_cover_size(CARD_WIDTH);
    cell.bind(0, album);
    let widget = cell.widget();
    (
        widget,
        Rc::new(move |row| {
            if let library::Row::Album(album) = row {
                cell.bind(0, album);
            }
        }),
    )
}

fn home_artist_card(
    shell: &Rc<CatalogUi>,
    artist: library::ArtistRow,
) -> (gtk::Widget, HomeRowBinding) {
    let cell = ArtistGridCell::new(shell, &rufin_core::settings::layout::HOME_ARTIST_FIELDS);
    cell.enable_card_activation();
    cell.set_cover_size(CARD_WIDTH);
    cell.bind(0, artist);
    let widget = cell.widget();
    (
        widget,
        Rc::new(move |row| {
            if let library::Row::Artist(artist) = row {
                cell.bind(0, artist);
            }
        }),
    )
}

/// A row of a Home column: the cover, the title and, under it, Home's fields of the row or a
/// genre's album count. A click opens its page and a double click plays it, as on the sidebar's
/// Saved rows, whose play controls it shows on hover.
fn home_row_widget(shell: &Rc<CatalogUi>, row: library::Row) -> (gtk::Widget, HomeRowBinding) {
    let widget = gtk::Overlay::new();
    widget.add_css_class("home-row");
    widget.add_css_class(CONTEXT_MENU_HOVER_OWNER_CLASS);
    widget.set_overflow(gtk::Overflow::Hidden);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let cover = shell.artwork.cover_group_projection_for_artwork(
        &[],
        ROW_COVER_SIZE,
        ROW_COVER_SIZE,
        matches!(row, library::Row::Tag(_)),
    );
    content.append(&cover.widget());
    let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
    labels.set_valign(gtk::Align::Center);
    labels.set_hexpand(true);
    let [title, subtitle] = [
        ["track-title", "home-row-title"],
        ["muted", "home-row-subtitle"],
    ]
    .map(|classes| {
        let label = gtk::Label::new(None);
        for class in classes {
            label.add_css_class(class);
        }
        label.set_xalign(0.0);
        label.set_width_chars(1);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&label);
        label
    });
    content.append(&labels);
    widget.set_child(Some(&content));

    let shown = Rc::new(RefCell::new(None::<library::Row>));
    let play = {
        let shell = Rc::downgrade(shell);
        let shown = Rc::clone(&shown);
        move |placement| {
            let target = shown.borrow().as_ref().and_then(PlayTarget::of_row);
            if let (Some(shell), Some(target)) = (shell.upgrade(), target) {
                crate::menus::media_menus::play_target(&shell.products, &target, placement, false);
            }
        }
    };
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    let click_shell = Rc::downgrade(shell);
    let click_row = Rc::clone(&shown);
    let click_play = play.clone();
    click.connect_pressed(move |gesture, presses, _, _| {
        if presses == 2 {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            click_play(QueuePlacement::Now);
        }
    });
    click.connect_released(move |_, presses, _, _| {
        let route = click_row.borrow().as_ref().and_then(Route::of_row);
        if let (1, Some(route), Some(shell)) = (presses, route, click_shell.upgrade()) {
            shell.open_route(route);
        }
    });
    widget.add_controller(click);
    install_row_transport_controls(&widget, play);
    let menu_shell = Rc::downgrade(shell);
    let menu_row = Rc::clone(&shown);
    install_context_menu_openers(
        &widget,
        Rc::new(move |target, position| {
            if let (Some(shell), Some(row)) = (menu_shell.upgrade(), menu_row.borrow().clone()) {
                present_row_menu(target, shell.shell(), row, None, position);
            }
        }),
    );
    let drag_row = Rc::clone(&shown);
    let drag_title = title.clone();
    install_compact_media_drag_source(&widget, &cover.widget(), move || {
        let target = PlayTarget::of_row(drag_row.borrow().as_ref()?)?;
        Some((
            MediaDragSource::capture_target(target),
            drag_title.text().into(),
        ))
    });

    let playing = MediaPlayingBinding::new(shell, &title);
    let shell = Rc::downgrade(shell);
    let bind: HomeRowBinding = Rc::new(move |row| {
        use rufin_core::settings::layout::{
            HOME_ALBUM_FIELDS, HOME_GENRE_FIELDS, HOME_TRACK_FIELDS,
        };
        let Some(shell) = shell.upgrade() else {
            return;
        };
        let (name, fields, artwork): (String, &[library::LibraryField], Vec<&[u8]>) = match &row {
            library::Row::Track(track) => (
                track.title.clone(),
                &HOME_TRACK_FIELDS,
                track.artwork_binding.as_deref().into_iter().collect(),
            ),
            row => {
                let Some(shown) = row.collection() else {
                    return;
                };
                let fields: &[library::LibraryField] = match row {
                    library::Row::Tag(_) => &HOME_GENRE_FIELDS,
                    _ => &HOME_ALBUM_FIELDS,
                };
                (shown.name.to_owned(), fields, shown.artwork)
            }
        };
        let text = fields
            .iter()
            .map(|field| rufin_core::settings::presentation::row_field(&row, *field))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        title.set_text(&name);
        subtitle.set_text(&text);
        let artwork = artwork
            .into_iter()
            .map(ArtworkBinding::opaque)
            .collect::<Vec<_>>();
        cover.replace(&shell.artwork, &artwork);
        playing.bind(&shell, PlayTarget::of_row(&row));
        shown.replace(Some(row));
    });
    bind(row);
    (widget.upcast(), bind)
}

use crate::menus::media_menus::CollectionPlay;
