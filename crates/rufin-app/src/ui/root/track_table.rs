use super::track_table_popover;
use super::*;

impl Shell {
    pub(in crate::ui) fn compact_artist_tracks_table(
        self: &Rc<Self>,
        tracks: Vec<Track>,
        context: &str,
    ) -> gtk::Widget {
        self.tracks_table_with_options(
            tracks,
            context,
            TrackTableOptions {
                paging: None,
                expand: false,
                max_visible_rows: Some(5),
                favorite_first: true,
            },
        )
    }
    pub(in crate::ui) fn tracks_table_with_options(
        self: &Rc<Self>,
        tracks: Vec<Track>,
        context: &str,
        options: TrackTableOptions,
    ) -> gtk::Widget {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 10);
        wrapper.set_vexpand(options.expand);
        let tracks = Rc::new(RefCell::new(tracks));
        let page_cursor = options.paging.map(|(offset, total)| {
            Rc::new(PagedGridCursor {
                offset: Cell::new(offset),
                total: Cell::new(total),
                loading: Cell::new(false),
            })
        });
        let server_search = page_cursor.is_some();
        let paged_query = Rc::new(RefCell::new(String::new()));

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        toolbar.add_css_class("track-toolbar");
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some(&tr("Search")));
        search.set_hexpand(true);
        toolbar.append(&search);

        let settings = self.state.settings.borrow().track_table.clone();
        let sort_button = gtk::Button::new();
        sort_button.add_css_class("flat");
        set_track_sort_button_content(&sort_button, &settings);
        toolbar.append(&sort_button);

        let configure = gtk::MenuButton::new();
        configure.add_css_class("flat");
        configure.set_icon_name("view-more-symbolic");
        configure.set_tooltip_text(Some(&tr("Configure columns")));
        toolbar.append(&configure);
        wrapper.append(&toolbar);

        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        populate_track_model_with_options(
            &model,
            &tracks.borrow(),
            &settings,
            "",
            options.favorite_first,
        );
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        let table = gtk::ColumnView::new(Some(selection));
        table.add_css_class("track-table");
        table.set_vexpand(options.expand);
        table.set_hexpand(true);
        table.set_single_click_activate(false);
        set_track_table_columns(self, &table, &settings);

        let controller = self.controller.clone();
        let model_for_activate = model.clone();
        table.connect_activate(move |_, position| {
            let Some(item) = model_for_activate.item(position) else {
                return;
            };
            let Ok(boxed) = item.downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            controller.play_now(boxed.borrow::<Track>().clone());
        });

        let model_for_search = model.clone();
        let tracks_for_search = Rc::clone(&tracks);
        let shell = Rc::clone(self);
        let page_cursor_for_search = page_cursor.clone();
        let paged_query_for_search = Rc::clone(&paged_query);
        search.connect_search_changed(move |entry| {
            let settings = shell.state.settings.borrow().track_table.clone();
            if let Some(cursor) = page_cursor_for_search.as_ref() {
                let query = entry.text().trim().to_string();
                *paged_query_for_search.borrow_mut() = query.clone();
                cursor.offset.set(0);
                cursor.total.set(usize::MAX);
                cursor.loading.set(true);
                match shell
                    .controller
                    .cached_tracks_page_matching(&query, 0, TRACK_ROUTE_PAGE_SIZE)
                {
                    Ok(page) => {
                        let count = page.items.len();
                        *tracks_for_search.borrow_mut() = page.items;
                        let tracks = tracks_for_search.borrow();
                        populate_track_model_with_options(
                            &model_for_search,
                            &tracks,
                            &settings,
                            "",
                            options.favorite_first,
                        );
                        finish_grid_page(cursor, 0, count, page.total);
                    }
                    Err(error) => {
                        warn!(%error, "failed to search cached tracks page");
                        cursor.loading.set(false);
                    }
                }
            } else {
                let tracks = tracks_for_search.borrow();
                populate_track_model_with_options(
                    &model_for_search,
                    &tracks,
                    &settings,
                    entry.text().as_str(),
                    options.favorite_first,
                );
            }
        });

        let model_for_sort = model.clone();
        let tracks_for_sort = Rc::clone(&tracks);
        let shell = Rc::clone(self);
        let search_for_sort = search.clone();
        sort_button.connect_clicked(move |button| {
            let mut settings = shell.state.settings.borrow().track_table.clone();
            settings.descending = !settings.descending;
            shell.update_track_table_settings(|stored| *stored = settings.clone());
            let tracks = tracks_for_sort.borrow();
            let search_text = search_for_sort.text();
            let query = if server_search {
                ""
            } else {
                search_text.as_str()
            };
            populate_track_model_with_options(
                &model_for_sort,
                &tracks,
                &settings,
                query,
                options.favorite_first,
            );
            set_track_sort_button_content(button, &settings);
        });

        configure.set_popover(Some(&self.track_table_popover(
            track_table_popover::TrackTablePopoverTarget {
                table: &table,
                model: &model,
                tracks: Rc::clone(&tracks),
                search: &search,
                sort_button: &sort_button,
                favorite_first: options.favorite_first,
                server_search,
            },
        )));

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroller.set_min_content_width(0);
        scroller.set_vexpand(options.expand);
        if let Some(max_visible_rows) = options.max_visible_rows {
            let visible_rows = tracks.borrow().len().min(max_visible_rows).max(1);
            let height = 92 + visible_rows as i32 * 58;
            scroller.set_min_content_height(height);
            scroller.set_max_content_height(height);
        }
        scroller.set_child(Some(&table));
        if let Some(cursor) = page_cursor {
            let shell = Rc::clone(self);
            let tracks_for_page = Rc::clone(&tracks);
            let model_for_page = model.clone();
            let paged_query_for_page = Rc::clone(&paged_query);
            let load_next = Rc::new(move || {
                if !shell.can_load_grid_page(&cursor, &Route::Tracks) {
                    return;
                }
                let offset = cursor.offset.get();
                let query = paged_query_for_page.borrow().clone();
                match shell.controller.cached_tracks_page_matching(
                    &query,
                    offset,
                    TRACK_ROUTE_PAGE_SIZE,
                ) {
                    Ok(page) => {
                        let count = page.items.len();
                        let mut items = page.items;
                        tracks_for_page.borrow_mut().extend(items.iter().cloned());
                        let settings = shell.state.settings.borrow().track_table.clone();
                        sort_tracks_with_options(&mut items, &settings, options.favorite_first);
                        append_tracks_to_model(&model_for_page, items);
                        finish_grid_page(&cursor, offset, count, page.total);
                    }
                    Err(error) => {
                        warn!(%error, "failed to append cached tracks page");
                        cursor.loading.set(false);
                    }
                }
            });
            connect_paged_grid_loader(&scroller, load_next);
        }
        wrapper.append(&scroller);
        wrapper.set_widget_name(context);
        wrapper.upcast()
    }
}
