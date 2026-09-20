use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib, subclass::prelude::ObjectSubclassIsExt};
use rufin_core::settings::app::{RecentSearchKind, RecentSearchResult};
use ui_shared::artwork::ArtworkTile;
use ui_shared::route::{CollectionCategory, Route};

use super::Shell;

const SEARCH_COVER_SIZE: i32 = 48;

ui_shared::composite_box!(
    pub SearchPreviewRow,
    search_preview_row_imp,
    "RufinSearchPreviewRow",
    "/io/github/screwys/Rufin/ui/shell/search_row.ui",
    { cover_host: gtk::Box, title: gtk::Label, subtitle: gtk::Label, separator: gtk::Label, kind: gtk::Stack, remove: gtk::Button, play: gtk::Button, menu: gtk::Button }
);

struct PreviewRow {
    row: gtk::ListBoxRow,
    body: SearchPreviewRow,
    cover: ArtworkTile,
}

impl PreviewRow {
    fn new(history: bool) -> Self {
        let body = SearchPreviewRow::new();
        let cover = ArtworkTile::new(SEARCH_COVER_SIZE);
        body.imp().cover_host.append(&cover.widget());
        let row = gtk::ListBoxRow::builder()
            .child(&body)
            .focusable(false)
            .build();
        body.imp().kind.set_visible(history);
        body.imp().menu.set_visible(!history);
        body.imp().remove.set_visible(history);
        row.add_css_class("search-result");
        row.add_css_class(ui_shared::interactions::CONTEXT_MENU_HOVER_OWNER_CLASS);
        Self { row, body, cover }
    }
}

#[derive(Clone)]
enum SearchAction {
    Recent(RecentSearchResult),
    Result(CollectionCategory, usize),
    Category(CollectionCategory),
}

pub(super) struct SearchPopup {
    shell: Weak<Shell>,
    entry: gtk::SearchEntry,
    host: gtk::Overlay,
    session: Rc<ui_library::SearchSession>,
    popover: gtk::Popover,
    scroller: gtk::ScrolledWindow,
    list: gtk::ListBox,
    error: gtk::Label,
    empty_results: gtk::Label,
    history_heading: gtk::ListBoxRow,
    history_footer: gtk::ListBoxRow,
    headings: [gtk::ListBoxRow; 3],
    more: [gtk::Button; 3],
    history: Vec<PreviewRow>,
    previews: [Vec<PreviewRow>; 3],
    actions: RefCell<Vec<(gtk::ListBoxRow, SearchAction)>>,
    observer: RefCell<Option<Rc<dyn Fn()>>>,
    entry_handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl SearchPopup {
    pub fn new(shell: &Rc<Shell>, host: &gtk::Overlay) -> Rc<Self> {
        let resource = crate::ui_resource::SEARCH_POPOVER_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            popover: gtk::Popover, scroller: gtk::ScrolledWindow, list: gtk::ListBox,
            error: gtk::Label,
            empty_results: gtk::Label, history_heading: gtk::ListBoxRow, history_footer: gtk::ListBoxRow,
            clear_history: gtk::Button,
            tracks_heading: gtk::ListBoxRow, albums_heading: gtk::ListBoxRow, artists_heading: gtk::ListBoxRow,
            tracks_more: gtk::Button, albums_more: gtk::Button, artists_more: gtk::Button,
        });
        popover.set_parent(host);
        let popup = Rc::new(Self {
            shell: Rc::downgrade(shell),
            entry: shell.chrome.topbar.search.clone(),
            host: host.clone(),
            session: Rc::clone(&shell.chrome.topbar.search_session),
            popover,
            scroller,
            list,
            error,
            empty_results,
            history_heading,
            history_footer,
            headings: [tracks_heading, albums_heading, artists_heading],
            more: [tracks_more, albums_more, artists_more],
            history: (0..20).map(|_| PreviewRow::new(true)).collect(),
            previews: std::array::from_fn(|_| {
                (0..ui_library::SEARCH_PREVIEW_LIMIT)
                    .map(|_| PreviewRow::new(false))
                    .collect()
            }),
            actions: RefCell::new(Vec::new()),
            observer: RefCell::new(None),
            entry_handlers: RefCell::new(Vec::new()),
        });
        popup.list.append(&popup.history_heading);
        for row in &popup.history {
            popup.list.append(&row.row);
        }
        popup.list.append(&popup.history_footer);
        for category in CollectionCategory::ALL {
            let index = category as usize;
            popup.list.append(&popup.headings[index]);
            for row in &popup.previews[index] {
                if category == CollectionCategory::Artists {
                    row.row.add_css_class("artist-search-result");
                }
                popup.list.append(&row.row);
            }
        }
        popup.connect(&clear_history);
        popup
    }

    fn connect(self: &Rc<Self>, clear: &gtk::Button) {
        for category in CollectionCategory::ALL {
            let index = category as usize;
            let weak = Rc::downgrade(self);
            self.more[index].connect_clicked(move |_| {
                if let Some(popup) = weak.upgrade() {
                    popup.activate(Some(SearchAction::Category(category)));
                }
            });
            for (position, row) in self.previews[index].iter().enumerate() {
                let weak = Rc::downgrade(self);
                row.body.imp().play.connect_clicked(move |_| {
                    if let Some(popup) = weak.upgrade() {
                        if let Some(catalog) = popup.catalog() {
                            popup.session.play(category, position, &catalog);
                        }
                    }
                });
                let weak = Rc::downgrade(self);
                row.body.imp().menu.connect_clicked(move |button| {
                    if let Some(popup) = weak.upgrade() {
                        if let Some(catalog) = popup.catalog() {
                            popup.session.present_context(
                                category,
                                position,
                                button.upcast_ref(),
                                &catalog,
                            );
                        }
                    }
                });
            }
        }
        self.entry
            .update_relation(&[gtk::accessible::Relation::Controls(&[self
                .list
                .upcast_ref()])]);
        let weak = Rc::downgrade(self);
        self.list.connect_row_selected(move |_, row| {
            if let Some(popup) = weak.upgrade() {
                if let Some(row) = row {
                    popup
                        .entry
                        .update_relation(&[gtk::accessible::Relation::ActiveDescendant(
                            row.upcast_ref(),
                        )]);
                } else {
                    popup
                        .entry
                        .reset_relation(gtk::AccessibleRelation::ActiveDescendant);
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.observer.replace(Some(self.session.observe(move || {
            if let Some(popup) = weak.upgrade().filter(|popup| popup.popover.is_visible()) {
                popup.render();
            }
        })));
        let weak = Rc::downgrade(self);
        self.entry_handlers
            .borrow_mut()
            .push(self.entry.connect_text_notify(move |entry| {
                if let Some(popup) = weak.upgrade() {
                    if entry.text().is_empty() {
                        popup.host.remove_css_class("has-query");
                    } else {
                        popup.host.add_css_class("has-query");
                    }
                    popup.session.set_query(&entry.text());
                    if popup.has_focus() && !popup.popover.is_visible() {
                        popup.open();
                    }
                }
            }));
        let weak = Rc::downgrade(self);
        self.entry_handlers
            .borrow_mut()
            .push(self.entry.connect_activate(move |_| {
                if let Some(popup) = weak.upgrade() {
                    let action = popup.list.selected_row().and_then(|row| popup.action(&row));
                    popup.activate(action);
                }
            }));
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_enter(move |_| {
            if let Some(popup) = weak.upgrade() {
                popup.open();
            }
        });
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(popup) = weak.upgrade() {
                popup.close();
            }
        });
        self.host.add_controller(focus);

        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_released(move |_, _, _, _| {
            if let Some(popup) = weak.upgrade() {
                if !popup.popover.is_visible() {
                    popup.open();
                }
            }
        });
        self.entry.add_controller(click);

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(popup) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            // A result's native options menu owns its own arrows and Escape key.
            if popup
                .shell
                .upgrade()
                .and_then(|shell| GtkWindowExt::focus(&shell.chrome.window))
                .is_some_and(|focus| focus.ancestor(gtk::PopoverMenu::static_type()).is_some())
            {
                return glib::Propagation::Proceed;
            }
            if modifiers.intersects(
                gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::ALT_MASK
                    | gdk::ModifierType::META_MASK,
            ) {
                return glib::Propagation::Proceed;
            }
            if key == gdk::Key::Escape && popup.popover.is_visible() {
                popup.close();
                popup.entry.grab_focus();
                return glib::Propagation::Stop;
            }
            if matches!(key, gdk::Key::Down | gdk::Key::Up) {
                popup.entry.grab_focus();
                if !popup.popover.is_visible() {
                    popup.open();
                }
                let rows: Vec<_> = popup
                    .actions
                    .borrow()
                    .iter()
                    .map(|(row, _)| row.clone())
                    .collect();
                let current = popup
                    .list
                    .selected_row()
                    .and_then(|selected| rows.iter().position(|row| row == &selected));
                let next = match (current, key == gdk::Key::Down) {
                    (None, true) => rows.first(),
                    (None, false) => rows.last(),
                    (Some(index), true) => rows.get(index + 1),
                    (Some(index), false) => index.checked_sub(1).and_then(|index| rows.get(index)),
                };
                popup.list.select_row(next);
                if let Some(row) = next {
                    if let Some(bounds) = row.compute_bounds(&popup.list) {
                        popup
                            .scroller
                            .vadjustment()
                            .clamp_page(bounds.y() as f64, (bounds.y() + bounds.height()) as f64);
                    }
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.host.add_controller(keys);
        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            if let Some(popup) = weak.upgrade() {
                popup.activate(popup.action(row));
            }
        });
        let weak = Rc::downgrade(self);
        clear.connect_clicked(move |_| {
            if let Some(popup) = weak.upgrade() {
                if let Some(shell) = popup.shell.upgrade() {
                    shell
                        .settings
                        .update_app_settings("clear recent searches", |settings| {
                            if settings.recent_search_results.is_empty() {
                                return false;
                            }
                            settings.recent_search_results.clear();
                            true
                        });
                    popup.render();
                }
            }
        });
        for row in &self.history {
            let weak = Rc::downgrade(self);
            let target = row.row.downgrade();
            row.body.imp().remove.connect_clicked(move |_| {
                if let (Some(popup), Some(row)) = (weak.upgrade(), target.upgrade()) {
                    let Some(SearchAction::Recent(result)) = popup.action(&row) else {
                        return;
                    };
                    if let Some(shell) = popup.shell.upgrade() {
                        shell
                            .settings
                            .update_app_settings("remove recent search", |settings| {
                                let previous = settings.recent_search_results.len();
                                settings.recent_search_results.retain(|recent| {
                                    recent.media_uri != result.media_uri
                                        || recent.kind != result.kind
                                });
                                previous != settings.recent_search_results.len()
                            });
                        popup.render();
                    }
                }
            });
            let weak = Rc::downgrade(self);
            let target = row.row.downgrade();
            row.body.imp().play.connect_clicked(move |_| {
                if let (Some(popup), Some(row)) = (weak.upgrade(), target.upgrade()) {
                    if let (Some(SearchAction::Recent(result)), Some(catalog)) =
                        (popup.action(&row), popup.catalog())
                    {
                        ui_library::SearchSession::play_recent(
                            &result,
                            &catalog,
                            playback::QueuePlacement::Now,
                        );
                        popup.render();
                    }
                }
            });
        }
        // An unmodal popover lets the entry keep receiving text. Dismiss outside clicks explicitly.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        click.set_button(0);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |gesture, _, x, y| {
            if let Some(popup) = weak.upgrade() {
                if gesture
                    .current_event()
                    .and_then(|event| event.surface())
                    .and_then(|surface| gtk::Native::for_surface(&surface))
                    .is_some_and(|native| native.is_ancestor(&popup.host))
                {
                    return;
                }
                if let Some(widget) = gesture.widget() {
                    let point = gtk::graphene::Point::new(x as f32, y as f32);
                    if !popup
                        .host
                        .compute_bounds(&widget)
                        .is_some_and(|bounds| bounds.contains_point(&point))
                    {
                        popup.close();
                    }
                }
            }
        });
        if let Some(shell) = self.shell.upgrade() {
            shell.chrome.window.add_controller(click);
        }
    }

    fn has_focus(&self) -> bool {
        self.entry.has_focus() || self.entry.focus_child().is_some()
    }

    pub fn open(&self) {
        if self.session.query().is_empty()
            && self.shell.upgrade().is_none_or(|shell| {
                shell
                    .settings
                    .current
                    .borrow()
                    .recent_search_results
                    .is_empty()
            })
        {
            return;
        }
        self.resize();
        if !self.popover.is_visible() {
            self.render();
            self.popover.popup();
            self.entry
                .update_state(&[gtk::accessible::State::Expanded(Some(true))]);
        }
    }

    pub fn present(&self) {
        if self.popover.is_visible() {
            self.resize();
            self.popover.present();
        }
    }

    fn resize(&self) {
        if let Some(shell) = self.shell.upgrade() {
            let width = (self.host.width() * 5 / 4)
                .max(420)
                .min((shell.chrome.window.width() - 24).max(1));
            self.popover.set_width_request(width);
            if let Some(anchor) = self.host.compute_bounds(&shell.chrome.window) {
                let player = &shell.player_ui.views.player_controls.root;
                let bottom = if player.is_visible() {
                    player
                        .compute_bounds(&shell.chrome.window)
                        .map(|bounds| bounds.y())
                } else {
                    None
                }
                .unwrap_or(shell.chrome.window.height() as f32);
                // Leave room for the popover padding and a gap above the player.
                let available = (bottom - anchor.y() - anchor.height() - 24.0) as i32;
                self.scroller.set_max_content_height(available.max(1));
            }
        }
    }

    fn close(&self) {
        if !self.popover.is_visible() {
            return;
        }
        self.popover.popdown();
        self.entry
            .update_state(&[gtk::accessible::State::Expanded(Some(false))]);
        self.list.unselect_all();
        for row in self.previews.iter().flatten().chain(&self.history) {
            row.cover.cancel_artwork_request();
        }
    }

    fn action(&self, row: &gtk::ListBoxRow) -> Option<SearchAction> {
        self.actions
            .borrow()
            .iter()
            .find(|(candidate, _)| candidate == row)
            .map(|(_, action)| action.clone())
    }

    fn render(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.actions.borrow_mut().clear();
        let query = self.session.query();
        let history = shell
            .settings
            .current
            .borrow()
            .recent_search_results
            .clone();
        if query.is_empty() && history.is_empty() {
            self.close();
            return;
        }
        let status = self.session.status();
        let loading = !query.is_empty() && status == "loading";
        self.list
            .update_state(&[gtk::accessible::State::Busy(loading)]);
        self.error
            .set_visible(!query.is_empty() && matches!(status, "error" | "initial"));
        self.empty_results.set_visible(false);
        self.history_heading
            .set_visible(query.is_empty() && !history.is_empty());
        self.history_footer
            .set_visible(query.is_empty() && !history.is_empty());
        for (index, row) in self.history.iter().enumerate() {
            let recent = history.get(index).filter(|_| query.is_empty());
            row.row.set_visible(recent.is_some());
            if let Some(result) = recent {
                row.body.imp().title.set_text(&result.title);
                row.body.imp().subtitle.set_text(&result.subtitle);
                row.body
                    .imp()
                    .separator
                    .set_visible(!result.subtitle.is_empty());
                row.body
                    .imp()
                    .subtitle
                    .set_visible(!result.subtitle.is_empty());
                row.body
                    .imp()
                    .kind
                    .set_visible_child_name(match result.kind {
                        RecentSearchKind::Track => "track",
                        RecentSearchKind::Album => "album",
                        RecentSearchKind::Artist => "artist",
                    });
                if result.kind == RecentSearchKind::Artist {
                    row.row.add_css_class("artist-search-result");
                } else {
                    row.row.remove_css_class("artist-search-result");
                }
                let artwork = result
                    .artwork_binding
                    .as_deref()
                    .map(artwork::ArtworkBinding::opaque)
                    .unwrap_or_default();
                shell.artwork.bind_artwork_tile(
                    &row.cover,
                    artwork,
                    SEARCH_COVER_SIZE,
                    SEARCH_COVER_SIZE as u32,
                );
                self.actions
                    .borrow_mut()
                    .push((row.row.clone(), SearchAction::Recent(result.clone())));
            } else {
                shell.artwork.clear_artwork_tile(&row.cover);
            }
        }
        let mut count = 0;
        let previews = self.session.previews();
        for category in CollectionCategory::ALL {
            let index = category as usize;
            let previews = &previews[index];
            self.headings[index].set_visible(!query.is_empty() && !previews.is_empty());
            for (position, row) in self.previews[index].iter().enumerate() {
                let preview = previews.get(position).filter(|_| !query.is_empty());
                let skeleton = loading && category == CollectionCategory::Tracks && position < 3;
                row.row.set_visible(preview.is_some() || skeleton);
                row.row.set_selectable(!skeleton);
                row.row.set_activatable(!skeleton);
                row.body.imp().play.set_visible(!skeleton);
                row.body.imp().menu.set_visible(!skeleton);
                if skeleton {
                    row.body.add_css_class("search-preview-loading");
                    row.body.imp().title.set_text("");
                    row.body.imp().subtitle.set_text("");
                    row.body.imp().subtitle.set_visible(true);
                } else {
                    row.body.remove_css_class("search-preview-loading");
                }
                if let Some(preview) = preview {
                    row.body.imp().title.set_text(&preview.title);
                    row.body.imp().subtitle.set_text(&preview.subtitle);
                    row.body
                        .imp()
                        .subtitle
                        .set_visible(!preview.subtitle.is_empty());
                    shell.artwork.bind_artwork_tile(
                        &row.cover,
                        preview.artwork.clone(),
                        SEARCH_COVER_SIZE,
                        SEARCH_COVER_SIZE as u32,
                    );
                    self.actions
                        .borrow_mut()
                        .push((row.row.clone(), SearchAction::Result(category, position)));
                    count += 1;
                } else {
                    shell.artwork.clear_artwork_tile(&row.cover);
                }
            }
        }
        let empty = !query.is_empty() && status == "results" && count == 0;
        if empty {
            self.empty_results.set_text(&localization::tr_with(
                "No results for \"{query}\"",
                &[("query", &query)],
            ));
        }
        self.empty_results.set_visible(empty);
        self.scroller
            .set_visible(loading || count > 0 || (query.is_empty() && !history.is_empty()));
        self.list.unselect_all();
    }

    fn catalog(&self) -> Option<Rc<ui_library::CatalogUi>> {
        let shell = self.shell.upgrade()?;
        let route = shell.navigation.routes.borrow().current().clone();
        let source = shell
            .selected_library()
            .map(|selected| selected.source_id.clone());
        Some(shell.build_catalog(&route, source.as_ref()))
    }

    fn activate(&self, action: Option<SearchAction>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.close();
        match action {
            Some(SearchAction::Recent(result)) => {
                if let Some(catalog) = self.catalog() {
                    ui_library::SearchSession::activate_recent(&result, &catalog);
                }
            }
            Some(SearchAction::Result(category, index)) => {
                if let Some(catalog) = self.catalog() {
                    self.session.activate(category, index, &catalog);
                }
            }
            other => {
                if let Some(SearchAction::Category(category)) = other {
                    self.session.set_category(category);
                }
                self.session.submit();
                if shell.navigation.routes.borrow().current() != &Route::Search {
                    shell.navigate(Route::Search);
                }
            }
        }
    }
}

impl Drop for SearchPopup {
    fn drop(&mut self) {
        for handler in self.entry_handlers.get_mut().drain(..) {
            self.entry.disconnect(handler);
        }
        self.popover.unparent();
    }
}
