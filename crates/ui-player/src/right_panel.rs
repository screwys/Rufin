use adw::prelude::*;
use localization::tr;
use rufin_core::settings::{
    Settings,
    right_panel::{RightPanelSettings, SidebarPanel},
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct PanelControls {
    root: gtk::Box,
    up: gtk::Button,
    down: gtk::Button,
}

impl PanelControls {
    fn new() -> Self {
        let resource = crate::ui_resource::SIDEBAR_CONTROLS_RESOURCE;
        let builder = ui_shared::ui_resource::builder(resource);
        ui_shared::objects!(builder, resource, {
            controls: gtk::Box,
            up: gtk::Button,
            down: gtk::Button,
        });
        Self {
            root: controls,
            up,
            down,
        }
    }
}

pub struct RightPanelWidgets {
    pub queue_loading: adw::Spinner,
    pub root: gtk::Box,
    pub queue_panel: gtk::Box,
    pub queue_search: gtk::SearchEntry,
    pub queue_clear_button: gtk::Button,
    pub queue_playlist_button: gtk::Button,
    pub queue_customize_button: gtk::Button,
    pub(crate) queue_width_fit: RefCell<Option<ui_shared::table_sizing::ColumnViewWidthFit>>,
    pub lyrics_host: gtk::Box,
    pub visualizer_settings: gtk::Button,
    pub visualizer_visible: Cell<bool>,
    visualizer_controls: gtk::Box,
    modules: [gtk::Overlay; 3],
    controls: [(SidebarPanel, PanelControls); 2],
    splits: RefCell<Vec<gtk::Paned>>,
    visible_panels: RefCell<Vec<SidebarPanel>>,
    combined: Cell<bool>,
}

pub fn build_right_panel(
    visualizer_area: &gtk::DrawingArea,
    settings: &Settings,
) -> RightPanelWidgets {
    let resource = crate::ui_resource::RIGHT_PANEL_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        queue_loading: adw::Spinner,
        root: gtk::Box,
        queue_region: gtk::Overlay,
        queue_panel: gtk::Box,
        queue_search: gtk::SearchEntry,
        queue_clear_button: gtk::Button,
        queue_playlist_button: gtk::Button,
        queue_customize_button: gtk::Button,
        media_overlay: gtk::Overlay,
        lyrics_host: gtk::Box,
        visualizer_module: gtk::Overlay,
        visualizer_controls: gtk::Box,
        visualizer_settings: gtk::Button,
    });
    // Each module clips its own content. Only the split owns the divider region.
    media_overlay.add_overlay(visualizer_area);
    media_overlay.add_overlay(&lyrics_host);
    visualizer_module.add_overlay(&visualizer_controls);
    let controls =
        [SidebarPanel::Lyrics, SidebarPanel::Visualizer].map(|panel| (panel, PanelControls::new()));
    visualizer_controls.prepend(&controls[1].1.root);
    ui_shared::controls::set_search_entry_icon(&queue_search);
    let search_click = gtk::GestureClick::new();
    search_click.set_button(1);
    search_click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let search = queue_search.downgrade();
    search_click.connect_pressed(move |_, _, _, _| {
        if let Some(search) = search.upgrade() {
            search.grab_focus();
        }
    });
    queue_search.add_controller(search_click);
    queue_search.connect_text_notify(|entry| {
        if entry.text().is_empty() {
            entry.remove_css_class("has-query");
        } else {
            entry.add_css_class("has-query");
        }
    });
    queue_search.connect_stop_search(move |entry| {
        entry.set_text("");
        if let Some(root) = entry.root() {
            root.set_focus(gtk::Widget::NONE);
        }
    });
    RightPanelWidgets {
        queue_loading,
        root,
        queue_panel,
        queue_search,
        queue_clear_button,
        queue_playlist_button,
        queue_customize_button,
        queue_width_fit: RefCell::new(None),
        lyrics_host,
        visualizer_settings,
        visualizer_visible: Cell::new(settings.visualizer_panel_visible),
        visualizer_controls,
        modules: [queue_region, media_overlay, visualizer_module],
        controls,
        splits: RefCell::new(Vec::new()),
        visible_panels: RefCell::new(Vec::new()),
        combined: Cell::new(true),
    }
}

impl RightPanelWidgets {
    pub fn attach_lyrics_pane(&self, pane: &crate::lyrics::LyricsPane) {
        self.lyrics_host.append(pane.widget());
        pane.settings_controls.prepend(&self.controls[0].1.root);
    }

    fn rebuild(
        &self,
        settings: &RightPanelSettings,
        combined_height: Option<i32>,
        lyrics_visible: bool,
        visualizer: &gtk::DrawingArea,
    ) {
        let order = settings.visible_panels(lyrics_visible, self.visualizer_visible.get());
        let combined = settings.combined;
        if *self.visible_panels.borrow() == order && self.combined.get() == combined {
            return;
        }
        // Retain the modules and their content while replacing only the split structure.
        for split in self.splits.borrow_mut().drain(..) {
            split.set_start_child(gtk::Widget::NONE);
            split.set_end_child(gtk::Widget::NONE);
        }
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        if self.combined.replace(combined) != combined {
            let from = if combined {
                SidebarPanel::Visualizer
            } else {
                SidebarPanel::Lyrics
            };
            let to = if combined {
                SidebarPanel::Lyrics
            } else {
                SidebarPanel::Visualizer
            };
            self.modules[from.index()].remove_overlay(visualizer);
            self.modules[to.index()].add_overlay(visualizer);
            if combined {
                // Lyrics and their controls stay above the visualizer.
                self.modules[to.index()].remove_overlay(&self.lyrics_host);
                self.modules[to.index()].add_overlay(&self.lyrics_host);
            } else {
                self.modules[to.index()].remove_overlay(&self.visualizer_controls);
                self.modules[to.index()].add_overlay(&self.visualizer_controls);
            }
        }
        for (panel, controls) in &self.controls {
            let position = order.iter().position(|candidate| candidate == panel);
            controls
                .up
                .set_sensitive(position.is_some_and(|index| index > 0));
            controls
                .down
                .set_sensitive(position.is_some_and(|index| index + 1 < order.len()));
        }
        let heights = [
            0,
            if combined {
                combined_height
            } else {
                settings.lyrics_height
            }
            .filter(|height| *height > 0)
            .unwrap_or(300),
            settings
                .visualizer_height
                .filter(|height| *height > 0)
                .unwrap_or(300),
        ];
        // Nest toward the queue. Changes to an outer divider then reach its
        // nearest neighbor, while window resizing still gives space to the queue.
        let queue_last = order.last() == Some(&SidebarPanel::Queue);
        let mut traversal = order.clone();
        if !queue_last {
            traversal.reverse();
        }
        let mut child: gtk::Widget = self.modules[traversal[0].index()].clone().upcast();
        let mut remainder = vec![traversal[0]];
        for panel in traversal.iter().skip(1) {
            let resource = crate::ui_resource::SIDEBAR_SPLIT_RESOURCE;
            let builder = ui_shared::ui_resource::builder(resource);
            ui_shared::objects!(builder, resource, { split: gtk::Paned });
            if queue_last {
                split.set_start_child(Some(&child));
                split.set_end_child(Some(&self.modules[panel.index()]));
            } else {
                split.set_start_child(Some(&self.modules[panel.index()]));
                split.set_end_child(Some(&child));
            }
            let start_fills = !queue_last && *panel == SidebarPanel::Queue;
            let end_fills = queue_last || remainder.contains(&SidebarPanel::Queue);
            split.set_resize_start_child(!end_fills);
            split.set_resize_end_child(end_fills);
            let start_height = heights[panel.index()];
            let end_height: i32 = remainder.iter().map(|panel| heights[panel.index()]).sum();
            let inner_dividers = remainder.len().saturating_sub(1) as i32;
            let weak = split.downgrade();
            let restored = Cell::new(false);
            child = ui_shared::layout::allocation_owner(&split, move |_, height| {
                if restored.replace(true) {
                    return;
                }
                if let Some(split) = weak.upgrade() {
                    // Restore from the real allocation, including native divider thickness.
                    let divider = split
                        .first_child()
                        .and_then(|child| {
                            std::iter::successors(Some(child), gtk::Widget::next_sibling)
                                .find(|child| child.css_name() == "separator")
                        })
                        .map_or(0, |handle| handle.measure(gtk::Orientation::Vertical, -1).1);
                    let available = (height - divider).max(0);
                    let position = if queue_last {
                        end_height + inner_dividers * divider
                    } else if start_fills {
                        available - end_height - inner_dividers * divider
                    } else {
                        start_height
                    };
                    split.set_position(position.clamp(0, available));
                }
            })
            .upcast();
            self.splits.borrow_mut().push(split);
            remainder.insert(0, *panel);
        }
        self.root.append(&child);
        self.visible_panels.replace(order);
    }
}

impl crate::PlayerUi {
    pub fn save_sidebar_panel_sizes(&self) {
        let panel = &self.right_panel;
        let visible = panel.visible_panels.borrow();
        if !panel.root.is_mapped() || visible.len() < 2 {
            return;
        }
        let lyrics = visible
            .contains(&SidebarPanel::Lyrics)
            .then(|| panel.modules[SidebarPanel::Lyrics.index()].height())
            .filter(|height| *height > 0);
        let visualizer = visible
            .contains(&SidebarPanel::Visualizer)
            .then(|| panel.modules[SidebarPanel::Visualizer.index()].height())
            .filter(|height| *height > 0);
        let combined = panel.combined.get();
        {
            let settings = self.settings.current.borrow();
            let saved_lyrics = if combined {
                settings.queue_lyrics_height
            } else {
                settings.right_panel.lyrics_height
            };
            if lyrics.is_none_or(|height| saved_lyrics == Some(height))
                && visualizer
                    .is_none_or(|height| settings.right_panel.visualizer_height == Some(height))
            {
                return;
            }
        }
        self.settings
            .update_app_settings("sidebar panel sizes", |settings| {
                let lyrics_height = if combined {
                    &mut settings.queue_lyrics_height
                } else {
                    &mut settings.right_panel.lyrics_height
                };
                if lyrics.is_some() {
                    *lyrics_height = lyrics;
                }
                if visualizer.is_some() {
                    settings.right_panel.visualizer_height = visualizer;
                }
                true
            });
    }

    pub fn update_right_panel_button(&self) {
        let visible = self.right_sidebar_visible();
        let label = if visible {
            tr("Hide sidebar")
        } else {
            tr("Show sidebar")
        };
        crate::icons::set_queue_sidebar_icon(&self.views.player_controls.queue_icon, visible);
        self.views
            .player_controls
            .queue_button
            .set_tooltip_text(Some(&label));
        self.views
            .player_controls
            .queue_button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    pub fn set_right_panel_combined(self: &Rc<Self>, combined: bool) {
        self.save_sidebar_panel_sizes();
        self.settings
            .set_app_setting("combine sidebar panels", combined, |settings| {
                &mut settings.right_panel.combined
            });
        self.arrange_sidebar_panels();
        self.sync_visualizer_state();
    }

    fn move_sidebar_panel(self: &Rc<Self>, panel: SidebarPanel, direction: isize) {
        let Some((_, controls)) = self
            .right_panel
            .controls
            .iter()
            .find(|(candidate, _)| *candidate == panel)
        else {
            return;
        };
        let had_focus = controls.up.has_focus() || controls.down.has_focus();
        self.save_sidebar_panel_sizes();
        let changed = self
            .settings
            .update_app_settings("sidebar panel order", |settings| {
                settings.right_panel.move_panel(
                    panel,
                    direction,
                    settings.lyrics_panel_visible,
                    settings.visualizer_panel_visible,
                )
            })
            .is_some();
        if !changed {
            return;
        }
        self.arrange_sidebar_panels();
        if !had_focus {
            return;
        }
        let (requested, other) = if direction < 0 {
            (&controls.up, &controls.down)
        } else {
            (&controls.down, &controls.up)
        };
        if requested.is_sensitive() {
            requested.grab_focus();
        } else {
            other.grab_focus();
        }
    }

    fn arrange_sidebar_panels(&self) {
        let (settings, combined_height) = {
            let settings = self.settings.current.borrow();
            (settings.right_panel.clone(), settings.queue_lyrics_height)
        };
        self.right_panel.rebuild(
            &settings,
            combined_height,
            self.lyrics.panel_visible.get(),
            &self.views.visualizer.sidebar_area,
        );
    }

    pub fn set_right_panel_media_visibility(
        self: &Rc<Self>,
        lyrics_visible: bool,
        visualizer_visible: bool,
    ) {
        if self.lyrics.panel_visible.get() != lyrics_visible
            || self.right_panel.visualizer_visible.get() != visualizer_visible
        {
            self.save_sidebar_panel_sizes();
        }
        self.lyrics.panel_visible.set(lyrics_visible);
        self.right_panel.visualizer_visible.set(visualizer_visible);
        self.settings
            .update_app_settings("right panel media visibility", |settings| {
                if settings.lyrics_panel_visible == lyrics_visible
                    && settings.visualizer_panel_visible == visualizer_visible
                {
                    return false;
                }
                settings.lyrics_panel_visible = lyrics_visible;
                settings.visualizer_panel_visible = visualizer_visible;
                true
            });
        apply_sidebar_media_visibility(Rc::clone(self));
    }
}

pub fn connect_sidebar_controls(shell: &Rc<crate::PlayerUi>) {
    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(shell);
    events.connect_event(move |_, event| {
        // Observe completion without competing with GTK's divider gestures.
        if matches!(
            event.event_type(),
            gtk::gdk::EventType::ButtonRelease
                | gtk::gdk::EventType::TouchEnd
                | gtk::gdk::EventType::KeyRelease
        ) && let Some(shell) = weak.upgrade()
        {
            shell.save_sidebar_panel_sizes();
        }
        gtk::glib::Propagation::Proceed
    });
    shell.right_panel.root.add_controller(events);
    for (panel, controls) in &shell.right_panel.controls {
        let panel = *panel;
        for (button, direction) in [(&controls.up, -1), (&controls.down, 1)] {
            let weak = Rc::downgrade(shell);
            button.connect_clicked(move |_| {
                if let Some(shell) = weak.upgrade() {
                    shell.move_sidebar_panel(panel, direction);
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(shell);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if !modifiers.is_empty() {
                return gtk::glib::Propagation::Proceed;
            }
            let direction = match key {
                gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => -1,
                gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => 1,
                _ => return gtk::glib::Propagation::Proceed,
            };
            if let Some(shell) = weak.upgrade() {
                shell.move_sidebar_panel(panel, direction);
            }
            gtk::glib::Propagation::Stop
        });
        controls.root.add_controller(keys);
    }
}

pub fn apply_sidebar_media_visibility(shell: Rc<crate::PlayerUi>) {
    shell.arrange_sidebar_panels();
    shell
        .views
        .visualizer
        .sidebar_area
        .set_visible(shell.right_panel.visualizer_visible.get());
    if shell.lyrics.panel_visible.get() {
        shell.sync_visible_lyrics_surfaces();
    } else {
        shell.update_lyrics_highlight();
    }
    shell.sync_visualizer_state();
}
