use super::*;
use rufin_core::source::feed::Changed;

fn route_descriptor(item: SidebarRouteItem) -> AndroidRouteDescriptor {
    let descriptor = sidebar_route_descriptor(item);
    AndroidRouteDescriptor {
        item,
        id: descriptor.stable_id.into(),
        title: localization::tr(descriptor.title),
        icon_name: descriptor.icon_name.into(),
        selected_icon_name: descriptor.selected_icon_name().into(),
        route: descriptor.root_route,
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub async fn item_download_status(
        &self,
        item: library::DownloadItem,
    ) -> Result<library::DownloadStatus, AndroidError> {
        let catalog = self.catalog.clone();
        run_read(&self.runtime, move |cancel| async move {
            catalog
                .item_download_status(&item, &cancel)
                .await
                .map_err(error)
        })
        .await
    }

    pub async fn folder_download_status(
        &self,
        path: Vec<rufin_core::route::FolderPathItem>,
    ) -> Result<library::DownloadStatus, AndroidError> {
        let selection = self.route_selection(Route::Folders { path }).await?;
        let MemberSelection::Members { mut read, .. } = Arc::unwrap_or_clone(selection) else {
            unreachable!("A folder lists its tracks")
        };
        let catalog = self.catalog.clone();
        run_read(&self.runtime, move |cancel| async move {
            let count = |read: library::MemberRead| {
                let catalog = &catalog;
                let cancel = &cancel;
                async move {
                    Ok::<_, AndroidError>(
                        catalog
                            .order(&library::Read::Members(read), cancel)
                            .await
                            .map_err(error)?
                            .len() as u64,
                    )
                }
            };
            let total = count(read.clone()).await?;
            read.downloaded_only = true;
            let downloaded = count(read).await?;
            Ok(library::DownloadStatus { total, downloaded })
        })
        .await
    }

    pub async fn download_target(
        &self,
        route: Option<Route>,
        media_uri: String,
        remove: bool,
    ) -> Result<(), AndroidError> {
        let selection = match &route {
            Some(route) => Some(self.route_selection(route.clone()).await?),
            None => None,
        };
        // A download takes a collection in every music folder; a folder's tracks and a smart
        // playlist's members are the ones of the folder they are read in.
        let every_music_folder = !matches!(route, Some(Route::Folders { .. }));
        let source = self.source.clone();
        self.runtime
            .spawn(async move {
                let selection = match selection {
                    Some(selection) => {
                        let mut selection = Arc::unwrap_or_clone(selection);
                        if let MemberSelection::Members { read, .. } = &mut selection
                            && every_music_folder
                            && !matches!(read.collection, Collection::Smart { .. })
                        {
                            read.folder = None;
                        }
                        selection
                    }
                    None => MemberSelection::Uris(vec![media_uri].into()),
                };
                let result = if remove {
                    source.remove_download_selection(selection)
                } else {
                    source.download_selection(
                        selection,
                        route
                            .map(|route| {
                                serde_json::to_string(&route).expect("Route serialization")
                            })
                            .unwrap_or_else(|| "track".into()),
                        None,
                    )
                };
                result.recv().await.map_err(error)?.map_err(error)
            })
            .await
            .map_err(error)?
    }
}

impl AndroidLibrary {
    fn update_browse_fields(
        &self,
        key: LibraryListKey,
        update: impl FnOnce(&mut LibraryListSettings),
    ) -> Result<AndroidBrowseDisplay, AndroidError> {
        self.settings
            .update_library_list_settings(key, update)
            .map_err(error)?;
        Ok(browse_display(key, &self.settings.load().library_list(key)))
    }
}

#[uniffi::export]
impl AndroidLibrary {
    pub fn music_folders(&self) -> AndroidMusicFolderState {
        let selected = self.source.selected_library();
        let mut choices = vec![AndroidMusicFolder {
            id: None,
            title: localization::tr("All Music"),
        }];
        if let Some(selected) = &selected {
            choices.extend(
                selected
                    .music_folders
                    .iter()
                    .map(|folder| AndroidMusicFolder {
                        id: Some(folder.object_id.clone()),
                        title: folder.name.clone(),
                    }),
            );
        }
        AndroidMusicFolderState {
            selected_id: selected.and_then(|selected| selected.music_folder_object_id),
            choices,
        }
    }

    pub async fn set_music_folder(&self, id: Option<String>) -> Result<(), AndroidError> {
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        self.source
            .set_music_folder(selected.source_id, id)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn set_browse_display(
        &self,
        route: Route,
        category: CollectionCategory,
        layout: LibraryLayout,
        size: DisplaySize,
        grid_spacing: DisplaySize,
    ) -> Result<AndroidBrowseDisplay, AndroidError> {
        let key = library_list_key(&route, category);
        self.settings
            .update_library_list_settings(key, |settings| {
                settings.layout = layout;
                settings.display.size = size;
                settings.display.grid_spacing = grid_spacing;
            })
            .map_err(error)?;
        Ok(browse_display(key, &self.settings.load().library_list(key)))
    }
    /// Shows or hides `field` in the page's layout.
    pub fn show_browse_field(
        &self,
        route: Route,
        category: CollectionCategory,
        field: LibraryField,
        shown: bool,
    ) -> Result<AndroidBrowseDisplay, AndroidError> {
        let key = library_list_key(&route, category);
        self.update_browse_fields(key, |settings| {
            settings.show_field(key, settings.layout, field, shown);
        })
    }

    /// Moves a shown `field` one place earlier (`up`) or later in the page's layout.
    pub fn move_browse_field(
        &self,
        route: Route,
        category: CollectionCategory,
        field: LibraryField,
        up: bool,
    ) -> Result<AndroidBrowseDisplay, AndroidError> {
        let key = library_list_key(&route, category);
        self.update_browse_fields(key, |settings| {
            settings.move_field(settings.layout, field, if up { -1 } else { 1 });
        })
    }

    pub fn set_browse_sort(
        &self,
        route: Route,
        category: CollectionCategory,
        sort: LibraryField,
        descending: bool,
    ) -> Result<(), AndroidError> {
        let key = library_list_key(&route, category);
        self.settings
            .update_library_list_settings(key, |settings| {
                settings.sort_key = sort;
                settings.descending = descending;
            })
            .map_err(error)?;
        Ok(())
    }

    /// Plays the radio of the track `track` names, or of the collection `route` shows.
    pub async fn play_radio(
        &self,
        track: Option<String>,
        route: Option<Route>,
        placement: rufin_core::playback::QueuePlacement,
    ) -> Result<(), AndroidError> {
        let seed = if let Some(track) = track {
            library::RadioSeed::Track(track)
        } else {
            let route = route.ok_or_else(|| error("Choose a radio seed"))?;
            self.catalog.loaded().await;
            match &route {
                Route::AlbumDetail(uri)
                | Route::ArtistDetail(uri)
                | Route::AlbumArtistDetail(uri) => match (self.catalog.item(uri), &route) {
                    (Some(library::Item::Album(key)), Route::AlbumDetail(_)) => {
                        Some(library::RadioSeed::Album(key))
                    }
                    (Some(library::Item::Artist { key, .. }), Route::ArtistDetail(_)) => {
                        Some(library::RadioSeed::Artist(key))
                    }
                    (Some(library::Item::Artist { key, .. }), Route::AlbumArtistDetail(_)) => {
                        Some(library::RadioSeed::AlbumArtist(key))
                    }
                    _ => None,
                },
                Route::GenreDetail(key) => Some(library::RadioSeed::Genre(*key)),
                Route::PlaylistDetail(key) => Some(library::RadioSeed::Playlist(*key)),
                _ => None,
            }
            .ok_or_else(|| error("This item has no radio action"))?
        };
        self.playback
            .play_radio(rufin_core::playback::RadioPlayRequest { seed, placement });
        Ok(())
    }

    pub fn is_pinned(&self, pin: SidebarPin) -> Result<bool, AndroidError> {
        Ok(self.settings.changes().borrow().ui.sidebar.is_pinned(&pin))
    }

    pub async fn refresh_home(&self, block: library::HomeBlockKind) -> Result<(), AndroidError> {
        let kind = block
            .section_kind()
            .ok_or_else(|| error("This section has no refresh"))?;
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        self.source
            .refresh_home(selected.source_id, kind)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn configured_routes(&self) -> Vec<AndroidRouteDescriptor> {
        self.settings
            .changes()
            .borrow()
            .ui
            .sidebar
            .route_items
            .iter()
            .filter(|item| item.visible)
            .map(|item| route_descriptor(item.item))
            .collect()
    }

    pub fn route_settings(&self) -> Vec<AndroidRouteSetting> {
        self.settings
            .changes()
            .borrow()
            .ui
            .sidebar
            .route_items
            .iter()
            .map(|item| AndroidRouteSetting {
                descriptor: route_descriptor(item.item),
                visible: item.visible,
            })
            .collect()
    }

    pub async fn set_route_visible(&self, id: String, visible: bool) -> Result<bool, AndroidError> {
        let item = SidebarRouteItem::from_stable_id(&id).ok_or_else(|| error("Unknown route"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.set_route_visible(item, visible))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn move_route(&self, id: String, up: bool) -> Result<bool, AndroidError> {
        let item = SidebarRouteItem::from_stable_id(&id).ok_or_else(|| error("Unknown route"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.move_route(item, if up { -1 } else { 1 }))
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn reorder_route(
        &self,
        id: String,
        target_id: String,
        after: bool,
    ) -> Result<(), AndroidError> {
        let item = SidebarRouteItem::from_stable_id(&id).ok_or_else(|| error("Unknown route"))?;
        let target =
            SidebarRouteItem::from_stable_id(&target_id).ok_or_else(|| error("Unknown route"))?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || {
                settings.update_preferences(move |ui| {
                    ui.sidebar.reorder_route(item, target, after);
                    Ok(())
                })
            })
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub fn play_rows(
        &self,
        context: String,
        media_uris: Vec<String>,
        index: u64,
        placement: rufin_core::playback::QueuePlacement,
        shuffled: bool,
        context_title: Option<String>,
    ) -> Result<(), AndroidError> {
        self.playback.play_selection(
            rufin_core::playback::PlayRequest::uris(media_uris, placement)
                .with_context(context.into())
                .with_anchor_index(usize::try_from(index).map_err(error)?)
                .with_context_title(context_title.map(|title| {
                    Arc::new(library::QueueContextTitle {
                        kind: None,
                        title: title.into(),
                    })
                }))
                .shuffled(shuffled),
        );
        Ok(())
    }

    pub fn collection_categories(&self) -> Vec<AndroidCollectionCategory> {
        CollectionCategory::ALL
            .into_iter()
            .map(|category| AndroidCollectionCategory {
                category,
                title: localization::tr(category.title()),
                icon_name: category.icon_name().into(),
            })
            .collect()
    }

    pub async fn random_genre(
        &self,
        route: Option<Route>,
    ) -> Result<Option<rufin_core::settings::RandomPlayGenreSelection>, AndroidError> {
        let Some(route) = route else {
            return Ok(None);
        };
        let Route::GenreDetail(key) = route else {
            return Err(error("Choose a genre"));
        };
        let selected = self
            .source
            .selected_library()
            .ok_or_else(|| error("No source selected"))?;
        let (_, _, genre_id) = self
            .catalog
            .uri(library::Item::Genre(key))
            .and_then(|uri| library::source_entity_parts(&uri))
            .ok_or_else(|| error("Genre is no longer available"))?;
        Ok(Some(rufin_core::settings::RandomPlayGenreSelection {
            source_id: selected.source_id,
            music_folder_id: selected.music_folder_object_id,
            genre_id,
        }))
    }

    pub async fn random_genre_title(
        &self,
        settings: rufin_core::settings::RandomPlaySettings,
    ) -> Result<String, AndroidError> {
        let Some(selected) = self.source.selected_library() else {
            return Ok(localization::tr("Any genre"));
        };
        let catalog = self.catalog.clone();
        let genre = self
            .runtime
            .spawn(async move {
                rufin_core::radio::saved_random_genre(
                    &catalog,
                    &settings,
                    &selected.source_id,
                    selected.source_key,
                    selected.music_folder_object_id.as_deref(),
                )
                .await
            })
            .await
            .map_err(error)?
            .map_err(error)?;
        Ok(genre.map_or_else(|| localization::tr("Any genre"), |genre| genre.name))
    }

    pub async fn random_play(
        &self,
        settings: rufin_core::settings::RandomPlaySettings,
        placement: rufin_core::playback::QueuePlacement,
    ) -> Result<bool, AndroidError> {
        let owner = self.settings.clone();
        let settings = self
            .runtime
            .spawn_blocking(move || owner.set_random_preferences(settings))
            .await
            .map_err(error)?
            .map_err(error)?;
        rufin_core::radio::play_saved_random(&self.playback, settings, placement)
            .await
            .map_err(error)
    }

    pub fn routes(&self) -> Vec<AndroidRouteDescriptor> {
        SidebarRouteItem::all()
            .into_iter()
            .map(route_descriptor)
            .collect()
    }

    pub async fn prepare_collection(&self, media_uri: String) -> Result<(), AndroidError> {
        self.source
            .prepare_collection(media_uri)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)
    }

    pub async fn rating(
        &self,
        target: library::FavoriteTarget,
    ) -> Result<Option<u8>, AndroidError> {
        let catalog = self.catalog.clone();
        self.runtime
            .spawn(async move {
                let cancellation = ReadCancellation::new();
                let user_rating = catalog
                    .user_state(target.media_uri(), &cancellation)
                    .await
                    .map_err(error)?
                    .and_then(|(_, rating)| rating);
                let value = if user_rating.is_some() {
                    user_rating
                } else {
                    let rating = match &target {
                        library::FavoriteTarget::Track(uri) => catalog
                            .track(uri, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.state.rating),
                        library::FavoriteTarget::Album(uri) => catalog
                            .album(uri, false, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.row.state.rating),
                        library::FavoriteTarget::Artist(uri) => catalog
                            .artist(uri, false, false, &cancellation)
                            .await
                            .map_err(error)?
                            .and_then(|row| row.row.state.rating),
                    };
                    rating.and_then(|value| u8::try_from(value).ok())
                };
                Ok(value)
            })
            .await
            .map_err(error)?
    }

    pub async fn import_playlist(
        &self,
        uri: String,
    ) -> Result<AndroidPlaylistImport, AndroidError> {
        let current = self
            .source
            .selected_library()
            .as_ref()
            .and_then(|selected| self.source.configuration(&selected.source_id));
        let imported = rufin_core::playlists::import_document_playlist(&self.source, uri, current)
            .recv()
            .await
            .map_err(error)?
            .map_err(error)?;
        let row = self
            .catalog
            .playlists(&[imported.playlist], &ReadCancellation::new())
            .await
            .map_err(error)?
            .pop()
            .ok_or_else(|| error("Playlist no longer exists"))?;
        Ok(AndroidPlaylistImport {
            row: Row::Playlist(library::PlaylistListRow::Playlist(row)).into(),
            skipped: imported.skipped,
        })
    }

    pub fn set_pin(&self, pin: SidebarPin, pinned: bool) -> Result<bool, AndroidError> {
        self.settings.set_pinned(pin, pinned).map_err(error)
    }

    /// Whether a committed change can change the pins.
    pub fn pins_refresh(&self, changed: Changed) -> bool {
        rufin_core::view::pins_refresh(&self.source, &changed)
    }

    /// Whether a committed change means a search reads again.
    pub fn search_refresh(&self, mode: library::SearchMode, changed: Changed) -> bool {
        rufin_core::view::refresh(
            &self.source,
            &Route::Search,
            None,
            &OpenOptions {
                search_mode: mode,
                ..Default::default()
            },
            &changed,
        ) != rufin_core::view::Refresh::Nothing
    }

    pub async fn pins(&self, offset: u64, limit: u32) -> Result<AndroidPinPage, AndroidError> {
        let skip = usize::try_from(offset).map_err(error)?;
        let (total, pins) = {
            let changes = self.settings.changes();
            let stored = changes.borrow();
            let sidebar = &stored.ui.sidebar;
            if !sidebar.pins_visible {
                return Ok(AndroidPinPage {
                    rows: Vec::new(),
                    next_offset: None,
                    total: 0,
                });
            }
            let pins = sidebar
                .pins
                .iter()
                .skip(skip)
                .take(limit.min(256) as usize)
                .cloned()
                .collect::<Vec<_>>();
            (sidebar.pins.len() as u64, pins)
        };
        let consumed = offset.saturating_add(pins.len() as u64);
        let next_offset = (consumed < total).then_some(consumed);
        let catalog = self.catalog.clone();
        self.runtime
            .spawn(async move {
                let cancel = ReadCancellation::new();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(error)?
                    .as_secs() as i64;
                let rows = catalog
                    .pin_rows(&pins, now, &cancel)
                    .await
                    .map_err(error)?
                    .into_iter()
                    .map(|(pin, row)| AndroidBrowseRow {
                        pin: Some(pin),
                        ..row.into()
                    })
                    .collect();
                Ok(AndroidPinPage {
                    rows,
                    next_offset,
                    total,
                })
            })
            .await
            .map_err(error)?
    }
}

#[uniffi::export]
impl AndroidLibrary {
    /// The tracks `route` lists, for a download or a playlist: an artist page lists the
    /// artist's tracks.
    pub async fn route_selection(
        &self,
        route: Route,
    ) -> Result<Arc<MemberSelection>, AndroidError> {
        let route = match route {
            Route::ArtistDetail(uri) | Route::ArtistDiscography(uri) => Route::ArtistTracks(uri),
            Route::AlbumArtistDetail(uri) | Route::AlbumArtistDiscography(uri) => {
                Route::AlbumArtistTracks(uri)
            }
            route => route,
        };
        let source = self.source.clone();
        let selected = source.selected_library();
        run_read(&self.runtime, move |cancel| async move {
            source
                .selection(&route, selected.as_ref(), &OpenOptions::default(), &cancel)
                .await
                .map_err(error)?
                .map(Arc::new)
                .ok_or_else(|| error("Choose a collection"))
        })
        .await
    }
}
