use adw::prelude::*;
use gtk::glib;
use playback::TransportStatus;
use std::{
    cell::{Cell, RefCell},
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::Arc,
};
use tracing::warn;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCurrentTrackContext {
    pub context_id: String,
    pub source_rank: usize,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteCurrentTrack {
    pub media_uri: String,
    pub occurrence: playback::OccurrenceId,
    pub context: Option<RouteCurrentTrackContext>,
    pub paused: bool,
}
pub fn route_current_track(player: Option<&playback::PlaybackView>) -> Option<RouteCurrentTrack> {
    let player = player?;
    let entry = player.transport.current.as_ref()?;
    let context = match &entry.provenance {
        playback::Provenance::Context {
            context_id,
            source_rank,
        } => Some(RouteCurrentTrackContext {
            context_id: context_id.to_string(),
            source_rank: *source_rank,
        }),
        playback::Provenance::Manual
        | playback::Provenance::Random
        | playback::Provenance::Radio
        | playback::Provenance::AutoDj
        | playback::Provenance::Legacy => None,
    };
    Some(RouteCurrentTrack {
        media_uri: entry.media_uri.clone(),
        occurrence: entry.id.occurrence.clone(),
        context,
        paused: player.transport.effective_state() == TransportStatus::Paused,
    })
}
pub type RouteCurrentTrackSelection = Rc<dyn Fn(Option<&RouteCurrentTrack>) -> bool>;
pub type MountedRouteResume = Rc<dyn Fn()>;
pub type MountedDownloadChange = Rc<dyn Fn(&downloads::DownloadEvent)>;
pub type MountedRouteCommand = Rc<dyn Fn()>;
pub type MountedRouteSearchProvider = Rc<dyn Fn() -> Option<MountedRouteSearchTarget>>;
pub type MountedFavoriteSettlement = Rc<dyn Fn(rufin_core::runtime::FavoriteSettlement)>;
pub type MountedHomePageApply = Rc<dyn Fn(library::HomePage)>;
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

/// Runs at most one mounted-route read at a time and retains only the newest
/// request while the route still owns this value.
pub struct LatestMountedRouteRead<T: Send + 'static, R: Send + 'static = ()> {
    apply: Rc<dyn Fn(R, T)>,
    load: Arc<dyn Fn(R) -> Pin<Box<dyn Future<Output = T> + Send>> + Send + Sync>,
    runtime: tokio::runtime::Handle,
    context: &'static str,
    generation: Cell<u64>,
    running: Cell<Option<u64>>,
    pending: RefCell<Option<(u64, R)>>,
}

impl<T: Send + 'static, R: Clone + Send + 'static> LatestMountedRouteRead<T, R> {
    pub fn new_with_request(
        runtime: tokio::runtime::Handle,
        apply: Rc<dyn Fn(R, T)>,
        load: Arc<dyn Fn(R) -> Pin<Box<dyn Future<Output = T> + Send>> + Send + Sync>,
        context: &'static str,
    ) -> Rc<Self> {
        Rc::new(Self {
            apply,
            load,
            runtime,
            context,
            generation: Cell::new(0),
            running: Cell::new(None),
            pending: RefCell::new(None),
        })
    }

    pub fn request_with(self: &Rc<Self>, request: R) {
        self.queue(request);
        self.start();
    }

    fn queue(&self, request: R) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.pending.replace(Some((generation, request)));
    }

    fn start(self: &Rc<Self>) {
        if self.running.get().is_some() {
            return;
        }
        let Some((generation, request)) = self.pending.borrow_mut().take() else {
            return;
        };
        self.running.set(Some(generation));
        let load = Arc::clone(&self.load);
        let runtime = self.runtime.clone();
        let read = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let load_request = request.clone();
            let result = runtime.spawn(async move { load(load_request).await }).await;
            let Some(read) = read.upgrade() else {
                return;
            };
            read.running.set(None);
            let value = match result {
                Ok(value) => value,
                Err(_) => {
                    warn!(context = read.context, "route projection task panicked");
                    read.start();
                    return;
                }
            };
            if read.generation.get() != generation {
                read.start();
                return;
            }
            (read.apply)(request, value);
        });
    }
}

#[cfg(test)]
mod latest_mounted_route_read_tests {
    use super::*;

    #[test]
    fn latest_route_read_applies_only_the_newest_request() {
        let runtime = tokio::runtime::Runtime::new().expect("route test runtime");
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (started_sender, started_receiver) = async_channel::bounded(2);
                    let (release_sender, release_receiver) = async_channel::bounded(2);
                    let (applied_sender, applied_receiver) = async_channel::bounded(2);
                    let load = Arc::new(move |request: usize| {
                        let started = started_sender.clone();
                        let release = release_receiver.clone();
                        Box::pin(async move {
                            started.send(request).await.expect("publish started read");
                            release.recv().await.expect("release route read");
                            request
                        }) as Pin<Box<dyn Future<Output = usize> + Send>>
                    });
                    let apply = Rc::new(move |request: usize, value: usize| {
                        assert_eq!(request, value);
                        applied_sender
                            .try_send(value)
                            .expect("publish applied route read");
                    });
                    let read = LatestMountedRouteRead::new_with_request(
                        runtime.handle().clone(),
                        apply,
                        load,
                        "test route",
                    );

                    read.request_with(0);
                    assert_eq!(started_receiver.recv().await.unwrap(), 0);
                    read.request_with(1);
                    read.request_with(2);
                    release_sender.send(()).await.unwrap();
                    assert_eq!(started_receiver.recv().await.unwrap(), 2);
                    assert!(applied_receiver.is_empty());
                    release_sender.send(()).await.unwrap();
                    assert_eq!(applied_receiver.recv().await.unwrap(), 2);
                    assert!(applied_receiver.is_empty());
                })
            })
            .expect("install route test MainContext");
    }

    #[test]
    fn detached_route_read_does_not_publish() {
        struct DropNotice(async_channel::Sender<()>);
        impl Drop for DropNotice {
            fn drop(&mut self) {
                let _ = self.0.try_send(());
            }
        }

        let runtime = tokio::runtime::Runtime::new().expect("route test runtime");
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let (started_sender, started_receiver) = async_channel::bounded(1);
                    let (release_sender, release_receiver) = async_channel::bounded(1);
                    let (dropped_sender, dropped_receiver) = async_channel::bounded(1);
                    let applied = Rc::new(Cell::new(false));
                    let load = Arc::new(move |(): ()| {
                        let started = started_sender.clone();
                        let release = release_receiver.clone();
                        let dropped = dropped_sender.clone();
                        Box::pin(async move {
                            started.send(()).await.expect("publish started read");
                            release.recv().await.expect("release detached read");
                            DropNotice(dropped)
                        })
                            as Pin<Box<dyn Future<Output = DropNotice> + Send>>
                    });
                    let apply_flag = Rc::clone(&applied);
                    let read = LatestMountedRouteRead::new_with_request(
                        runtime.handle().clone(),
                        Rc::new(move |(), _| apply_flag.set(true)),
                        load,
                        "detached test route",
                    );

                    read.request_with(());
                    started_receiver.recv().await.unwrap();
                    drop(read);
                    release_sender.send(()).await.unwrap();
                    dropped_receiver.recv().await.unwrap();
                    assert!(!applied.get());
                })
            })
            .expect("install route test MainContext");
    }
}

#[derive(Clone)]
pub struct MountedRoute {
    pub widget: gtk::Widget,
    pub resume: MountedRouteResume,
    pub catalog_refresh: MountedRouteResume,
    pub item_navigation: Option<MountedRouteItemNavigation>,
    pub search: Option<MountedRouteSearchProvider>,
    pub layout_cycle: Option<MountedRouteCommand>,
    pub tab_cycle: Option<MountedRouteCommand>,
    pub initial_demand: MountedRouteResume,
    pub favorite_settlement: Option<MountedFavoriteSettlement>,
    pub download_change: Option<MountedDownloadChange>,
    pub home_page_apply: Option<MountedHomePageApply>,
}

impl MountedRoute {
    pub fn new(widget: gtk::Widget, resume: MountedRouteResume) -> Self {
        Self {
            widget,
            catalog_refresh: Rc::clone(&resume),
            resume,
            item_navigation: None,
            search: None,
            layout_cycle: None,
            tab_cycle: None,
            initial_demand: Rc::new(|| {}),
            favorite_settlement: None,
            download_change: None,
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

    pub fn with_favorite_settlement(mut self, apply: MountedFavoriteSettlement) -> Self {
        self.favorite_settlement = Some(apply);
        self
    }

    pub fn with_download_change(mut self, apply: MountedDownloadChange) -> Self {
        self.download_change = Some(match self.download_change.take() {
            Some(previous) => Rc::new(move |event| {
                previous(event);
                apply(event);
            }),
            None => apply,
        });
        self
    }

    pub fn with_home_page_apply(mut self, apply: MountedHomePageApply) -> Self {
        self.home_page_apply = Some(apply);
        self
    }

    pub fn apply_favorite_settlement(&self, settlement: rufin_core::runtime::FavoriteSettlement) {
        if let Some(apply) = &self.favorite_settlement {
            apply(settlement);
        }
    }

    pub fn apply_home_page(&self, home: library::HomePage) {
        if let Some(apply) = &self.home_page_apply {
            apply(home);
        }
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
