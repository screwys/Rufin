use super::*;

fn search_result_matches(
    kind: RecentSearchKind,
    media_uri: &str,
    current: &ui_shared::mounted_route::RouteCurrentTrack,
) -> bool {
    let prefix = match kind {
        RecentSearchKind::Track => return media_uri == current.media_uri,
        RecentSearchKind::Album => "album:",
        RecentSearchKind::Artist => "artist:",
    };
    current.context.as_ref().is_some_and(|context| {
        context
            .context_id
            .strip_prefix(prefix)
            .and_then(|uri| uri.strip_prefix(media_uri))
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with("|query="))
    })
}

pub struct SearchPreview {
    pub title: String,
    pub subtitle: String,
    pub artwork: ArtworkBinding,
}

pub const SEARCH_PREVIEW_LIMIT: usize = 30;

/// One current query shared by the header and the Search route.
pub struct SearchSession {
    selected: RefCell<Option<SelectedLibrary>>,
    query: RefCell<String>,
    pub(super) items: RefCell<[Vec<SearchItem>; 3]>,
    status: Cell<&'static str>,
    pub(super) revision: Cell<u64>,
    generation: Cell<u64>,
    debounce: RefCell<Option<glib::SourceId>>,
    cancellation: RefCell<Option<ReadCancellation>>,
    observers: RefCell<Vec<Weak<dyn Fn()>>>,
    pub(super) category: Cell<CollectionCategory>,
}

impl Default for SearchSession {
    fn default() -> Self {
        Self {
            selected: RefCell::new(None),
            query: RefCell::new(String::new()),
            items: RefCell::new(Default::default()),
            status: Cell::new("initial"),
            revision: Cell::new(0),
            generation: Cell::new(0),
            debounce: RefCell::new(None),
            cancellation: RefCell::new(None),
            observers: RefCell::new(Vec::new()),
            category: Cell::new(CollectionCategory::default()),
        }
    }
}

impl SearchSession {
    pub fn play_recent(
        result: &RecentSearchResult,
        catalog: &Rc<CatalogUi>,
        placement: QueuePlacement,
    ) {
        catalog
            .settings
            .update_app_settings("recent search", |settings| {
                settings.remember_search_result(result.clone())
            });
        if result.kind == RecentSearchKind::Track {
            (catalog.media_menus.play_target)(
                &PlaybackTarget::Track(result.media_uri.clone()),
                placement,
                false,
            );
            return;
        }
        let source = MediaDragSource::LiveCollection {
            media_uri: result.media_uri.clone(),
            operations: catalog.source.clone(),
        };
        let queue = catalog.queue.clone();
        catalog.runtime.spawn(async move {
            match source.queue_input().await {
                Ok(input) => queue.play(playback::PlayRequest::ordered(input, 0, placement, true)),
                Err(error) => tracing::warn!(%error, "could not prepare recent search playback"),
            }
        });
    }

    pub fn activate_recent(result: &RecentSearchResult, catalog: &Rc<CatalogUi>) {
        let route = match result.kind {
            RecentSearchKind::Track => {
                Self::play_recent(result, catalog, QueuePlacement::Now);
                return;
            }
            RecentSearchKind::Album => Route::AlbumDetail(result.media_uri.clone()),
            RecentSearchKind::Artist => Route::ArtistDetail(result.media_uri.clone()),
        };
        catalog
            .settings
            .update_app_settings("recent search", |settings| {
                settings.remember_search_result(result.clone())
            });
        let receiver = catalog.source.prepare_collection(result.media_uri.clone());
        let is_current = Rc::clone(&catalog.is_current);
        let navigate = catalog.route_navigation();
        glib::spawn_future_local(async move {
            if let Ok(Err(error)) = receiver.recv().await {
                tracing::warn!(%error, "could not acquire recent search collection");
            }
            if is_current() {
                navigate(route);
            }
        });
    }

    pub fn observe(&self, callback: impl Fn() + 'static) -> Rc<dyn Fn()> {
        let callback: Rc<dyn Fn()> = Rc::new(callback);
        self.observers.borrow_mut().push(Rc::downgrade(&callback));
        callback
    }

    fn notify(&self) {
        let callbacks: Vec<_> = self
            .observers
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        self.observers
            .borrow_mut()
            .retain(|observer| observer.strong_count() != 0);
        for callback in callbacks {
            callback();
        }
    }

    pub fn query(&self) -> String {
        self.query.borrow().clone()
    }

    pub fn status(&self) -> &'static str {
        self.status.get()
    }

    pub fn set_category(&self, category: CollectionCategory) {
        self.category.set(category);
        self.notify();
    }

    pub fn set_selected(self: &Rc<Self>, selected: Option<SelectedLibrary>) {
        let changed = self
            .selected
            .borrow()
            .as_ref()
            .map(|s| (&s.source_id, s.music_folder_key, &s.music_folder_object_id))
            != selected
                .as_ref()
                .map(|s| (&s.source_id, s.music_folder_key, &s.music_folder_object_id));
        self.selected.replace(selected);
        if changed {
            self.schedule();
        }
    }

    pub fn set_query(self: &Rc<Self>, query: &str) {
        let query = query.trim();
        if *self.query.borrow() == query {
            return;
        }
        self.query.replace(query.to_owned());
        self.schedule();
    }

    fn cancel(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        if let Some(cancellation) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
        }
    }

    fn schedule(self: &Rc<Self>) {
        self.cancel();
        self.items.replace(Default::default());
        self.revision.set(self.revision.get().wrapping_add(1));
        if self.query.borrow().is_empty() || self.selected.borrow().is_none() {
            self.status.set("initial");
        } else {
            self.status.set("loading");
            let weak = Rc::downgrade(self);
            self.debounce.replace(Some(glib::timeout_add_local_once(
                SEARCH_DEBOUNCE,
                move || {
                    if let Some(session) = weak.upgrade() {
                        session.debounce.borrow_mut().take();
                        session.submit();
                    }
                },
            )));
        }
        self.notify();
    }

    /// Enter skips only the pending delay, never repeats an active or completed query.
    pub fn submit(self: &Rc<Self>) {
        if self.status.get() != "loading" || self.cancellation.borrow().is_some() {
            return;
        }
        if let Some(source) = self.debounce.borrow_mut().take() {
            source.remove();
        }
        let Some(selected) = self.selected.borrow().clone() else {
            return;
        };
        let generation = self.generation.get();
        let cancellation = ReadCancellation::new();
        self.cancellation.replace(Some(cancellation.clone()));
        let query = self.query();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = acquire_search(&selected, query, cancellation).await;
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.generation.get() != generation {
                return;
            }
            session.cancellation.borrow_mut().take();
            match result {
                Ok(items) => {
                    session.items.replace(items);
                    session.status.set("results");
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to load Search results");
                    session.status.set("error");
                }
            }
            session.revision.set(session.revision.get().wrapping_add(1));
            session.notify();
        });
    }

    pub fn previews(&self) -> [Vec<SearchPreview>; 3] {
        let items = self.items.borrow();
        let mut counts = [0; 3];
        let mut total = 0;
        while total < SEARCH_PREVIEW_LIMIT {
            let previous = total;
            for category in [
                CollectionCategory::Tracks,
                CollectionCategory::Tracks,
                CollectionCategory::Albums,
                CollectionCategory::Artists,
            ] {
                let index = category as usize;
                if total < SEARCH_PREVIEW_LIMIT && counts[index] < items[index].len() {
                    counts[index] += 1;
                    total += 1;
                }
            }
            if total == previous {
                break;
            }
        }
        std::array::from_fn(|index| {
            items[index]
                .iter()
                .take(counts[index])
                .map(|item| SearchPreview {
                    title: item.title().to_owned(),
                    subtitle: item.subtitle().to_owned(),
                    artwork: item.artwork(),
                })
                .collect()
        })
    }

    pub fn activate(&self, category: CollectionCategory, index: usize, catalog: &Rc<CatalogUi>) {
        let item = self.items.borrow()[category as usize].get(index).cloned();
        if let Some(item) = item {
            item.activate(catalog);
        }
    }

    pub fn play(&self, category: CollectionCategory, index: usize, catalog: &Rc<CatalogUi>) {
        let item = self.items.borrow()[category as usize].get(index).cloned();
        if let Some(item) = item {
            item.play(catalog, QueuePlacement::Now);
        }
    }

    pub fn preview_matches(
        &self,
        category: CollectionCategory,
        index: usize,
        current: &ui_shared::mounted_route::RouteCurrentTrack,
    ) -> bool {
        self.items.borrow()[category as usize]
            .get(index)
            .is_some_and(|item| {
                let (kind, uri) = match item {
                    SearchItem::Track(row) => (RecentSearchKind::Track, row.media_uri.as_str()),
                    SearchItem::Album(row) => (RecentSearchKind::Album, row.media_uri.as_str()),
                    SearchItem::Artist(row) => (RecentSearchKind::Artist, row.media_uri.as_str()),
                };
                search_result_matches(kind, uri, current)
            })
    }

    pub fn recent_matches(
        result: &RecentSearchResult,
        current: &ui_shared::mounted_route::RouteCurrentTrack,
    ) -> bool {
        search_result_matches(result.kind, &result.media_uri, current)
    }

    pub fn present_context(
        self: &Rc<Self>,
        category: CollectionCategory,
        index: usize,
        target: &gtk::Widget,
        catalog: &Rc<CatalogUi>,
    ) {
        let item = self.items.borrow()[category as usize].get(index).cloned();
        if let Some(item) = item {
            let generation = self.generation.get();
            let session = Rc::downgrade(self);
            let current = Rc::clone(&catalog.is_current);
            item.present_context(
                target,
                catalog,
                None,
                Rc::new(move || {
                    current()
                        && session
                            .upgrade()
                            .is_some_and(|session| session.generation.get() == generation)
                }),
            );
        }
    }
}

impl Drop for SearchSession {
    fn drop(&mut self) {
        self.cancel();
    }
}
