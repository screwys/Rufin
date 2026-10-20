use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use audio_processing::effects::{
    BandLayout, EqProfile, FilterKind, FilterWidth, response_curve_db,
};
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use localization::tr;
use rufin_core::playback::PlaybackOwner;
use rufin_core::{EqCastFormat, EqEdit, EqPresetFormat, EqSubscription, EqView};

pub fn equalizer_preset_title(name: &str) -> String {
    match name {
        "Custom" => tr("Custom"),
        "Flat" => tr("Flat"),
        "Classical" => tr("Classical"),
        "Club" => tr("Club"),
        "Dance" => tr("Dance"),
        "Full Bass" => tr("Full Bass"),
        "Full Treble" => tr("Full Treble"),
        "Full Bass & Treble" => tr("Full Bass & Treble"),
        "Laptop/Headphones" => tr("Laptop/Headphones"),
        "Rock" => tr("Rock"),
        "Pop" => tr("Pop"),
        "Techno" => tr("Techno"),
        _ => name.to_owned(),
    }
}

mod plot_imp {
    use super::*;

    #[derive(Default)]
    pub struct EqualizerPlot {
        pub(super) editor: RefCell<Option<std::rc::Weak<Editor>>>,
        pub(super) graphic: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EqualizerPlot {
        const NAME: &'static str = "RufinEqualizerPlot";
        type Type = super::EqualizerPlot;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for EqualizerPlot {}

    impl WidgetImpl for EqualizerPlot {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let editor = self
                .editor
                .borrow()
                .as_ref()
                .and_then(std::rc::Weak::upgrade);
            if let Some(editor) = editor {
                editor.snapshot_plot(&self.obj(), snapshot, self.graphic.get());
            }
        }
    }
}

glib::wrapper! {
    pub struct EqualizerPlot(ObjectSubclass<plot_imp::EqualizerPlot>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

mod gain_track_imp {
    use super::*;

    #[derive(Default)]
    pub struct GainTrackLayout {
        pub(super) scale: glib::WeakRef<gtk::Scale>,
        pub(super) range: Cell<Option<(f64, f64)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GainTrackLayout {
        const NAME: &'static str = "RufinGainTrackLayout";
        type Type = super::GainTrackLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for GainTrackLayout {}

    impl LayoutManagerImpl for GainTrackLayout {
        fn request_mode(&self, _widget: &gtk::Widget) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(
            &self,
            _widget: &gtk::Widget,
            orientation: gtk::Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            self.scale
                .upgrade()
                .map_or((0, 0, -1, -1), |scale| scale.measure(orientation, for_size))
        }

        fn allocate(&self, _widget: &gtk::Widget, width: i32, height: i32, _baseline: i32) {
            let Some(scale) = self.scale.upgrade() else {
                return;
            };
            let adjustment = scale.adjustment();
            let (lower, upper) = self
                .range
                .get()
                .unwrap_or((adjustment.lower(), adjustment.upper()));
            let minimum = scale.measure(gtk::Orientation::Vertical, width).0;
            let travel = f64::from((height - minimum).max(0));
            let offset = (upper - adjustment.upper()) / (upper - lower) * travel;
            let scale_height = (adjustment.upper() - adjustment.lower()) / (upper - lower) * travel
                + f64::from(minimum);
            scale.allocate(
                width,
                scale_height.round() as i32,
                -1,
                Some(
                    gtk::gsk::Transform::new()
                        .translate(&gtk::graphene::Point::new(0.0, offset as f32)),
                ),
            );
        }
    }
}

glib::wrapper! {
    pub struct GainTrackLayout(ObjectSubclass<gain_track_imp::GainTrackLayout>)
        @extends gtk::LayoutManager;
}

#[derive(Clone)]
pub struct EqualizerSurface {
    pub root: gtk::Box,
}

struct BandWidgets {
    track: gtk::Box,
    track_layout: GainTrackLayout,
    gain: gtk::Entry,
    scale: gtk::Scale,
    enabled: gtk::CheckButton,
    kind: gtk::DropDown,
    frequency_value: gtk::Entry,
    gain_value: gtk::Entry,
    width_unit: gtk::DropDown,
    width: gtk::Entry,
    inactive: gtk::Image,
}

struct ResponseInput {
    profile: EqProfile,
    rate: u32,
    enabled: bool,
}

impl ResponseInput {
    fn matches(&self, view: &EqView) -> bool {
        self.profile == view.profile
            && self.rate == view.sample_rate.unwrap_or(48_000)
            && self.enabled == view.enabled
    }
}

struct ResponseCurve {
    input: ResponseInput,
    points: Vec<(f64, f64)>,
}

struct ResponseJob {
    task: Arc<tokio::sync::Mutex<tokio::task::JoinHandle<ResponseCurve>>>,
    abort: tokio::task::AbortHandle,
}

struct Editor {
    root: glib::WeakRef<gtk::Box>,
    player: std::rc::Weak<crate::PlayerUi>,
    owner: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    feedback: Rc<gtk_widgets::feedback::ControlFeedbackState>,
    view: RefCell<EqView>,
    syncing: Cell<bool>,
    initialized: Cell<bool>,
    pointer: Cell<bool>,
    pending: RefCell<Option<glib::SourceId>>,
    clip_hold: RefCell<Option<glib::SourceId>>,
    subscription: RefCell<Option<Arc<EqSubscription>>>,
    listener: RefCell<Option<glib::JoinHandle<()>>>,
    widgets: RefCell<Vec<BandWidgets>>,
    enabled: gtk::Switch,
    toolbar: gtk::Box,
    editor: gtk::Box,
    effects: adw::ExpanderRow,
    unavailable: gtk::Label,
    assigned: gtk::CheckButton,
    preset: gtk::MenuButton,
    save: gtk::Button,
    rename: gtk::Button,
    delete: gtk::Button,
    custom_title: String,
    preset_items: gtk::Box,
    preset_title: gtk::Label,
    output_selector: gtk::DropDown,
    output_name: gtk::Label,
    graphic: gtk::Box,
    parametric: gtk::Box,
    mode: gtk::ToggleButton,
    mode_titles: gtk::StringList,
    bands: gtk::Box,
    filters: gtk::Box,
    graphic_plot: EqualizerPlot,
    parametric_plot: EqualizerPlot,
    response: gtk::CheckButton,
    spectrum: gtk::CheckButton,
    spectrum_unit: gtk::Label,
    layout: gtk::DropDown,
    band_value: gtk::Box,
    band_picker: gtk::DropDown,
    band_gain: gtk::Entry,
    preamp: gtk::Entry,
    preamp_scale: gtk::Scale,
    parametric_preamp: gtk::Entry,
    automatic: gtk::Switch,
    headroom: gtk::Label,
    output_gain: gtk::Label,
    peak: gtk::Label,
    peak_row: adw::ActionRow,
    peak_titles: gtk::StringList,
    clip: gtk::Label,
    bass: gtk::Entry,
    bass_scale: gtk::Scale,
    treble: gtk::Entry,
    treble_scale: gtk::Scale,
    balance: gtk::Entry,
    balance_scale: gtk::Scale,
    crossfeed: gtk::Switch,
    mono: gtk::Switch,
    cast: gtk::Box,
    cast_format: gtk::DropDown,
    apply: gtk::Button,
    add: gtk::Button,
    error: gtk::Label,
    graphic_range: Cell<(f64, f64)>,
    response_curve: RefCell<Option<ResponseCurve>>,
    response_job: RefCell<Option<ResponseJob>>,
    response_waiter: RefCell<Option<glib::JoinHandle<()>>>,
    selected: Cell<usize>,
}

impl Drop for Editor {
    fn drop(&mut self) {
        if let Some(source) = self.pending.get_mut().take() {
            source.remove();
        }
        if let Some(source) = self.clip_hold.get_mut().take() {
            source.remove();
        }
        if let Some(listener) = self.listener.get_mut().take() {
            listener.abort();
        }
        self.subscription.get_mut().take();
        if let Some(waiter) = self.response_waiter.get_mut().take() {
            waiter.abort();
        }
        if let Some(job) = self.response_job.get_mut().take() {
            job.abort.abort();
        }
    }
}

impl EqualizerSurface {
    pub fn new(player: &Rc<crate::PlayerUi>) -> Self {
        EqualizerPlot::static_type();
        GainTrackLayout::static_type();
        let resource = crate::ui_resource::EQUALIZER_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            root: gtk::Box, enabled: gtk::Switch, assigned: gtk::CheckButton,
            toolbar: gtk::Box, editor: gtk::Box, effects: adw::ExpanderRow,
            unavailable: gtk::Label,
            preset: gtk::MenuButton, save: gtk::Button,
            rename: gtk::Button, delete: gtk::Button, duplicate: gtk::Button,
            import: gtk::Button, export: gtk::Button, custom: gtk::Button,
            custom_title: gtk::Label, preset_items: gtk::Box, preset_title: gtk::Label,
            preset_popover: gtk::Popover,
            output_host: gtk::Box, graphic: gtk::Box, parametric: gtk::Box,
            mode: gtk::ToggleButton, mode_titles: gtk::StringList,
            bands: gtk::Box, filters: gtk::Box, graphic_plot: EqualizerPlot,
            parametric_plot: EqualizerPlot, response: gtk::CheckButton,
            spectrum: gtk::CheckButton, spectrum_unit: gtk::Label, layout: gtk::DropDown,
            band_value: gtk::Box, band_picker: gtk::DropDown, band_gain: gtk::Entry,
            preamp: gtk::Entry, preamp_scale: gtk::Scale,
            parametric_preamp: gtk::Entry, automatic: gtk::Switch,
            headroom: gtk::Label, output_gain: gtk::Label, peak: gtk::Label, clip: gtk::Label,
            peak_row: adw::ActionRow, peak_titles: gtk::StringList,
            bass: gtk::Entry, bass_scale: gtk::Scale,
            treble: gtk::Entry, treble_scale: gtk::Scale,
            balance: gtk::Entry, balance_scale: gtk::Scale,
            crossfeed: gtk::Switch, mono: gtk::Switch, cast: gtk::Box,
            cast_format: gtk::DropDown, apply: gtk::Button, add: gtk::Button,
            reset: gtk::Button,
            error: gtk::Label,
        });
        preset.set_popover(Some(&preset_popover));
        let output_selector = crate::outputs::audio_output_dropdown(player, 1);
        output_selector.set_halign(gtk::Align::Start);
        output_host.append(&output_selector);
        let output_name = gtk::Label::new(None);
        output_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        output_name.set_max_width_chars(24);
        output_host.append(&output_name);
        let view = player.playback_owner.equalizer_view(String::new());
        let editor_widget = editor;
        let editor = Rc::new(Editor {
            root: root.downgrade(),
            player: Rc::downgrade(player),
            owner: player.playback_owner.clone(),
            runtime: player.runtime.clone(),
            feedback: player.control_feedback.clone(),
            view: RefCell::new(view.clone()),
            syncing: Cell::new(false),
            initialized: Cell::new(false),
            pointer: Cell::new(false),
            pending: RefCell::new(None),
            clip_hold: RefCell::new(None),
            subscription: RefCell::new(None),
            listener: RefCell::new(None),
            widgets: RefCell::new(Vec::new()),
            enabled,
            toolbar,
            editor: editor_widget,
            effects,
            unavailable,
            assigned,
            preset,
            save,
            rename,
            delete,
            custom_title: custom_title.text().to_string(),
            preset_items,
            preset_title,
            output_selector,
            output_name,
            graphic,
            parametric,
            mode,
            mode_titles,
            bands,
            filters,
            graphic_plot,
            parametric_plot,
            response,
            spectrum,
            spectrum_unit,
            layout,
            band_value,
            band_picker,
            band_gain,
            preamp,
            preamp_scale,
            parametric_preamp,
            automatic,
            headroom,
            output_gain,
            peak,
            peak_row,
            peak_titles,
            clip,
            bass,
            bass_scale,
            treble,
            treble_scale,
            balance,
            balance_scale,
            crossfeed,
            mono,
            cast,
            cast_format,
            apply,
            add,
            error,
            graphic_range: Cell::new((-24.0, 12.0)),
            response_curve: RefCell::new(None),
            response_job: RefCell::new(None),
            response_waiter: RefCell::new(None),
            selected: Cell::new(0),
        });
        editor.connect();
        editor.refresh(view);
        for (button, kind) in [
            (duplicate, NameAction::Duplicate),
            (editor.rename.clone(), NameAction::Rename),
        ] {
            let weak = Rc::downgrade(&editor);
            button.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.name_dialog(kind);
                }
            });
        }
        let weak = Rc::downgrade(&editor);
        editor.save.connect_clicked(move |_| {
            let Some(editor) = weak.upgrade() else { return };
            let update = editor.view.borrow().save_target.is_some();
            if update {
                let owner = editor.owner.clone();
                let output = editor.output();
                editor.run(
                    async move { owner.save_equalizer_preset(output, None).await.map(|_| ()) },
                );
            } else {
                editor.name_dialog(NameAction::Save);
            }
        });
        let weak = Rc::downgrade(&editor);
        editor.delete.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.confirm_delete();
            }
        });
        let weak = Rc::downgrade(&editor);
        import.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.import_dialog();
            }
        });
        let weak = Rc::downgrade(&editor);
        export.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.export_dialog();
            }
        });
        let weak = Rc::downgrade(&editor);
        custom.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.select_preset(None);
            }
        });
        let weak = Rc::downgrade(&editor);
        preset_popover.connect_show(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.refresh_presets();
            }
        });
        let weak = Rc::downgrade(&editor);
        reset.connect_clicked(move |_| {
            let Some(editor) = weak.upgrade() else { return };
            let count = editor.view.borrow().profile.bands.len();
            for index in 0..count {
                editor.preview(EqEdit::BandGain {
                    index: index as u32,
                    value: 0.0,
                });
            }
            editor.commit();
        });
        let weak = Rc::downgrade(&editor);
        root.connect_map(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.subscribe();
            }
        });
        let weak = Rc::downgrade(&editor);
        root.connect_unmap(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.commit_pending();
                if let Some(listener) = editor.listener.borrow_mut().take() {
                    listener.abort();
                }
                editor.subscription.borrow_mut().take();
                if let Some(waiter) = editor.response_waiter.borrow_mut().take() {
                    waiter.abort();
                }
                if let Some(job) = editor.response_job.borrow().as_ref() {
                    job.abort.abort();
                }
                if let Some(source) = editor.clip_hold.borrow_mut().take() {
                    source.remove();
                }
                editor.clip.set_visible(false);
            }
        });
        // The mounted root owns this projection; every callback holds it weakly.
        let _ = root.add_weak_ref_notify_local(move || drop(editor));
        Self { root }
    }
}

impl Editor {
    fn output(&self) -> String {
        self.view.borrow().output.clone()
    }

    fn run<T, E>(self: &Rc<Self>, future: impl Future<Output = Result<T, E>> + Send + 'static)
    where
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        let task = self
            .runtime
            .spawn(async move { future.await.map(|_| ()).map_err(|error| error.to_string()) });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(editor) = weak.upgrade() else { return };
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => editor.feedback.show_feedback_toast(error),
                Err(error) => editor.feedback.show_feedback_toast(error.to_string()),
            }
        });
    }

    fn preview(&self, edit: EqEdit) {
        if self.syncing.get() {
            return;
        }
        if let Err(error) = self.owner.preview_equalizer(self.output(), edit) {
            self.feedback.show_feedback_toast(error.to_string());
        }
    }

    fn edit(self: &Rc<Self>, edit: EqEdit) {
        if self.syncing.get() {
            return;
        }
        self.preview(edit);
        self.commit();
    }

    fn commit(self: &Rc<Self>) {
        if let Some(source) = self.pending.borrow_mut().take() {
            source.remove();
        }
        let owner = self.owner.clone();
        let output = self.output();
        self.run(async move { owner.commit_equalizer(output).await });
    }

    fn commit_pending(self: &Rc<Self>) {
        if self.pending.borrow().is_some() || self.pointer.replace(false) {
            self.commit();
        }
    }

    fn defer_commit(self: &Rc<Self>) {
        if let Some(source) = self.pending.borrow_mut().take() {
            source.remove();
        }
        if self.pointer.get() {
            return;
        }
        let weak = Rc::downgrade(self);
        *self.pending.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_millis(200),
            move || {
                if let Some(editor) = weak.upgrade() {
                    editor.pending.borrow_mut().take();
                    editor.commit();
                }
            },
        ));
    }

    fn subscribe(self: &Rc<Self>) {
        if self.subscription.borrow().is_some() {
            return;
        }
        let subscription = self.owner.clone().subscribe_equalizer(String::new());
        subscription.set_measurement(true, self.spectrum.is_active());
        *self.subscription.borrow_mut() = Some(subscription.clone());
        self.refresh(self.owner.equalizer_view(String::new()));
        self.request_response();
        let weak = Rc::downgrade(self);
        *self.listener.borrow_mut() = Some(glib::spawn_future_local(async move {
            loop {
                let Ok(view) = subscription.next().await else {
                    break;
                };
                let Some(editor) = weak.upgrade() else { break };
                editor.refresh(view);
            }
        }));
    }

    fn connect(self: &Rc<Self>) {
        let switches: [(&gtk::Switch, fn(bool) -> EqEdit); 4] = [
            (&self.enabled, |value| EqEdit::Enabled { value }),
            (&self.automatic, |value| EqEdit::AutomaticPreamp { value }),
            (&self.crossfeed, |value| EqEdit::Crossfeed { value }),
            (&self.mono, |value| EqEdit::Mono { value }),
        ];
        for (switch, edit) in switches {
            let weak = Rc::downgrade(self);
            switch.connect_active_notify(move |switch| {
                if let Some(editor) = weak.upgrade() {
                    editor.edit(edit(switch.is_active()));
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.assigned.connect_toggled(move |check| {
            let Some(editor) = weak.upgrade() else { return };
            if editor.syncing.get() {
                return;
            }
            let owner = editor.owner.clone();
            let output = editor.output();
            let assigned = check.is_active();
            editor.run(async move { owner.assign_equalizer(output, assigned).await });
        });
        let weak = Rc::downgrade(self);
        self.mode.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.show_parametric(!editor.parametric.get_visible());
            }
        });
        for toggle in [&self.response, &self.spectrum] {
            let weak = Rc::downgrade(self);
            toggle.connect_toggled(move |_| {
                let Some(editor) = weak.upgrade() else { return };
                if let Some(subscription) = editor.subscription.borrow().as_ref() {
                    subscription.set_measurement(true, editor.spectrum.is_active());
                }
                editor
                    .spectrum_unit
                    .set_visible(editor.spectrum.is_active());
                editor.update_graphic_range();
                editor.request_response();
                editor.queue_draw();
            });
        }
        let weak = Rc::downgrade(self);
        self.layout.connect_selected_notify(move |dropdown| {
            let Some(editor) = weak.upgrade() else { return };
            if editor.syncing.get() {
                return;
            }
            let layout = match dropdown.selected() {
                0 => BandLayout::Classic10,
                1 => BandLayout::Bands18,
                2 => BandLayout::Bands31,
                _ => return,
            };
            if editor.view.borrow().profile.layout != layout {
                editor.edit(EqEdit::Layout { value: layout });
            }
        });
        let weak = Rc::downgrade(self);
        self.band_picker.connect_selected_notify(move |picker| {
            if let Some(editor) = weak.upgrade()
                && !editor.syncing.get()
            {
                editor.select_band(picker.selected() as usize);
            }
        });
        let edit_editor = Rc::downgrade(self);
        let value_editor = Rc::downgrade(self);
        self.bind_entry(
            &self.band_gain,
            move |value| EqEdit::BandGain {
                index: edit_editor
                    .upgrade()
                    .map_or(0, |editor| editor.selected.get()) as u32,
                value,
            },
            move |view| {
                let index = value_editor.upgrade()?.selected.get();
                view.profile.bands.get(index).map(|band| band.gain_db)
            },
        );
        let weak = Rc::downgrade(self);
        self.cast_format.connect_selected_notify(move |dropdown| {
            if let Some(editor) = weak.upgrade() {
                editor.edit(EqEdit::CastFormat {
                    value: if dropdown.selected() == 0 {
                        EqCastFormat::Mp3
                    } else {
                        EqCastFormat::Flac
                    },
                });
            }
        });
        let weak = Rc::downgrade(self);
        self.apply.connect_clicked(move |_| {
            let Some(editor) = weak.upgrade() else { return };
            if let Err(error) = editor.owner.apply_equalizer(editor.output()) {
                editor.feedback.show_feedback_toast(error.to_string());
            }
        });
        let weak = Rc::downgrade(self);
        self.add.connect_clicked(move |_| {
            if let Some(editor) = weak.upgrade() {
                editor.edit(EqEdit::AddBand);
            }
        });
        self.bind_scale(&self.preamp_scale, |value| EqEdit::Preamp { value });
        self.bind_scale(&self.bass_scale, |value| EqEdit::Bass { value });
        self.bind_scale(&self.treble_scale, |value| EqEdit::Treble { value });
        self.bind_scale(&self.balance_scale, |value| EqEdit::Balance { value });
        self.bind_entry(
            &self.preamp,
            |value| EqEdit::Preamp { value },
            |view| Some(view.profile.manual_preamp_db),
        );
        self.bind_entry(
            &self.parametric_preamp,
            |value| EqEdit::Preamp { value },
            |view| Some(view.profile.manual_preamp_db),
        );
        self.bind_entry(
            &self.bass,
            |value| EqEdit::Bass { value },
            |view| Some(view.profile.bass_db),
        );
        self.bind_entry(
            &self.treble,
            |value| EqEdit::Treble { value },
            |view| Some(view.profile.treble_db),
        );
        self.bind_entry(
            &self.balance,
            |value| EqEdit::Balance { value },
            |view| Some(view.balance),
        );
        for (area, graphic) in [(&self.graphic_plot, true), (&self.parametric_plot, false)] {
            *area.imp().editor.borrow_mut() = Some(Rc::downgrade(self));
            area.imp().graphic.set(graphic);
        }
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, x, _| {
            let Some(editor) = weak.upgrade() else { return };
            let width = editor.parametric_plot.width() as f64;
            let selected = editor
                .view
                .borrow()
                .profile
                .bands
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let ax = frequency_x(a.frequency_hz, width);
                    let bx = frequency_x(b.frequency_hz, width);
                    (ax - x).abs().total_cmp(&(bx - x).abs())
                })
                .map(|(index, _)| index);
            if let Some(selected) = selected {
                editor.select_band(selected);
                if let Some(band) = editor.widgets.borrow().get(selected) {
                    band.frequency_value.grab_focus();
                }
                editor.queue_draw();
            }
        });
        self.parametric_plot.add_controller(click);
    }

    fn select_band(&self, index: usize) {
        let view = self.view.borrow();
        let Some(band) = view.profile.bands.get(index) else {
            return;
        };
        self.selected.set(index);
        let syncing = self.syncing.replace(true);
        self.band_picker.set_selected(index as u32);
        if !self.band_gain.has_focus() {
            self.band_gain.set_text(&band.gain_db.to_string());
        }
        self.syncing.set(syncing);
        self.queue_draw();
    }

    fn bind_scale(self: &Rc<Self>, scale: &gtk::Scale, edit: impl Fn(f64) -> EqEdit + 'static) {
        gtk_widgets::scale::install_scale_scroll_forwarding(scale);
        let weak = Rc::downgrade(self);
        scale.connect_value_changed(move |scale| {
            let Some(editor) = weak.upgrade() else { return };
            if editor.syncing.get() {
                return;
            }
            editor.preview(edit(scale.value()));
            editor.defer_commit();
        });
        let weak = Rc::downgrade(self);
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        events.connect_event(move |_, event| {
            let Some(editor) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match event.event_type() {
                gtk::gdk::EventType::ButtonPress | gtk::gdk::EventType::TouchBegin => {
                    editor.pointer.set(true);
                    if let Some(source) = editor.pending.borrow_mut().take() {
                        source.remove();
                    }
                }
                gtk::gdk::EventType::ButtonRelease
                | gtk::gdk::EventType::TouchEnd
                | gtk::gdk::EventType::TouchCancel
                | gtk::gdk::EventType::GrabBroken => {
                    editor.pointer.set(false);
                    editor.update_graphic_range();
                    editor.queue_draw();
                    editor.defer_commit();
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        scale.add_controller(events);
    }

    fn rebuild_bands(self: &Rc<Self>, count: usize) {
        self.widgets.borrow_mut().clear();
        while let Some(child) = self.bands.first_child() {
            self.bands.remove(&child);
        }
        while let Some(child) = self.filters.first_child() {
            self.filters.remove(&child);
        }
        self.band_value.set_visible(count > 10);
        if count > 10 {
            self.bands.add_css_class("equalizer-dense");
        } else {
            self.bands.remove_css_class("equalizer-dense");
        }
        let mut widgets = Vec::with_capacity(count);
        for index in 0..count {
            let resource = crate::ui_resource::EQUALIZER_BAND_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            gtk_widgets::objects!(builder, resource, {
                band: gtk::Box, track: gtk::Box, track_layout: GainTrackLayout,
                gain: gtk::Entry, scale: gtk::Scale,
            });
            gain.set_visible(count <= 10);
            track_layout.imp().scale.set(Some(&scale));
            track_layout.imp().range.set(Some(self.graphic_range.get()));
            self.bind_scale(&scale, move |value| EqEdit::BandGain {
                index: index as u32,
                value,
            });
            self.bind_entry(
                &gain,
                move |value| EqEdit::BandGain {
                    index: index as u32,
                    value,
                },
                move |view| view.profile.bands.get(index).map(|band| band.gain_db),
            );
            let focus = gtk::EventControllerFocus::new();
            let weak = Rc::downgrade(self);
            focus.connect_enter(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.select_band(index);
                }
            });
            scale.add_controller(focus);
            let click = gtk::GestureClick::new();
            click.set_propagation_phase(gtk::PropagationPhase::Capture);
            let weak = Rc::downgrade(self);
            click.connect_pressed(move |_, presses, _, _| {
                if let Some(editor) = weak.upgrade() {
                    editor.select_band(index);
                }
                if presses == 2 {
                    if let Some(editor) = weak.upgrade() {
                        editor.edit(EqEdit::BandGain {
                            index: index as u32,
                            value: 0.0,
                        });
                    }
                }
            });
            scale.add_controller(click);
            self.bands.append(&band);
            let resource = crate::ui_resource::EQUALIZER_FILTER_RESOURCE;
            let builder = gtk_widgets::ui_resource::builder(resource);
            gtk_widgets::objects!(builder, resource, {
                filter: gtk::Box, enabled: gtk::CheckButton, number: gtk::Label,
                kind: gtk::DropDown, width_unit: gtk::DropDown, width: gtk::Entry,
                inactive: gtk::Image, remove: gtk::Button,
            });
            let frequency_value: gtk::Entry =
                gtk_widgets::ui_resource::object(&builder, resource, "frequency");
            let gain_value: gtk::Entry =
                gtk_widgets::ui_resource::object(&builder, resource, "gain");
            number.set_text(&(index + 1).to_string());
            self.bind_entry(
                &frequency_value,
                move |value| EqEdit::BandFrequency {
                    index: index as u32,
                    value,
                },
                move |view| view.profile.bands.get(index).map(|band| band.frequency_hz),
            );
            self.bind_entry(
                &gain_value,
                move |value| EqEdit::BandGain {
                    index: index as u32,
                    value,
                },
                move |view| view.profile.bands.get(index).map(|band| band.gain_db),
            );
            let unit = width_unit.clone();
            self.bind_entry(
                &width,
                move |value| EqEdit::BandWidth {
                    index: index as u32,
                    value: if unit.selected() == 0 {
                        FilterWidth::Q { value }
                    } else {
                        FilterWidth::Hertz { value }
                    },
                },
                move |view| {
                    view.profile.bands.get(index).map(|band| match band.width {
                        FilterWidth::Q { value } | FilterWidth::Hertz { value } => value,
                    })
                },
            );
            let weak = Rc::downgrade(self);
            width_unit.connect_selected_notify(move |dropdown| {
                let Some(editor) = weak.upgrade() else { return };
                if editor.syncing.get() {
                    return;
                }
                editor.edit(EqEdit::BandWidthUnit {
                    index: index as u32,
                    q: dropdown.selected() == 0,
                });
            });
            let weak = Rc::downgrade(self);
            enabled.connect_toggled(move |check| {
                if let Some(editor) = weak.upgrade() {
                    editor.edit(EqEdit::BandEnabled {
                        index: index as u32,
                        value: check.is_active(),
                    });
                }
            });
            let weak = Rc::downgrade(self);
            kind.connect_selected_notify(move |dropdown| {
                if let Some(editor) = weak.upgrade() {
                    let value = match dropdown.selected() {
                        1 => FilterKind::LowShelf,
                        2 => FilterKind::HighShelf,
                        _ => FilterKind::Peak,
                    };
                    editor.edit(EqEdit::BandKind {
                        index: index as u32,
                        value,
                    });
                }
            });
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.edit(EqEdit::RemoveBand {
                        index: index as u32,
                    });
                }
            });
            self.filters.append(&filter);
            widgets.push(BandWidgets {
                track,
                track_layout,
                gain,
                scale,
                enabled,
                kind,
                frequency_value,
                gain_value,
                width_unit,
                width,
                inactive,
            });
        }
        *self.widgets.borrow_mut() = widgets;
    }

    fn bind_entry(
        self: &Rc<Self>,
        entry: &gtk::Entry,
        edit: impl Fn(f64) -> EqEdit + 'static,
        value: impl Fn(&EqView) -> Option<f64> + 'static,
    ) {
        let weak = Rc::downgrade(self);
        let commit = Rc::new(move |entry: &gtk::Entry| {
            let Some(editor) = weak.upgrade() else { return };
            if editor.syncing.get() {
                return;
            }
            let previous = value(&editor.view.borrow());
            let Some(previous) = previous else { return };
            match entry.text().trim().replace(',', ".").parse::<f64>() {
                Ok(value) if value != previous => editor.edit(edit(value)),
                _ => entry.set_text(&previous.to_string()),
            }
        });
        let activate = commit.clone();
        entry.connect_activate(move |entry| activate(entry));
        let focus = gtk::EventControllerFocus::new();
        let weak_entry = entry.downgrade();
        focus.connect_leave(move |_| {
            if let Some(entry) = weak_entry.upgrade() {
                commit(&entry);
            }
        });
        entry.add_controller(focus);
    }

    fn refresh(self: &Rc<Self>, view: EqView) {
        let first = !self.initialized.replace(true);
        let previous = self.view.replace(view.clone());
        self.peak_row.set_title(
            &self
                .peak_titles
                .string(u32::from(view.cast))
                .expect("peak title"),
        );
        self.peak.set_text(
            &view
                .peak_db
                .iter()
                .map(|peak| format!("{peak:.1} dBFS"))
                .collect::<Vec<_>>()
                .join(" / "),
        );
        if view.peak_db.iter().any(|peak| *peak >= 0.0) {
            self.clip.set_visible(true);
            if let Some(source) = self.clip_hold.borrow_mut().take() {
                source.remove();
            }
            let weak = Rc::downgrade(self);
            *self.clip_hold.borrow_mut() = Some(glib::timeout_add_local_once(
                Duration::from_secs(1),
                move || {
                    if let Some(editor) = weak.upgrade() {
                        editor.clip_hold.borrow_mut().take();
                        editor.clip.set_visible(false);
                    }
                },
            ));
        }
        let controls_unchanged = previous.output == view.output
            && previous.available == view.available
            && previous.profile == view.profile
            && previous.enabled == view.enabled
            && previous.assigned == view.assigned
            && previous.preset_id == view.preset_id
            && previous.save_target == view.save_target
            && previous.name == view.name
            && previous.modified == view.modified
            && previous.preset_builtin == view.preset_builtin
            && previous.crossfeed == view.crossfeed
            && previous.balance == view.balance
            && previous.mono == view.mono
            && previous.cast == view.cast
            && previous.pending == view.pending
            && previous.cast_format == view.cast_format
            && previous.sample_rate == view.sample_rate
            && previous.preamp_db == view.preamp_db
            && previous.error == view.error;
        if controls_unchanged && !first {
            self.queue_draw();
            return;
        }
        let profile_changed = previous.profile != view.profile
            || previous.sample_rate.unwrap_or(48_000) != view.sample_rate.unwrap_or(48_000)
            || previous.enabled != view.enabled
            || first;
        if profile_changed {
            self.response_curve.borrow_mut().take();
        }
        let output_changed = previous.output != view.output;
        if output_changed {
            let pending = self.pending.borrow_mut().take();
            if pending.is_some() || self.pointer.get() {
                if let Some(source) = pending {
                    source.remove();
                }
                let owner = self.owner.clone();
                let output = previous.output.clone();
                self.run(async move { owner.commit_equalizer(output).await });
            }
            self.pointer.set(false);
        }
        self.syncing.set(true);
        if previous.profile.layout != view.profile.layout || first {
            self.show_parametric(view.profile.layout == BandLayout::Parametric);
        }
        if self.widgets.borrow().len() != view.profile.bands.len() {
            self.rebuild_bands(view.profile.bands.len());
        }
        if first
            || previous
                .profile
                .bands
                .iter()
                .map(|band| band.frequency_hz)
                .ne(view.profile.bands.iter().map(|band| band.frequency_hz))
        {
            let names: Vec<_> = view
                .profile
                .bands
                .iter()
                .map(|band| format!("{} Hz", band.frequency_hz))
                .collect();
            self.band_picker.set_model(Some(&gtk::StringList::new(
                &names.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
        }
        self.select_band(
            self.selected
                .get()
                .min(view.profile.bands.len().saturating_sub(1)),
        );
        self.enabled.set_active(view.enabled);
        self.enabled.set_sensitive(view.available);
        self.assigned.set_sensitive(view.available);
        self.toolbar.set_sensitive(view.available);
        self.editor.set_sensitive(view.available);
        self.effects.set_sensitive(view.available);
        self.automatic.set_sensitive(view.available);
        self.cast.set_sensitive(view.available);
        self.unavailable.set_visible(!view.available);
        self.assigned.set_active(view.assigned);
        self.preset_title.set_text(&if view.preset_builtin {
            equalizer_preset_title(&view.name)
        } else if view.preset_id.is_some() {
            view.name.clone()
        } else {
            self.custom_title.clone()
        });
        self.save
            .set_sensitive(view.modified || view.preset_id.is_none());
        let saved = view.preset_id.is_some() && !view.preset_builtin;
        self.rename.set_sensitive(saved);
        self.delete.set_sensitive(saved);
        self.layout.set_selected(match view.profile.layout {
            BandLayout::Classic10 => 0,
            BandLayout::Bands18 => 1,
            BandLayout::Bands31 => 2,
            BandLayout::Parametric => 3,
        });
        if !self.preamp.has_focus() {
            self.preamp
                .set_text(&view.profile.manual_preamp_db.to_string());
        }
        self.preamp_scale.set_value(view.profile.manual_preamp_db);
        if !self.parametric_preamp.has_focus() {
            self.parametric_preamp
                .set_text(&view.profile.manual_preamp_db.to_string());
        }
        self.automatic.set_active(view.profile.automatic_preamp);
        self.headroom.set_text(&format!(
            "{:+.1} dB",
            if view.enabled && view.profile.automatic_preamp {
                view.preamp_db
            } else {
                0.0
            }
        ));
        self.output_gain.set_text(&format!(
            "{:+.1} dB",
            if view.enabled { view.preamp_db } else { 0.0 }
        ));
        if !self.bass.has_focus() {
            self.bass.set_text(&view.profile.bass_db.to_string());
        }
        self.bass_scale.set_value(view.profile.bass_db);
        if !self.treble.has_focus() {
            self.treble.set_text(&view.profile.treble_db.to_string());
        }
        self.treble_scale.set_value(view.profile.treble_db);
        if !self.balance.has_focus() {
            self.balance.set_text(&view.balance.to_string());
        }
        self.balance_scale.set_value(view.balance);
        self.crossfeed.set_active(view.crossfeed);
        self.mono.set_active(view.mono);
        self.cast.set_visible(view.cast);
        self.cast_format
            .set_selected(if view.cast_format == EqCastFormat::Mp3 {
                0
            } else {
                1
            });
        self.apply.set_sensitive(view.pending);
        self.error.set_visible(view.error.is_some());
        self.error
            .set_text(view.error.as_deref().unwrap_or_default());
        self.output_selector.set_visible(!view.cast);
        self.output_name.set_visible(view.cast);
        if !view.cast && (output_changed || first) {
            if let Some(player) = self.player.upgrade() {
                let selected = player
                    .settings
                    .current
                    .borrow()
                    .playback
                    .audio_output
                    .clone();
                let index = player
                    .controls
                    .audio_output_options
                    .borrow()
                    .iter()
                    .position(|(id, _)| *id == selected);
                self.output_selector
                    .set_selected(index.map_or(gtk::INVALID_LIST_POSITION, |index| index as u32));
            }
        }
        if view.cast {
            if let playback::PlaybackOutput::Remote(output) = self.owner.playback_output() {
                self.output_name.set_text(&output.name);
            }
        }
        self.add.set_sensitive(view.profile.bands.len() < 64);
        self.spectrum_unit.set_visible(self.spectrum.is_active());
        for (widgets, band) in self.widgets.borrow().iter().zip(&view.profile.bands) {
            if !widgets.gain.has_focus() {
                widgets.gain.set_text(&band.gain_db.to_string());
            }
            widgets.scale.set_value(band.gain_db);
            let title = format!("{} Hz", band.frequency_hz);
            widgets
                .scale
                .set_tooltip_text(Some(&format!("{title}: {:+} dB", band.gain_db)));
            widgets
                .scale
                .update_property(&[gtk::accessible::Property::Label(&title)]);
            widgets.enabled.set_active(band.enabled);
            widgets.kind.set_selected(match band.kind {
                FilterKind::Peak => 0,
                FilterKind::LowShelf => 1,
                FilterKind::HighShelf => 2,
            });
            if !widgets.frequency_value.has_focus() {
                widgets
                    .frequency_value
                    .set_text(&band.frequency_hz.to_string());
            }
            if !widgets.gain_value.has_focus() {
                widgets.gain_value.set_text(&band.gain_db.to_string());
            }
            let (unit, width) = match band.width {
                FilterWidth::Q { value } => (0, value),
                FilterWidth::Hertz { value } => (1, value),
            };
            widgets.width_unit.set_selected(unit);
            if !widgets.width.has_focus() {
                widgets.width.set_text(&width.to_string());
            }
            widgets.inactive.set_visible(
                view.sample_rate
                    .is_some_and(|rate| band.frequency_hz >= rate as f64 / 2.0),
            );
            widgets.scale.set_opacity(
                if band.enabled
                    && view
                        .sample_rate
                        .is_none_or(|rate| band.frequency_hz < rate as f64 / 2.0)
                {
                    1.0
                } else {
                    0.4
                },
            );
        }
        self.syncing.set(false);
        self.update_graphic_range();
        if profile_changed {
            self.request_response();
        }
        self.queue_draw();
    }

    fn show_parametric(self: &Rc<Self>, parametric: bool) {
        self.graphic.set_visible(!parametric);
        self.parametric.set_visible(parametric);
        self.mode.set_active(parametric);
        self.mode.set_icon_name(if parametric {
            "rufin-sliders-vertical-symbolic"
        } else {
            "rufin-spring-graph-symbolic"
        });
        let title = self
            .mode_titles
            .string(u32::from(!parametric))
            .expect("equalizer mode title");
        self.mode.set_tooltip_text(Some(&title));
        self.mode
            .update_property(&[gtk::accessible::Property::Label(&title)]);
        self.update_graphic_range();
        self.request_response();
        self.queue_draw();
    }

    fn update_graphic_range(&self) {
        if self.syncing.get() || self.pointer.get() {
            return;
        }
        let mut range = (-24.0_f64, 12.0_f64);
        if self.response.is_active() && self.view.borrow().enabled {
            if let Some(curve) = self.response_curve.borrow().as_ref() {
                for (_, value) in &curve.points {
                    range.0 = range.0.min((value / 6.0).floor() * 6.0);
                    range.1 = range.1.max((value / 6.0).ceil() * 6.0);
                }
            }
        }
        if self.graphic_range.replace(range) != range {
            for widgets in self.widgets.borrow().iter() {
                widgets.track_layout.imp().range.set(Some(range));
                widgets.track_layout.layout_changed();
            }
        }
    }

    fn request_response(self: &Rc<Self>) {
        if self.syncing.get()
            || !self.root.upgrade().is_some_and(|root| root.is_mapped())
            || !(self.response.is_active() || self.parametric.get_visible())
        {
            return;
        }
        let view = self.view.borrow();
        if !view.enabled
            || self
                .response_curve
                .borrow()
                .as_ref()
                .is_some_and(|curve| curve.input.matches(&view))
        {
            return;
        }
        if self.response_job.borrow().is_none() {
            let input = ResponseInput {
                profile: view.profile.clone(),
                rate: view.sample_rate.unwrap_or(48_000),
                enabled: view.enabled,
            };
            let task = self.runtime.spawn_blocking(move || {
                let frequencies = (0..300)
                    .map(|index| 20.0 * 1000.0_f64.powf(index as f64 / 299.0))
                    .collect::<Vec<_>>();
                let values = response_curve_db(&input.profile, input.rate, &frequencies);
                ResponseCurve {
                    input,
                    points: frequencies.into_iter().zip(values).collect(),
                }
            });
            *self.response_job.borrow_mut() = Some(ResponseJob {
                abort: task.abort_handle(),
                task: Arc::new(tokio::sync::Mutex::new(task)),
            });
        }
        drop(view);
        if self.response_waiter.borrow().is_some() {
            return;
        }
        let task = self
            .response_job
            .borrow()
            .as_ref()
            .expect("response job")
            .task
            .clone();
        let weak = Rc::downgrade(self);
        *self.response_waiter.borrow_mut() = Some(glib::spawn_future_local(async move {
            // Retaining the handle lets a remapped view await the same blocking calculation.
            let result = {
                let mut task = task.lock().await;
                (&mut *task).await
            };
            let Some(editor) = weak.upgrade() else { return };
            editor.response_waiter.borrow_mut().take();
            editor.response_job.borrow_mut().take();
            match result {
                Ok(curve) => {
                    if curve.input.matches(&editor.view.borrow()) {
                        *editor.response_curve.borrow_mut() = Some(curve);
                        editor.update_graphic_range();
                        editor.queue_draw();
                    }
                    editor.request_response();
                }
                Err(error) if error.is_cancelled() => editor.request_response(),
                Err(error) => editor.feedback.show_feedback_toast(error.to_string()),
            }
        }));
    }

    fn queue_draw(&self) {
        self.graphic_plot.queue_draw();
        self.parametric_plot.queue_draw();
    }

    fn refresh_presets(self: &Rc<Self>) {
        while let Some(child) = self.preset_items.first_child() {
            self.preset_items.remove(&child);
        }
        let selected = self.view.borrow().preset_id.clone();
        for preset in self.owner.equalizer_presets() {
            let button = gtk::Button::with_label(&if preset.builtin {
                equalizer_preset_title(&preset.name)
            } else {
                preset.name
            });
            button.add_css_class("flat");
            if selected.as_deref() == Some(preset.id.as_str()) {
                button.add_css_class("accent");
            }
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.select_preset(Some(preset.id.clone()));
                }
            });
            self.preset_items.append(&button);
        }
    }

    fn select_preset(self: &Rc<Self>, preset: Option<String>) {
        self.preset.popdown();
        self.commit_pending();
        let owner = self.owner.clone();
        let output = self.output();
        self.run(async move { owner.select_equalizer_preset(output, preset).await });
    }
}

#[derive(Clone, Copy)]
enum NameAction {
    Save,
    Duplicate,
    Rename,
}

impl Editor {
    fn name_dialog(self: &Rc<Self>, action: NameAction) {
        let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            name_dialog: adw::Dialog, name: gtk::Entry, name_accept: gtk::Button,
            duplicate_title: gtk::Label, duplicate_label: gtk::Label,
            rename_title: gtk::Label, rename_label: gtk::Label,
        });
        let view = self.view.borrow().clone();
        match action {
            NameAction::Save => {}
            NameAction::Duplicate => {
                name_dialog.set_title(&duplicate_title.text());
                name_accept.set_label(&duplicate_label.text());
                name.set_text(&view.name);
            }
            NameAction::Rename => {
                name_dialog.set_title(&rename_title.text());
                name_accept.set_label(&rename_label.text());
                name.set_text(&view.name);
            }
        }
        name_dialog.set_focus(Some(&name));
        name_accept.set_sensitive(!name.text().trim().is_empty());
        let accept_weak = name_accept.downgrade();
        name.connect_changed(move |entry| {
            if let Some(accept) = accept_weak.upgrade() {
                accept.set_sensitive(!entry.text().trim().is_empty());
            }
        });
        let dialog_weak = name_dialog.downgrade();
        let weak = Rc::downgrade(self);
        name_accept.connect_clicked(move |_| {
            let Some(editor) = weak.upgrade() else { return };
            let name = name.text().trim().to_owned();
            let owner = editor.owner.clone();
            let view = view.clone();
            editor.run(async move {
                match action {
                    NameAction::Save => owner
                        .save_equalizer_preset(view.output, Some(name))
                        .await
                        .map(|_| ()),
                    NameAction::Duplicate => owner
                        .duplicate_equalizer_preset(view.output, name)
                        .await
                        .map(|_| ()),
                    NameAction::Rename => match view.preset_id {
                        Some(id) => owner.rename_equalizer_preset(id, name).await,
                        None => Ok(()),
                    },
                }
            });
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.close();
            }
        });
        if let Some(root) = self.root.upgrade() {
            gtk_widgets::popup::present_light_dismiss_dialog(&name_dialog, &root);
        }
    }

    fn confirm_delete(self: &Rc<Self>) {
        let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, { confirm: adw::AlertDialog });
        let view = self.view.borrow().clone();
        let Some(id) = view.preset_id else { return };
        confirm.set_body(&view.name);
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(root) = root.upgrade() else { return };
            if confirm.choose_future(Some(&root)).await != "accept" {
                return;
            }
            let Some(editor) = weak.upgrade() else { return };
            let owner = editor.owner.clone();
            editor.run(async move { owner.delete_equalizer_preset(id).await });
        });
    }

    fn import_dialog(self: &Rc<Self>) {
        let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            import_dialog: adw::Dialog, import_text: gtk::TextView,
            import_apo: gtk::ToggleButton, import_accept: gtk::Button, import_error: gtk::Label,
            choose_file: gtk::Button, paste: gtk::Button,
            file_open: gtk::FileDialog,
        });
        import_dialog.set_focus(Some(&import_text));
        let buffer = import_text.buffer();
        import_accept.set_sensitive(false);
        let accept_weak = import_accept.downgrade();
        buffer.connect_changed(move |buffer| {
            if let Some(accept) = accept_weak.upgrade() {
                accept.set_sensitive(
                    !buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .trim()
                        .is_empty(),
                );
            }
        });
        let text_weak = import_text.downgrade();
        let format_weak = import_apo.downgrade();
        paste.connect_clicked(move |button| {
            let clipboard = button.clipboard();
            let text_weak = text_weak.clone();
            let format_weak = format_weak.clone();
            glib::spawn_future_local(async move {
                let Ok(Some(text)) = clipboard.read_text_future().await else {
                    return;
                };
                if let Some(format) = format_weak.upgrade() {
                    format.set_active(!text.trim_start().starts_with('{'));
                }
                if let Some(view) = text_weak.upgrade() {
                    view.buffer().set_text(&text);
                }
            });
        });
        let weak = Rc::downgrade(self);
        let text_weak = import_text.downgrade();
        let format_weak = import_apo.downgrade();
        choose_file.connect_clicked(move |_| {
            let Some(editor) = weak.upgrade() else { return };
            let Some(root) = editor.root.upgrade() else {
                return;
            };
            let window = root.root().and_downcast::<gtk::Window>();
            let chooser = file_open.clone();
            let weak = weak.clone();
            let text_weak = text_weak.clone();
            let format_weak = format_weak.clone();
            glib::spawn_future_local(async move {
                let Ok(file) = chooser.open_future(window.as_ref()).await else {
                    return;
                };
                match file.load_contents_future().await {
                    Ok((bytes, _)) => match String::from_utf8(bytes.to_vec()) {
                        Ok(text) => {
                            if let Some(format) = format_weak.upgrade() {
                                format.set_active(!text.trim_start().starts_with('{'));
                            }
                            if let Some(view) = text_weak.upgrade() {
                                view.buffer().set_text(&text);
                            }
                        }
                        Err(error) => {
                            if let Some(editor) = weak.upgrade() {
                                editor.feedback.show_feedback_toast(error.to_string());
                            }
                        }
                    },
                    Err(error) => {
                        if let Some(editor) = weak.upgrade() {
                            editor.feedback.show_feedback_toast(error.to_string());
                        }
                    }
                }
            });
        });
        let output = self.output();
        let weak = Rc::downgrade(self);
        let dialog_weak = import_dialog.downgrade();
        import_accept.connect_clicked(move |button| {
            let Some(editor) = weak.upgrade() else { return };
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            let format = preset_format(import_apo.is_active());
            let owner = editor.owner.clone();
            let output = output.clone();
            let task = editor
                .runtime
                .spawn(async move { owner.import_equalizer(output, text, format).await });
            button.set_sensitive(false);
            import_error.set_visible(false);
            let button = button.downgrade();
            let dialog = dialog_weak.clone();
            let error_label = import_error.downgrade();
            glib::spawn_future_local(async move {
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                let error = match task.await {
                    Ok(Ok(_)) => {
                        dialog.close();
                        return;
                    }
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => error.to_string(),
                };
                if let Some(button) = button.upgrade() {
                    button.set_sensitive(true);
                }
                if let Some(label) = error_label.upgrade() {
                    label.set_text(&error);
                    label.set_visible(true);
                }
            });
        });
        if let Some(root) = self.root.upgrade() {
            gtk_widgets::popup::present_light_dismiss_dialog(&import_dialog, &root);
        }
    }

    fn export_dialog(self: &Rc<Self>) {
        let resource = crate::ui_resource::EQUALIZER_DIALOGS_RESOURCE;
        let builder = gtk_widgets::ui_resource::builder(resource);
        gtk_widgets::objects!(builder, resource, {
            export_dialog: adw::Dialog, export_apo: gtk::ToggleButton,
            export_text: gtk::TextView, export_error: gtk::Label, file_save: gtk::FileDialog,
            export_accept: gtk::Button, export_save: gtk::Button,
        });
        let buffer = export_text.buffer();
        let preview_buffer = buffer.clone();
        let copy_weak = export_accept.downgrade();
        let save_weak = export_save.downgrade();
        let weak = Rc::downgrade(self);
        let output = self.output();
        let preview = move |format: &gtk::ToggleButton| {
            let Some(editor) = weak.upgrade() else { return };
            let result = editor
                .owner
                .export_equalizer(output.clone(), preset_format(format.is_active()));
            if let Some(copy) = copy_weak.upgrade() {
                copy.set_sensitive(result.is_ok());
            }
            if let Some(save) = save_weak.upgrade() {
                save.set_sensitive(result.is_ok());
            }
            match result {
                Ok(text) => {
                    preview_buffer.set_text(&text);
                    export_error.set_visible(false);
                }
                Err(error) => {
                    preview_buffer.set_text("");
                    export_error.set_text(&error.to_string());
                    export_error.set_visible(true);
                }
            }
        };
        preview(&export_apo);
        export_apo.connect_toggled(preview);
        let copy_buffer = buffer.clone();
        let dialog_weak = export_dialog.downgrade();
        export_accept.connect_clicked(move |button| {
            let text = copy_buffer.text(&copy_buffer.start_iter(), &copy_buffer.end_iter(), false);
            button.clipboard().set_text(&text);
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.close();
            }
        });
        let weak = Rc::downgrade(self);
        let dialog_weak = export_dialog.downgrade();
        export_save.connect_clicked(move |button| {
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            file_save.set_initial_name(Some(if export_apo.is_active() {
                "equalizer.txt"
            } else {
                "equalizer.rufin-eq.json"
            }));
            let window = button.root().and_downcast::<gtk::Window>();
            let chooser = file_save.clone();
            let weak = weak.clone();
            let dialog = dialog_weak.clone();
            glib::spawn_future_local(async move {
                let Ok(file) = chooser.save_future(window.as_ref()).await else {
                    return;
                };
                match file
                    .replace_contents_future(
                        text.into_bytes(),
                        None,
                        false,
                        gio::FileCreateFlags::REPLACE_DESTINATION,
                    )
                    .await
                {
                    Ok(_) => {
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.close();
                        }
                    }
                    Err((_, error)) => {
                        if let Some(editor) = weak.upgrade() {
                            editor.feedback.show_feedback_toast(error.to_string());
                        }
                    }
                }
            });
        });
        if let Some(root) = self.root.upgrade() {
            gtk_widgets::popup::present_light_dismiss_dialog(&export_dialog, &root);
        }
    }

    fn snapshot_plot(&self, area: &EqualizerPlot, snapshot: &gtk::Snapshot, graphic: bool) {
        let (width, height) = (area.width(), area.height());
        if width <= 0 || height <= 0 {
            return;
        }
        let view = self.view.borrow();
        let curve = self.response_curve.borrow();
        let points = curve
            .as_ref()
            .map_or(&[][..], |curve| curve.points.as_slice());
        let width = width as f64;
        let height = height as f64;
        let accent = area.color();
        let foreground = self.root.upgrade().map_or(accent, |root| root.color());
        let (mut low, mut high) = if graphic {
            self.graphic_range.get()
        } else {
            (-24.0, 12.0)
        };
        if !graphic {
            for (_, value) in points.iter().filter(|(_, value)| value.is_finite()) {
                low = low.min((value / 6.0).floor() * 6.0);
                high = high.max((value / 6.0).ceil() * 6.0);
            }
        }
        let widgets = self.widgets.borrow();
        let positions = if graphic {
            widgets
                .iter()
                .filter_map(|widgets| {
                    let rect = widgets.scale.range_rect();
                    let (start, end) = widgets.scale.slider_range();
                    let inset = widgets
                        .scale
                        .measure(gtk::Orientation::Vertical, widgets.track.width())
                        .0 as f32
                        / 2.0;
                    let x = rect.x() as f32 + rect.width() as f32 / 2.0;
                    let top = widgets
                        .track
                        .compute_point(area, &gtk::graphene::Point::new(0.0, inset))?;
                    let bottom = widgets.track.compute_point(
                        area,
                        &gtk::graphene::Point::new(0.0, widgets.track.height() as f32 - inset),
                    )?;
                    let handle = widgets.scale.compute_point(
                        area,
                        &gtk::graphene::Point::new(x, (start + end) as f32 / 2.0),
                    )?;
                    Some((
                        handle.x() as f64,
                        top.y() as f64,
                        bottom.y() as f64,
                        handle.y() as f64,
                    ))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let top = positions.first().map(|p| p.1).unwrap_or(20.0);
        let bottom = positions.first().map(|p| p.2).unwrap_or(height - 28.0);
        let left = if graphic { 0.0 } else { 36.0 };
        let right = if graphic { width } else { width - 36.0 };
        let y = |value: f64| top + (high - value) / (high - low) * (bottom - top);
        let x = |frequency: f64| {
            if graphic && !positions.is_empty() {
                let bands = &view.profile.bands;
                let upper = bands.iter().position(|band| band.frequency_hz >= frequency);
                match upper {
                    Some(0) => positions[0].0,
                    Some(index) => {
                        let first = bands[index - 1].frequency_hz.ln();
                        let last = bands[index].frequency_hz.ln();
                        let fraction = if last > first {
                            (frequency.ln() - first) / (last - first)
                        } else {
                            0.0
                        };
                        positions[index - 1].0
                            + fraction * (positions[index].0 - positions[index - 1].0)
                    }
                    None => positions.last().unwrap().0,
                }
            } else {
                left + (frequency / 20.0).log10() / 3.0 * (right - left)
            }
        };
        let color = |mut color: gtk::gdk::RGBA, alpha: f32| {
            color.set_alpha(alpha);
            color
        };
        let rect = |x: f64, y: f64, width: f64, height: f64| {
            gtk::graphene::Rect::new(x as f32, y as f32, width as f32, height as f32)
        };
        let layout = area.create_pango_layout(None);
        let mut font = gtk::pango::FontDescription::new();
        font.set_absolute_size(10.0 * gtk::pango::SCALE as f64);
        layout.set_font_description(Some(&font));
        let label = |text: &str, x: f64, y: f64, alpha: f32| {
            layout.set_text(text);
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(x as f32, y as f32));
            snapshot.append_layout(&layout, &color(foreground, alpha));
            snapshot.restore();
        };
        let step = (((high - low) / 4.0 / 6.0).ceil() * 6.0).max(6.0);
        let mut ticks = vec![low, high];
        ticks.extend(
            ((low / step).ceil() as i32..=(high / step).floor() as i32)
                .map(|tick| tick as f64 * step),
        );
        ticks.sort_by(f64::total_cmp);
        ticks.dedup();
        for value in ticks {
            snapshot.append_color(
                &color(foreground, if value == 0.0 { 0.25 } else { 0.10 }),
                &rect(left, y(value), right - left, 1.0),
            );
            label(&format!("{value:.0}"), 2.0, y(value) - 5.0, 0.65);
        }
        if !graphic {
            for frequency in [20.0, 100.0, 1000.0, 10000.0, 20000.0] {
                snapshot.append_color(
                    &color(foreground, 0.1),
                    &rect(x(frequency), top, 1.0, bottom - top),
                );
                label(
                    &frequency_label(frequency),
                    x(frequency) - 10.0,
                    height - 16.0,
                    0.65,
                );
            }
        }
        snapshot.push_clip(&rect(left, top, right - left, (bottom - top).max(0.0)));
        if self.spectrum.is_active() && !view.spectrum_db.is_empty() {
            let mut columns = vec![f64::NEG_INFINITY; (right - left).ceil().max(1.0) as usize];
            let step = view.sample_rate.unwrap_or(48000) as f64
                / (2 * view.spectrum_db.len().saturating_sub(1).max(1)) as f64;
            for (index, level) in view.spectrum_db.iter().enumerate() {
                let frequency = index as f64 * step;
                if frequency + step / 2.0 < 20.0 || frequency - step / 2.0 > 20000.0 {
                    continue;
                }
                let start = (x((frequency - step / 2.0).max(20.0)) - left)
                    .floor()
                    .max(0.0) as usize;
                let end = ((x((frequency + step / 2.0).min(20000.0)) - left)
                    .ceil()
                    .max(0.0) as usize)
                    .min(columns.len());
                for column in columns.iter_mut().take(end).skip(start) {
                    *column = column.max(*level);
                }
            }
            for (index, level) in columns
                .into_iter()
                .enumerate()
                .filter(|(_, level)| level.is_finite())
            {
                let height = (level.clamp(-90.0, 0.0) + 90.0) / 90.0 * (bottom - top);
                snapshot.append_color(
                    &color(accent, 0.15),
                    &rect(left + index as f64, bottom - height, 1.0, height),
                );
            }
        }
        if !graphic || self.response.is_active() {
            let path = gtk::gsk::PathBuilder::new();
            if !view.enabled {
                path.move_to(left as f32, y(0.0) as f32);
                path.line_to(right as f32, y(0.0) as f32);
            } else {
                for (index, (frequency, db)) in points.iter().enumerate() {
                    if index == 0 {
                        path.move_to(x(*frequency) as f32, y(*db) as f32);
                    } else {
                        path.line_to(x(*frequency) as f32, y(*db) as f32);
                    }
                }
            }
            let stroke = gtk::gsk::Stroke::new(2.0);
            if graphic {
                stroke.set_dash(&[5.0, 4.0]);
            }
            snapshot.append_stroke(
                &path.to_path(),
                &stroke,
                &color(accent, if view.enabled { 0.95 } else { 0.35 }),
            );
        }
        if graphic && positions.len() > 1 {
            let path = gtk::gsk::PathBuilder::new();
            let fill = gtk::gsk::PathBuilder::new();
            for (index, position) in positions.iter().enumerate() {
                for builder in [&path, &fill] {
                    if index == 0 {
                        builder.move_to(position.0 as f32, position.3 as f32);
                    } else {
                        let previous = positions[index - 1];
                        let middle = ((previous.0 + position.0) / 2.0) as f32;
                        builder.cubic_to(
                            middle,
                            previous.3 as f32,
                            middle,
                            position.3 as f32,
                            position.0 as f32,
                            position.3 as f32,
                        );
                    }
                }
            }
            let outline = path.to_path();
            fill.line_to(positions.last().unwrap().0 as f32, bottom as f32);
            fill.line_to(positions[0].0 as f32, bottom as f32);
            fill.close();
            snapshot.append_fill(
                &fill.to_path(),
                gtk::gsk::FillRule::Winding,
                &color(accent, if view.enabled { 0.07 } else { 0.025 }),
            );
            snapshot.append_stroke(
                &outline,
                &gtk::gsk::Stroke::new(2.0),
                &color(accent, if view.enabled { 0.8 } else { 0.3 }),
            );
        } else if !graphic {
            for (index, band) in view.profile.bands.iter().enumerate() {
                let path = gtk::gsk::PathBuilder::new();
                path.add_circle(
                    &gtk::graphene::Point::new(x(band.frequency_hz) as f32, y(band.gain_db) as f32),
                    if self.selected.get() == index {
                        5.0
                    } else {
                        3.0
                    },
                );
                snapshot.append_fill(
                    &path.to_path(),
                    gtk::gsk::FillRule::Winding,
                    &color(accent, if band.enabled { 1.0 } else { 0.3 }),
                );
            }
        }
        snapshot.pop();
        if graphic && !positions.is_empty() {
            let count = positions.len();
            let labels = count.min(((right - left) / 34.0).floor().max(2.0) as usize);
            for tick in 0..labels {
                let index = if labels == 1 {
                    0
                } else {
                    tick * (count - 1) / (labels - 1)
                };
                let text = frequency_label(view.profile.bands[index].frequency_hz);
                layout.set_text(&text);
                label(
                    &text,
                    positions[index].0 - f64::from(layout.pixel_size().0) / 2.0,
                    bottom + 12.0,
                    0.75,
                );
            }
        }
        if self.spectrum.is_active() {
            for db in [0.0, -30.0, -60.0, -90.0] {
                label(
                    &format!("{db:.0}"),
                    right - 24.0,
                    top + -db / 90.0 * (bottom - top),
                    0.55,
                );
            }
        }
    }
}

fn frequency_label(frequency: f64) -> String {
    if frequency >= 1000.0 {
        format!("{}k", frequency / 1000.0)
    } else {
        format!("{frequency}")
    }
}

fn frequency_x(frequency: f64, width: f64) -> f64 {
    36.0 + (frequency / 20.0).log10() / 3.0 * (width - 72.0)
}

fn preset_format(apo: bool) -> EqPresetFormat {
    if apo {
        EqPresetFormat::Apo
    } else {
        EqPresetFormat::Json
    }
}
