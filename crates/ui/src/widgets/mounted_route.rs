use crate::widgets::view_model::Rows;
use adw::prelude::*;
use gtk::glib;
use library::Row;
use playback::TransportStatus;
use std::rc::Rc;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCurrentTrackContext {
    pub context_id: String,
    pub source_rank: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCurrentTrack {
    pub track_key: Option<library::TrackKey>,
    pub media_uri: String,
    pub context: Option<RouteCurrentTrackContext>,
    pub paused: bool,
}
pub fn route_current_track(player: Option<&playback::PlaybackView>) -> Option<RouteCurrentTrack> {
    let player = player?;
    let entry = player.transport.current.as_ref()?;
    let context = match &entry.row.provenance {
        playback::QueueProvenance::Context {
            context_id,
            source_rank,
            ..
        } => Some(RouteCurrentTrackContext {
            context_id: context_id.to_string(),
            source_rank: *source_rank,
        }),
        playback::QueueProvenance::Manual
        | playback::QueueProvenance::Random
        | playback::QueueProvenance::Radio
        | playback::QueueProvenance::AutoDj
        | playback::QueueProvenance::Legacy => None,
    };
    Some(RouteCurrentTrack {
        track_key: Some(entry.row.track.track_key),
        media_uri: entry.row.track.media_uri.clone(),
        context,
        paused: !matches!(
            player.transport.effective_state(),
            TransportStatus::Resolving | TransportStatus::Buffering | TransportStatus::Playing
        ),
    })
}
pub type RouteCurrentTrackSelection = Rc<dyn Fn(Option<&RouteCurrentTrack>) -> bool>;
pub type MountedRouteResume = Rc<dyn Fn()>;
pub type MountedRouteCommand = Rc<dyn Fn()>;
pub type MountedRouteSearchProvider = Rc<dyn Fn() -> Option<MountedRouteSearchTarget>>;
pub type MountedHomePageApply = Rc<dyn Fn(Vec<rufin_core::view::HomeSection>)>;
/// Shows a committed change in what a route shows outside its rows and header.
pub type MountedStates =
    Rc<dyn Fn(&rufin_core::source::feed::Changed, Option<&rufin_core::view::View>)>;
/// Shows a route's header read again.
pub type MountedHeader = Rc<dyn Fn(Option<library::Header>)>;
/// The header a route shows now.
pub type MountedCurrentHeader = Rc<dyn Fn() -> Option<library::Header>>;
pub type MountedRouteItemNavigation = Rc<dyn Fn(gtk::DirectionType) -> glib::Propagation>;

#[derive(Clone)]
pub struct MountedRouteSearchTarget {
    pub search: gtk::SearchEntry,
    pub focus: Option<MountedRouteCommand>,
}

impl MountedRouteSearchTarget {
    pub fn focus(&self) {
        if let Some(focus) = &self.focus {
            focus();
        } else {
            self.search.grab_focus();
        }
    }
}

pub fn item_navigation_entry_position(
    current: u32,
    item_count: u32,
    direction: gtk::DirectionType,
) -> Option<u32> {
    if item_count == 0 {
        return None;
    }
    if current != gtk::INVALID_LIST_POSITION {
        return Some(current.min(item_count - 1));
    }
    match direction {
        gtk::DirectionType::Up | gtk::DirectionType::Left => Some(item_count - 1),
        gtk::DirectionType::Down | gtk::DirectionType::Right => Some(0),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemScrollPosition {
    pub position: u32,
    pub offset: f64,
    pub query_position: Option<usize>,
}

#[derive(Clone)]
pub struct NativeListScroll {
    pub list: gtk::ListView,
    items: Rc<dyn Fn() -> Vec<(u32, gtk::Widget)>>,
    query_positions: bool,
    native_position: Option<Rc<dyn Fn(u32) -> u32>>,
}

impl NativeListScroll {
    pub fn new(
        list: gtk::ListView,
        items: Rc<dyn Fn() -> Vec<(u32, gtk::Widget)>>,
        query_positions: bool,
    ) -> Self {
        Self {
            list,
            items,
            query_positions,
            native_position: None,
        }
    }

    pub fn with_native_position(mut self, map: Rc<dyn Fn(u32) -> u32>) -> Self {
        self.native_position = Some(map);
        self
    }

    pub fn capture(&self) -> Option<ItemScrollPosition> {
        (self.items)()
            .into_iter()
            .filter_map(|(position, widget)| {
                let bounds = widget.compute_bounds(&self.list)?;
                (widget.is_mapped()
                    && bounds.y() < self.list.height() as f32
                    && bounds.y() + bounds.height() > 0.0)
                    .then_some(ItemScrollPosition {
                        position,
                        offset: f64::from(bounds.y()),
                        query_position: self.query_positions.then_some(position as usize),
                    })
            })
            .min_by(|left, right| left.offset.total_cmp(&right.offset))
    }

    pub fn scroll_to(&self, position: u32) {
        let position = self
            .native_position
            .as_ref()
            .map_or(position, |map| map(position));
        let count = self.list.model().map_or(0, |model| model.n_items());
        if count > 0 {
            self.list
                .scroll_to(position.min(count - 1), gtk::ListScrollFlags::NONE, None);
        }
    }

    pub fn restore_offset(&self, saved: ItemScrollPosition) -> bool {
        let Some((_, widget)) = (self.items)()
            .into_iter()
            .find(|(position, _)| *position == saved.position)
        else {
            return false;
        };
        let (Some(bounds), Some(adjustment)) =
            (widget.compute_bounds(&self.list), self.list.vadjustment())
        else {
            return false;
        };
        let value = adjustment.value() + f64::from(bounds.y()) - saved.offset;
        adjustment.set_value(value.clamp(
            adjustment.lower(),
            (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()),
        ));
        true
    }
}

#[derive(Clone)]
pub struct MountedRoute {
    pub widget: gtk::Widget,
    pub resume: MountedRouteResume,
    pub catalog_refresh: MountedRouteResume,
    pub item_navigation: Option<MountedRouteItemNavigation>,
    pub item_scroll: Option<Rc<dyn Fn() -> Option<NativeListScroll>>>,
    pub search: Option<MountedRouteSearchProvider>,
    pub layout_cycle: Option<MountedRouteCommand>,
    pub tab_cycle: Option<MountedRouteCommand>,
    pub initial_demand: MountedRouteResume,
    /// The rows the route shows, which show committed favorites, ratings and downloads.
    pub rows: Vec<Rows<Row>>,
    /// The View of a route that shows no rows of its own, such as Home.
    view: Option<std::sync::Arc<rufin_core::view::View>>,
    /// Shows committed changes in what the route shows outside its rows and header, such as
    /// Home's sections.
    pub states: Option<MountedStates>,
    /// The header the route shows now, and showing it with other downloads.
    pub header: Option<(MountedCurrentHeader, MountedHeader)>,
    /// The committed changes the header shows.
    header_changes: rufin_core::shown::ShownChanges,
    pub home_page_apply: Option<MountedHomePageApply>,
}

impl MountedRoute {
    pub fn new(widget: gtk::Widget, resume: MountedRouteResume) -> Self {
        Self {
            widget,
            catalog_refresh: Rc::clone(&resume),
            resume,
            item_navigation: None,
            item_scroll: None,
            search: None,
            layout_cycle: None,
            tab_cycle: None,
            initial_demand: Rc::new(|| {}),
            rows: Vec::new(),
            view: None,
            states: None,
            header: None,
            header_changes: Default::default(),
            home_page_apply: None,
        }
    }

    pub fn static_widget(widget: gtk::Widget) -> Self {
        Self::new(widget, Rc::new(|| {})).with_catalog_refresh(Rc::new(|| {}))
    }

    pub fn with_catalog_refresh(mut self, refresh: MountedRouteResume) -> Self {
        self.catalog_refresh = refresh;
        self
    }

    pub fn with_item_navigation(mut self, item_navigation: MountedRouteItemNavigation) -> Self {
        self.item_navigation = Some(item_navigation);
        self
    }

    pub fn with_item_scroll(mut self, scroll: Rc<dyn Fn() -> Option<NativeListScroll>>) -> Self {
        self.item_scroll = Some(scroll);
        self
    }

    pub fn item_scroll(&self) -> Option<NativeListScroll> {
        self.item_scroll.as_ref().and_then(|scroll| scroll())
    }

    pub fn with_search(mut self, search: gtk::SearchEntry) -> Self {
        self.search = Some(Rc::new(move || {
            Some(MountedRouteSearchTarget {
                search: search.clone(),
                focus: None,
            })
        }));
        self
    }

    pub fn with_search_provider(mut self, search: MountedRouteSearchProvider) -> Self {
        self.search = Some(search);
        self
    }

    pub fn with_layout_cycle(mut self, cycle: MountedRouteCommand) -> Self {
        self.layout_cycle = Some(cycle);
        self
    }

    pub fn with_tab_cycle(mut self, cycle: MountedRouteCommand) -> Self {
        self.tab_cycle = Some(cycle);
        self
    }

    pub fn with_initial_demand(mut self, resume: MountedRouteResume) -> Self {
        self.initial_demand = resume;
        self
    }

    /// Keeps `rows` showing committed favorites, ratings and downloads.
    pub fn with_rows(mut self, rows: &Rows<Row>) -> Self {
        self.rows.push(rows.clone());
        self
    }

    pub fn with_view(mut self, view: std::sync::Arc<rufin_core::view::View>) -> Self {
        self.view = Some(view);
        self
    }

    /// The View the route shows now, which says what a committed change means for it.
    pub fn view(&self) -> Option<std::sync::Arc<rufin_core::view::View>> {
        self.rows
            .first()
            .and_then(Rows::view)
            .or_else(|| self.view.clone())
    }

    pub fn with_states(mut self, states: MountedStates) -> Self {
        self.states = Some(states);
        self
    }

    /// Shows the downloads the Catalog counts now in the header `current` gives, through `show`.
    pub fn with_header(
        mut self,
        current: impl Fn() -> Option<library::Header> + 'static,
        show: MountedHeader,
    ) -> Self {
        self.header = Some((Rc::new(current), show));
        self
    }

    pub fn with_home_page_apply(mut self, apply: MountedHomePageApply) -> Self {
        self.home_page_apply = Some(apply);
        self
    }

    /// Shows a committed change in what the route shows, through core's
    /// [`rufin_core::shown::ShownChanges`]: its rows, its header and what `states` shows.
    pub fn show_changes(
        &self,
        owner: &rufin_core::source::SourceOwner,
        changed: &rufin_core::source::feed::Changed,
    ) {
        if changed.items.is_empty() && changed.downloads.is_empty() {
            return;
        }
        let view = self.view();
        if let Some(states) = &self.states {
            states(changed, view.as_deref());
        }
        for rows in &self.rows {
            rows.show_changes(owner, changed);
        }
        let Some((current, show)) = self.header.clone() else {
            return;
        };
        let Some(header) = current() else {
            return;
        };
        let shown = self
            .header_changes
            .show(owner, &[&header], changed, view.as_deref());
        if let Some((_, header)) = shown.states.into_iter().next() {
            show(Some(header));
        }
        let Some(downloads) = shown.downloads else {
            return;
        };
        // The downloads go into the header shown now, which keeps what changed meanwhile.
        glib::spawn_future_local(async move {
            use library::ShowsState;
            let Some(read) = downloads.await else {
                return;
            };
            if let (Some((_, read)), Some(mut shown)) = (read.first(), current())
                && shown.show_downloads(*read)
            {
                show(Some(shown));
            }
        });
    }

    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub fn resume(&self) {
        (self.resume)();
    }

    pub fn navigate_items(&self, direction: gtk::DirectionType) -> glib::Propagation {
        if let Some(navigate) = &self.item_navigation {
            navigate(direction)
        } else {
            glib::Propagation::Stop
        }
    }

    pub fn search(&self) -> Option<MountedRouteSearchTarget> {
        self.search.as_ref().and_then(|search| search())
    }

    pub fn cycle_layout(&self) {
        if let Some(cycle) = &self.layout_cycle {
            cycle();
        }
    }

    pub fn cycle_tabs(&self) {
        if let Some(cycle) = &self.tab_cycle {
            cycle();
        }
    }

    pub fn resume_initial_demand(&self) {
        (self.initial_demand)();
    }
}
