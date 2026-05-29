use super::*;

pub(in crate::ui) struct TrackTablePopoverTarget<'a> {
    pub(in crate::ui) table: &'a gtk::ColumnView,
    pub(in crate::ui) model: &'a gio::ListStore,
    pub(in crate::ui) tracks: Rc<RefCell<Vec<Track>>>,
    pub(in crate::ui) search: &'a gtk::SearchEntry,
    pub(in crate::ui) sort_button: &'a gtk::Button,
    pub(in crate::ui) favorite_first: bool,
    pub(in crate::ui) server_search: bool,
}

impl Shell {
    pub(in crate::ui) fn track_table_popover(
        self: &Rc<Self>,
        target: TrackTablePopoverTarget<'_>,
    ) -> gtk::Popover {
        let TrackTablePopoverTarget {
            table,
            model,
            tracks,
            search,
            sort_button,
            favorite_first,
            server_search,
        } = target;
        let popover = gtk::Popover::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);

        let sort_label = gtk::Label::new(Some(&tr("Sort by")));
        sort_label.add_css_class("muted");
        sort_label.set_xalign(0.0);
        content.append(&sort_label);

        let sort_titles = TrackSortKey::all()
            .iter()
            .map(|key| tr(key.title()))
            .collect::<Vec<_>>();
        let sort_title_refs = sort_titles.iter().map(String::as_str).collect::<Vec<_>>();
        let sort_options = gtk::StringList::new(&sort_title_refs);
        let sort_dropdown = gtk::DropDown::new(Some(sort_options), None::<gtk::Expression>);
        let current_sort = self.state.settings.borrow().track_table.sort_key;
        sort_dropdown.set_selected(track_sort_index(current_sort));
        let shell = Rc::clone(self);
        let model_for_sort = model.clone();
        let tracks_for_sort = Rc::clone(&tracks);
        let search_for_sort = search.clone();
        let sort_button_for_sort = sort_button.clone();
        let popover_for_sort = popover.clone();
        sort_dropdown.connect_selected_notify(move |dropdown| {
            let sort_key = track_sort_from_index(dropdown.selected());
            let mut settings = shell.state.settings.borrow().track_table.clone();
            if settings.sort_key == sort_key {
                return;
            }
            settings.sort_key = sort_key;
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
                favorite_first,
            );
            set_track_sort_button_content(&sort_button_for_sort, &settings);
            let popover = popover_for_sort.clone();
            glib::idle_add_local_once(move || popover.popdown());
        });
        content.append(&sort_dropdown);

        let columns_label = gtk::Label::new(Some(&tr("Columns")));
        columns_label.add_css_class("muted");
        columns_label.set_xalign(0.0);
        content.append(&columns_label);

        let visible = self
            .state
            .settings
            .borrow()
            .track_table
            .visible_columns
            .clone();
        let column_checks = Rc::new(RefCell::new(Vec::new()));
        let syncing_column_checks = Rc::new(Cell::new(false));
        for column in TrackTableColumn::all() {
            let check = gtk::CheckButton::with_label(&tr(track_table_column_config_title(column)));
            check.set_active(visible.contains(&column));
            column_checks.borrow_mut().push((column, check.clone()));
            let shell = Rc::clone(self);
            let table_for_column = table.clone();
            let column_checks_for_column = Rc::clone(&column_checks);
            let syncing_column_checks_for_column = Rc::clone(&syncing_column_checks);
            check.connect_toggled(move |check| {
                if syncing_column_checks_for_column.get() {
                    return;
                }
                shell.update_track_table_settings(|settings| {
                    if check.is_active() {
                        if !settings.visible_columns.contains(&column) {
                            settings.visible_columns.push(column);
                        }
                    } else {
                        settings.visible_columns.retain(|stored| *stored != column);
                        if settings.visible_columns.is_empty() {
                            settings.visible_columns.push(TrackTableColumn::Title);
                        }
                    }
                });
                let settings = shell.state.settings.borrow().track_table.clone();
                sync_track_column_checks(
                    &column_checks_for_column,
                    &settings,
                    &syncing_column_checks_for_column,
                );
                set_track_table_columns(&shell, &table_for_column, &settings);
            });
            content.append(&check);
        }

        popover.set_child(Some(&content));
        popover
    }
}
