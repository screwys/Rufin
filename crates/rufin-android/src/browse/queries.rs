use super::*;
use library::{Item, MemberRead};
use rufin_core::playback::{PlayRequest, QueuePlacement};
use rufin_core::source::feed::Changed;
use rufin_core::view::Refresh;

#[uniffi::export]
impl AndroidBrowseQuery {
    /// Whether a committed change means the page reads again. Favorites and ratings read it
    /// again only where they order or filter its rows; elsewhere rows show them through
    /// `show_state`.
    pub fn refresh(&self, changed: Changed) -> bool {
        rufin_core::view::refresh(
            &self.source,
            &self.route,
            self.selected.as_ref(),
            &self.options,
            &changed,
        ) != Refresh::Nothing
            || (!changed.items.is_empty()
                && self.view.get().is_some_and(|view| view.states_reorder()))
    }

    /// The context id the page's plays carry.
    pub fn context_id(&self) -> String {
        rufin_core::view::context_id(
            &self.route,
            self.selected.as_ref(),
            &self.options,
            self.sort,
            self.descending,
        )
    }

    pub fn home_section_title(&self, id: String) -> Option<String> {
        let block =
            serde_json::from_value::<library::HomeBlockKind>(serde_json::Value::String(id)).ok()?;
        Some(localization::tr(
            rufin_core::settings::presentation::home_block_title(block),
        ))
    }
    pub fn supports_downloaded_filter(&self) -> bool {
        rufin_core::view::supports_downloaded_filter(&self.route, self.options.category)
    }

    pub fn supports_favorite_filter(&self) -> bool {
        rufin_core::view::supports_favorite_filter(&self.route, self.options.category)
    }

    pub fn breadcrumbs(&self) -> Vec<AndroidBreadcrumb> {
        let Route::Folders { path } = &self.route else {
            return Vec::new();
        };
        let mut items = vec![AndroidBreadcrumb {
            title: localization::tr("Folders"),
            route: Route::Folders { path: Vec::new() },
        }];
        items.extend(
            path.iter()
                .enumerate()
                .map(|(index, item)| AndroidBreadcrumb {
                    title: item.name.clone(),
                    route: Route::Folders {
                        path: path[..=index].to_vec(),
                    },
                }),
        );
        items
    }

    pub async fn section_positions(
        self: Arc<Self>,
    ) -> Result<Vec<AndroidScrollSection>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let sections = self
                .view(&cancel)
                .await
                .map_err(error)?
                .sections(&cancel)
                .await
                .map_err(error)?;
            Ok(sections
                .into_iter()
                .map(|section| AndroidScrollSection {
                    title: section.title,
                    index: section.index,
                })
                .collect())
        })
        .await
    }

    pub async fn play_section(
        self: Arc<Self>,
        section_id: String,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        if !matches!(self.route, Route::Home) {
            return Err(error("Sections belong to Home"));
        }
        let placement = queue_placement(&placement)?;
        let title = context_caption(&self.route, context_title);
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let cancel = ReadCancellation::new();
                let view = self.view(&cancel).await.map_err(error)?;
                let selections = self
                    .home_rows(view)
                    .unwrap_or_default()
                    .iter()
                    .filter(|row| row.section_id == section_id)
                    .filter_map(home_selection)
                    .collect();
                self.playback.play_selection(
                    PlayRequest::captured(MemberSelection::Concat(selections), 0, placement, false)
                        .with_context(
                            format!("home:{}:{section_id}", self.selected().source_id).into(),
                        )
                        .with_context_title(title)
                        .shuffled(shuffled),
                );
                Ok(())
            })
            .await
            .map_err(error)?
    }

    pub async fn count(self: Arc<Self>) -> Result<u64, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let view = self.view(&cancel).await.map_err(error)?;
            let count = self.home_rows(view).map_or_else(|| view.len(), <[_]>::len);
            Ok(count as u64)
        })
        .await
    }

    pub fn sort_fields(&self) -> Vec<AndroidSortField> {
        let key = library_list_key(&self.route, self.options.category);
        rufin_core::settings::available_sort_fields(key)
            .iter()
            .map(|field| AndroidSortField {
                id: *field,
                title: localization::tr(rufin_core::settings::presentation::library_field_title(
                    *field,
                )),
            })
            .collect()
    }

    pub fn display_settings(&self) -> AndroidBrowseDisplay {
        let mut display = self.display.clone();
        display.sort_key = self.sort;
        display.descending = self.descending;
        browse_display(
            library_list_key(&self.route, self.options.category),
            &display,
        )
    }

    pub async fn page(
        self: Arc<Self>,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
        let runtime = self.runtime.clone();
        run_read(&runtime, move |cancel| async move {
            let offset = usize::try_from(offset).map_err(error)?;
            self.rows(offset..offset + limit.min(256) as usize, &cancel)
                .await
                .map_err(error)
        })
        .await
    }

    /// Plays the page from its row `index`. A track of the Albums list in Detail layout plays
    /// its album, which `context_title` names.
    pub async fn play(
        self: Arc<Self>,
        index: u64,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let placement = QueuePlacement::Now;
        let runtime = self.runtime.clone();
        runtime
            .spawn(async move {
                let index = usize::try_from(index).map_err(error)?;
                let cancel = ReadCancellation::new();
                let view = self.view(&cancel).await.map_err(error)?;
                let gone = || error("Track is no longer in this route");
                let caption = context_caption(&self.route, context_title.clone());
                let request = if let Some(rows) = self.home_rows(view) {
                    let key = rows
                        .get(index)
                        .and_then(AndroidBrowseRow::track)
                        .ok_or_else(gone)?
                        .track_key;
                    let keys = rows
                        .iter()
                        .filter_map(|row| Some(row.track()?.track_key))
                        .collect::<Vec<_>>();
                    let anchor = rows[..index]
                        .iter()
                        .filter(|row| row.track().is_some())
                        .count();
                    PlayRequest::captured(
                        MemberSelection::Keys {
                            source: None,
                            keys: keys.into(),
                        },
                        anchor,
                        placement,
                        false,
                    )
                    .with_context(self.context_id().into())
                    .with_anchor(key, None)
                    .with_context_title(caption)
                } else {
                    match view.item(index, &cancel).await.map_err(error)? {
                        Some(Item::AlbumTrack { key, album, .. }) if self.album_detail_layout() => {
                            let mut read = MemberRead::new(Collection::Album(album));
                            read.sort = Some(self.album_tracks.sort_key);
                            read.descending = self.album_tracks.descending;
                            let album_uri = self
                                .catalog
                                .uri(Item::Album(album))
                                .ok_or_else(|| error("Album is no longer available"))?;
                            PlayRequest::captured(read.selection(), 0, placement, false)
                                .with_context(
                                    rufin_core::view::context_id(
                                        &Route::AlbumDetail(album_uri),
                                        None,
                                        &OpenOptions::default(),
                                        self.album_tracks.sort_key,
                                        self.album_tracks.descending,
                                    )
                                    .into(),
                                )
                                .with_anchor(key, None)
                                .with_context_title(context_title.map(|title| {
                                    Arc::new(library::QueueContextTitle {
                                        kind: Some("Album".into()),
                                        title: title.into(),
                                    })
                                }))
                        }
                        item => {
                            let read = view.members().ok_or_else(gone)?;
                            let (key, entry) = match item {
                                Some(Item::Track { key, entry }) => (key, entry),
                                // A History row names its listen; its row holds the track.
                                Some(Item::Listen(_)) => (
                                    view.rows(index..index + 1, &cancel)
                                        .await
                                        .map_err(error)?
                                        .into_iter()
                                        .find_map(|row| match row {
                                            Row::Track(track) => Some(track.track_key),
                                            _ => None,
                                        })
                                        .ok_or_else(gone)?,
                                    None,
                                ),
                                _ => return Err(gone()),
                            };
                            PlayRequest::captured(
                                read.clone().selection(),
                                index - view.folder_len().unwrap_or(0),
                                placement,
                                false,
                            )
                            .with_context(self.context_id().into())
                            .with_anchor(key, entry)
                            .with_context_title(caption)
                        }
                    }
                };
                self.playback.play_selection(request.shuffled(shuffled));
                Ok(())
            })
            .await
            .map_err(error)?
    }

    /// Plays or queues the page whole, as its header does.
    pub async fn play_all(
        self: Arc<Self>,
        placement: String,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        let placement = queue_placement(&placement)?;
        let selection = self.clone().selection().await?;
        self.playback.play_selection(
            PlayRequest::ordered(Arc::unwrap_or_clone(selection), 0, placement, true)
                .with_context(self.context_id().into())
                .with_context_title(context_caption(&self.route, context_title))
                .shuffled(shuffled),
        );
        Ok(())
    }
}
