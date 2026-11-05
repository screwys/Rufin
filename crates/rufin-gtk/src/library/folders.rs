use std::{cell::RefCell, rc::Rc};

use library::Row;

use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use localization::{msgid, tr};
use rufin_core::playback::QueuePlacement;

use crate::library::{CatalogUi, LibraryField, LibraryListKey};
use crate::widgets::interactions::install_context_menu_openers;
use crate::widgets::mounted_route::MountedRoute;
use rufin_core::view::Opened;

use crate::widgets::view_model::{Rows, connect_row_bind};
use rufin_core::route::{FolderPathItem, Route};

type FolderResume = Rc<dyn Fn()>;

crate::composite_box!(
    pub FoldersRouteView,
    folders_route_view_imp,
    "RufinFoldersRouteView",
    "/io/github/screwys/Rufin/ui/routes/folders.ui",
    {
        toolbar_host: gtk::Box,
        breadcrumb_host: gtk::Box,
    }
);

fn folder_table_initial_width(route_width: i32) -> i32 {
    route_width
        .saturating_sub(crate::library::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET)
        .max(1)
}

impl CatalogUi {
    pub fn folders_route(self: &Rc<Self>, path: Vec<FolderPathItem>, read: Opened) -> MountedRoute {
        let wrapper = FoldersRouteView::new();
        let search = gtk::SearchEntry::new();
        crate::widgets::controls::configure_search_entry(&search);
        search.set_placeholder_text(Some(&tr("Search current folder")));
        let toolbar = self.library_toolbar_projection_without_detail(
            LibraryListKey::Folders,
            Some(search.clone()),
            crate::library::available_sort_fields(LibraryListKey::Folders),
        );
        toolbar.set_layout_control_visible(false);
        wrapper.imp().toolbar_host.append(&toolbar.widget());
        wrapper
            .imp()
            .breadcrumb_host
            .append(&folder_path_switcher(self, &path));
        let (content, rows, resume, refresh) = folder_page(self, path, search.clone(), read);
        wrapper.append(&content);
        let shell = Rc::downgrade(self);
        let resume = Rc::new(move || {
            let Some(shell) = shell.upgrade() else { return };
            toolbar.apply(
                LibraryListKey::Folders,
                &shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Folders),
            );
            resume();
        });
        MountedRoute::new(wrapper.upcast(), resume)
            .with_catalog_refresh(refresh)
            .with_search(search)
            .with_initial_demand({
                let rows = rows.clone();
                Rc::new(move || rows.resume_initial_demand())
            })
            .with_rows(&rows)
    }
}

fn folder_page(
    shell: &Rc<CatalogUi>,
    path: Vec<FolderPathItem>,
    search: gtk::SearchEntry,
    mut read: Opened,
) -> (gtk::Widget, Rows<Row>, FolderResume, FolderResume) {
    search.set_visible(true);
    let route_width = shell.route_width();
    // Child folders, then the folder's tracks, as the View lists them.
    let rows = Rows::over_view(shell.products.runtime.clone(), &mut read);
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::Folders);
    let table_initial_width = folder_table_initial_width(route_width);
    let table = folder_table(
        shell,
        rows.clone(),
        path.clone(),
        table_initial_width,
        &settings.row_fields,
    );

    let table_scroller = gtk::ScrolledWindow::new();
    crate::widgets::layout::configure_fill_width_clip(&table_scroller, gtk::PolicyType::Automatic);
    table_scroller.set_hexpand(true);
    table_scroller.set_vexpand(true);
    table_scroller.set_child(Some(&table.widget()));
    let table_view = crate::library::route_layout::route_scroller_widget(table_scroller.clone());
    let resize_table_scroller = table_scroller.clone();
    let resize_table = table.clone();
    let table_view = crate::widgets::layout::width_allocation_owner(&table_view, move |width| {
        resize_table.fit_scroller_allocation(&resize_table_scroller, width);
    })
    .upcast::<gtk::Widget>();
    let table_stack = gtk::Stack::new();
    table_stack.set_hexpand(true);
    table_stack.set_vexpand(true);
    table_stack.add_named(&table_view, Some("content"));
    table_stack.add_named(&folder_empty_view(shell), Some("empty"));
    table_stack.set_visible_child_name(if rows.is_empty() { "empty" } else { "content" });

    let apply = {
        let stack = table_stack.clone();
        let rows = rows.clone();
        move |_| {
            stack.set_visible_child_name(if rows.is_empty() { "empty" } else { "content" });
        }
    };
    let reads = crate::library::routes::PageReads::new(
        &rows,
        crate::library::routes::list_inputs(&search),
        apply,
    );
    crate::library::routes::connect_list_search(&search, &reads);
    let resume = {
        let shell = Rc::downgrade(shell);
        let rows = rows.clone();
        let reads = reads.clone();
        let previous_fields = RefCell::new(settings.row_fields);
        Rc::new(move || {
            let Some(shell) = shell.upgrade() else { return };
            let settings = shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::Folders);
            let name_changed = folder_name_field(&previous_fields.borrow())
                != folder_name_field(&settings.row_fields);
            table.apply_fields(&settings.row_fields);
            crate::widgets::display::apply(&table.widget(), &settings.display);
            if name_changed {
                rows.update(|row| matches!(row, Row::Folder(_)), |_| {});
            }
            previous_fields.replace(settings.row_fields);
            reads.resume(&shell.settings.current.borrow());
        }) as FolderResume
    };
    let refresh = Rc::new(move || reads.refresh()) as FolderResume;
    (table_stack.upcast(), rows, resume, refresh)
}

fn folder_table(
    shell: &Rc<CatalogUi>,
    rows: Rows<Row>,
    path: Vec<FolderPathItem>,
    initial_width: i32,
    fields: &[LibraryField],
) -> crate::library::collections::CollectionTableProjection {
    let selection = gtk::SingleSelection::new(Some(rows.list_model()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let playing = crate::widgets::recycled_cells::TrackRowPlayingIndicator::new();
    let current_playing = playing.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        current_playing.set_current(
            current.map(|current| current.media_uri.as_str()),
            gtk::INVALID_LIST_POSITION,
        );
        current_playing.set_paused(current.is_some_and(|current| current.paused));
        true
    }));
    let column_shell = Rc::clone(shell);
    let column_path = path.clone();
    let column_rows = rows.clone();
    let activate_shell = Rc::clone(shell);
    let activate_rows = rows.clone();
    let activate = move |position: u32, row: Row| match row {
        Row::Folder(folder) => {
            let mut next = path.clone();
            next.push(FolderPathItem {
                id: folder.object_id.clone(),
                name: folder.name.clone(),
            });
            activate_shell.open_route(Route::Folders { path: next });
        }
        Row::Track(_) => activate_rows.play(
            &activate_shell.products.playback,
            position as usize,
            QueuePlacement::Now,
            false,
        ),
        _ => {}
    };
    let table = crate::library::collections::dynamic_collection_table(
        shell,
        LibraryListKey::Folders,
        rows.list_model(),
        fields,
        Vec::new(),
        move |field| {
            folder_column(
                &column_shell,
                column_path.clone(),
                column_rows.clone(),
                &playing,
                field,
            )
        },
        |field| super::columns::track_column_width(LibraryListKey::Folders, field),
        false,
        Some(Box::new(activate)),
        Some(selection.upcast()),
        initial_width,
    );
    let widget = table.widget();
    widget.add_css_class("folder-table");
    widget.add_css_class("folders-table");
    widget.add_css_class("data-table");
    table
}

fn folder_index_column(
    rows: Rows<Row>,
    playing: crate::widgets::recycled_cells::TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            item.set_child(Some(
                &crate::widgets::recycled_cells::track_list_row_index_cell(item),
            ));
        }
    });
    let bind_playing = playing.clone();
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(cell) = item
            .child()
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        else {
            return;
        };
        let text = match crate::widgets::view_model::item_at_from_item::<Row>(item) {
            Some(Row::Track(track)) => {
                bind_playing.bind(cell.upcast_ref(), item.position(), &track.media_uri);
                item.position()
                    .saturating_sub(
                        rows.view().and_then(|view| view.folder_len()).unwrap_or(0) as u32
                    )
                    .saturating_add(1)
                    .to_string()
            }
            _ => {
                bind_playing.unbind(cell.upcast_ref());
                String::new()
            }
        };
        crate::widgets::recycled_cells::set_track_row_index_text(&cell, &text);
    });
    factory.connect_unbind(move |_, item| {
        if let Some(cell) = item
            .downcast_ref::<gtk::ListItem>()
            .and_then(gtk::ListItem::child)
            .and_then(|child| child.downcast::<gtk::Overlay>().ok())
        {
            crate::widgets::recycled_cells::set_track_row_index_text(&cell, "");
            playing.unbind(cell.upcast_ref());
        }
    });
    let column = gtk::ColumnViewColumn::new(Some("#"), Some(factory));
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Folders, LibraryField::RowIndex),
    );
    column
}

fn folder_merged_column(shell: &Rc<CatalogUi>, path: Vec<FolderPathItem>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = crate::widgets::recycled_cells::RecycledMergedCell::new(
            setup_shell.route_navigation(),
            &setup_shell.downloads,
            48,
            true,
        );
        cell.subtitle().add_css_class("artist-label");
        cell.subtitle().add_css_class("table-link-label");
        let icon = gtk::Image::from_icon_name("rufin-folders-symbolic");
        icon.set_pixel_size(20);
        icon.set_size_request(32, 32);
        icon.set_valign(gtk::Align::Center);
        icon.set_visible(false);
        cell.prepend(&icon);
        let weak = item.downgrade();
        let shell = Rc::downgrade(&setup_shell);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let (Some(item), Some(shell)) = (weak.upgrade(), shell.upgrade()) else {
                    return;
                };
                let Some(Row::Track(track)) =
                    crate::widgets::view_model::item_at_from_item::<Row>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target,
                    &shell,
                    track.media_uri,
                    position,
                );
            }),
        );
        install_folder_cell_activation(&cell, item, &setup_shell, path.clone());
        item.set_child(Some(&cell));
    });
    let bind_shell = Rc::clone(shell);
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = crate::widgets::view_model::item_at_from_item::<Row>(item) else {
            return;
        };
        let Some(cell) = crate::widgets::recycled_cells::list_cell::<
            crate::widgets::recycled_cells::RecycledMergedCell,
        >(item) else {
            return;
        };
        let Some(icon) = cell
            .first_child()
            .and_then(|child| child.downcast::<gtk::Image>().ok())
        else {
            return;
        };
        let cover = cell.cover();
        match row {
            Row::Folder(folder) => {
                bind_shell.artwork.clear_artwork_tile(&cover);
                cover.widget().set_visible(false);
                icon.set_visible(true);
                cell.title().set_text(&folder.name);
                cell.clear_subtitle();
                if let Some(downloaded) = cell.downloaded() {
                    downloaded.set_visible(false);
                }
            }
            Row::Track(track) => {
                icon.set_visible(false);
                cover.widget().set_visible(true);
                bind_shell.artwork.bind_artwork_tile(
                    &cover,
                    crate::widgets::library_fields::opaque_artwork(
                        track.artwork_binding.as_deref(),
                    ),
                    48,
                );
                cell.title().set_text(&track.title);
                cell.bind_subtitle(crate::widgets::library_fields::TrackPresentation::links(
                    &track,
                    LibraryField::Artist,
                ));
                cell.subtitle().set_visible(true);
                if let Some(downloaded) = cell.downloaded() {
                    bind_shell.downloads.bind_download_badge(
                        &downloaded,
                        crate::downloads::media_download_badge(
                            &track.media_uri,
                            track.is_downloaded,
                        ),
                    );
                }
            }
            _ => {}
        }
    });
    let clear_shell = Rc::clone(shell);
    factory.connect_unbind(move |_, item| {
        if let Some(cell) = item.downcast_ref::<gtk::ListItem>().and_then(
            crate::widgets::recycled_cells::list_cell::<
                crate::widgets::recycled_cells::RecycledMergedCell,
            >,
        ) {
            clear_shell.artwork.clear_artwork_tile(&cell.cover());
            cell.cover().widget().set_visible(true);
            cell.title().set_text("");
            cell.clear_subtitle();
            if let Some(downloaded) = cell.downloaded() {
                downloaded.set_visible(false);
            }
        }
    });
    let column = gtk::ColumnViewColumn::new(
        Some(&tr(
            rufin_core::settings::presentation::library_field_title(LibraryField::Title),
        )),
        Some(factory),
    );
    configure_folder_column(
        &column,
        super::columns::track_column_width(LibraryListKey::Folders, LibraryField::TitleMerged),
    );
    column
}

fn folder_column(
    shell: &Rc<CatalogUi>,
    path: Vec<FolderPathItem>,
    rows: Rows<Row>,
    playing: &crate::widgets::recycled_cells::TrackRowPlayingIndicator,
    field: LibraryField,
) -> gtk::ColumnViewColumn {
    use crate::widgets::library_fields::TrackPresentation;
    let width = super::columns::track_column_width(LibraryListKey::Folders, field);
    match field {
        LibraryField::RowIndex => return folder_index_column(rows, playing.clone()),
        LibraryField::TitleMerged => return folder_merged_column(shell, path),
        LibraryField::Image => {
            return super::columns::mapped_track_image_column::<Row, _, _>(
                shell,
                rufin_core::settings::presentation::library_field_title(field),
                width,
                |row| match row {
                    Row::Track(track) => Some(track.media_uri.clone()),
                    _ => None,
                },
                |row| match row {
                    Row::Track(track) => crate::widgets::library_fields::opaque_artwork(
                        track.artwork_binding.as_deref(),
                    ),
                    _ => Default::default(),
                },
            );
        }
        LibraryField::Favorite | LibraryField::Tools => {
            return super::columns::mapped_track_actions_column::<Row, _, _>(
                shell,
                field,
                |row| match row {
                    Row::Track(track) => Some(track.media_uri.clone()),
                    _ => None,
                },
                |row| match row {
                    Row::Track(track) => Some((track.media_uri.clone(), track.state.favorite)),
                    _ => None,
                },
            );
        }
        _ => {}
    }
    let value_shell = Rc::clone(shell);
    let column = folder_text_column(
        shell,
        path,
        super::columns::track_column_title(field),
        24,
        !matches!(
            field,
            LibraryField::Title
                | LibraryField::Artist
                | LibraryField::AlbumArtist
                | LibraryField::Album
                | LibraryField::Genre
        ),
        move |row| match row {
            Row::Folder(folder) => {
                let fields = value_shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::Folders)
                    .row_fields;
                if folder_name_field(&fields) == Some(field) {
                    folder.name.clone()
                } else {
                    String::new()
                }
            }
            Row::Track(track) => track.field(field),
            _ => String::new(),
        },
        move |row| match row {
            Row::Track(track) => Some(track.links(field)),
            _ => None,
        },
    );
    configure_folder_column(&column, width);
    column
}

fn folder_name_field(fields: &[LibraryField]) -> Option<LibraryField> {
    [LibraryField::TitleMerged, LibraryField::Title]
        .into_iter()
        .find(|field| fields.contains(field))
        .or_else(|| {
            fields.iter().copied().find(|field| {
                !matches!(
                    field,
                    LibraryField::RowIndex
                        | LibraryField::Image
                        | LibraryField::Favorite
                        | LibraryField::Tools
                )
            })
        })
}

fn folder_text_column(
    shell: &Rc<CatalogUi>,
    path: Vec<FolderPathItem>,
    title: &str,
    max_width_chars: i32,
    tabular_numeric: bool,
    value: impl Fn(&Row) -> String + 'static,
    links: impl Fn(&Row) -> Option<rufin_core::route::detail_links::DetailLinks> + 'static,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let setup_shell = Rc::clone(shell);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let cell = crate::widgets::recycled_cells::RecycledTextCell::new();
        cell.enable_links(setup_shell.route_navigation());
        let label = cell.label();
        if tabular_numeric {
            label.add_css_class("tabular-numeric");
        }
        label.set_xalign(0.0);
        label.set_width_request(1);
        label.set_max_width_chars(max_width_chars);
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Fill);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let weak = item.downgrade();
        let shell = Rc::clone(&setup_shell);
        install_context_menu_openers(
            &cell,
            Rc::new(move |target, position| {
                let Some(item) = weak.upgrade() else { return };
                let Some(Row::Track(track)) =
                    crate::widgets::view_model::item_at_from_item::<Row>(&item)
                else {
                    return;
                };
                super::collection_context::present_track_context_menu(
                    target,
                    &shell,
                    track.media_uri,
                    position,
                );
            }),
        );
        install_folder_cell_activation(&cell, item, &setup_shell, path.clone());
        item.set_child(Some(&cell));
    });
    connect_row_bind(&factory, move |item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = crate::widgets::view_model::item_at_from_item::<Row>(item) else {
            return;
        };
        let Some(cell) = crate::widgets::recycled_cells::list_cell::<
            crate::widgets::recycled_cells::RecycledTextCell,
        >(item) else {
            return;
        };
        let text = value(&row);
        cell.bind_links(
            links(&row)
                .unwrap_or_else(|| rufin_core::route::detail_links::DetailLinks::text(&text)),
        );
    });
    factory.connect_unbind(|_, item| {
        let Some(cell) = item.downcast_ref::<gtk::ListItem>().and_then(
            crate::widgets::recycled_cells::list_cell::<
                crate::widgets::recycled_cells::RecycledTextCell,
            >,
        ) else {
            return;
        };
        cell.clear();
    });
    gtk::ColumnViewColumn::new(Some(&tr(title)), Some(factory))
}

fn install_folder_cell_activation(
    target: &impl IsA<gtk::Widget>,
    item: &gtk::ListItem,
    shell: &Rc<CatalogUi>,
    path: Vec<FolderPathItem>,
) {
    let item = item.downgrade();
    let shell = Rc::clone(shell);
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    click.connect_released(move |_, presses, _, _| {
        if presses != 1 {
            return;
        }
        let Some(item) = item.upgrade() else { return };
        let Some(Row::Folder(folder)) = crate::widgets::view_model::item_at_from_item::<Row>(&item)
        else {
            return;
        };
        let mut next = path.clone();
        next.push(FolderPathItem {
            id: folder.object_id,
            name: folder.name,
        });
        shell.open_route(Route::Folders { path: next });
    });
    target.add_controller(click);
}

fn configure_folder_column(column: &gtk::ColumnViewColumn, width: i32) {
    column.set_fixed_width(width);
    column.set_resizable(false);
}

fn folder_path_switcher(shell: &Rc<CatalogUi>, path: &[FolderPathItem]) -> gtk::Box {
    let switcher = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    switcher.add_css_class("folder-path-switcher");
    switcher.set_hexpand(true);
    switcher.set_halign(gtk::Align::Fill);
    switcher.set_width_request(1);
    let root = folder_path_button(shell, &tr("Folders"), Vec::new(), path.is_empty(), None);
    switcher.append(&root);
    for (index, entry) in path.iter().enumerate() {
        let destination = path.iter().take(index + 1).cloned().collect::<Vec<_>>();
        let separator = gtk::Image::from_icon_name("rufin-go-next-symbolic");
        separator.add_css_class("folder-path-separator");
        separator.set_pixel_size(16);
        separator.set_valign(gtk::Align::Center);
        separator.set_can_target(false);
        switcher.append(&separator);
        switcher.append(&folder_path_button(
            shell,
            &entry.name,
            destination,
            index + 1 == path.len(),
            Some(&root),
        ));
    }
    switcher
}

fn folder_path_button(
    shell: &Rc<CatalogUi>,
    text: &str,
    path: Vec<FolderPathItem>,
    current: bool,
    group: Option<&gtk::ToggleButton>,
) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::new();
    button.add_css_class("flat");
    button.add_css_class("folder-path-segment");
    if let Some(group) = group {
        button.set_group(Some(group));
    }
    button.set_active(current);
    button.set_width_request(1);
    button.set_can_target(!current);
    button.set_focusable(!current);
    let label = gtk::Label::new(Some(text));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    button.set_child(Some(&label));
    button.set_tooltip_text(Some(text));
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| shell.open_route(Route::Folders { path: path.clone() }));
    button
}

fn folder_empty_view(shell: &Rc<CatalogUi>) -> gtk::Widget {
    let view = gtk::Box::new(gtk::Orientation::Vertical, 12);
    view.add_css_class("empty-state");
    view.set_vexpand(true);
    view.set_hexpand(true);
    view.set_valign(gtk::Align::Center);
    view.set_halign(gtk::Align::Center);
    let label = gtk::Label::new(Some(&tr(msgid("Nothing in this folder"))));
    label.add_css_class("muted");
    view.append(&label);
    let refresh = gtk::Button::with_label(&tr(msgid("Refresh")));
    refresh.add_css_class("pill");
    refresh.set_halign(gtk::Align::Center);
    let shell = Rc::clone(shell);
    refresh.connect_clicked(move |_| shell.replace_current_route_when_ready());
    view.append(&refresh);
    view.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_table_uses_the_complete_route_width() {
        assert_eq!(folder_table_initial_width(900), 880);
        assert_eq!(folder_table_initial_width(8), 1);
    }
}
