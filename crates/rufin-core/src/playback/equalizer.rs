//! Equalizer editing, presets and output assignments owned by playback.
use super::{PlaybackError, PlaybackOwner, PlaybackStoreWork};
use audio_processing::effects::{
    BandLayout, EqProfile, Filter, FilterKind, FilterWidth, ProcessingConfig,
};
use playback::{PlaybackOutput, ProcessingRequest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{oneshot, watch};

const CLASSIC: &[f64] = &[
    60., 170., 310., 600., 1000., 3000., 6000., 12000., 14000., 16000.,
];
const EIGHTEEN: &[f64] = &[
    31., 63., 87., 125., 175., 250., 350., 500., 700., 1000., 1400., 2000., 2800., 4000., 5600.,
    8000., 11200., 16000.,
];
const THIRTY_ONE: &[f64] = &[
    20., 25., 31.5, 40., 50., 63., 80., 100., 125., 160., 200., 250., 315., 400., 500., 630., 800.,
    1000., 1250., 1600., 2000., 2500., 3150., 4000., 5000., 6300., 8000., 10000., 12500., 16000.,
    20000.,
];
const BUILTINS: &[(&str, &str, [f64; 10])] = &[
    ("flat", "Flat", [0.; 10]),
    (
        "classical",
        "Classical",
        [0., 0., 0., 0., 0., 0., -7.2, -7.2, -7.2, -9.6],
    ),
    (
        "club",
        "Club",
        [0., 0., 3.2, 5.6, 5.6, 5.6, 3.2, 0., 0., 0.],
    ),
    (
        "dance",
        "Dance",
        [9.6, 7.2, 2.4, 0., 0., -5.6, -7.2, -7.2, 0., 0.],
    ),
    (
        "full-bass",
        "Full Bass",
        [9.6, 9.6, 9.6, 5.6, 1.6, -4., -8., -10.4, -11.2, -11.2],
    ),
    (
        "full-treble",
        "Full Treble",
        [-9.6, -9.6, -9.6, -4., 2.4, 11.2, 12., 12., 12., 12.],
    ),
    (
        "full-bass-treble",
        "Full Bass & Treble",
        [7.2, 5.6, 0., -7.2, -4.8, 1.6, 8., 11.2, 12., 12.],
    ),
    (
        "headphones",
        "Laptop/Headphones",
        [4.8, 11.2, 5.6, -3.2, -2.4, 1.6, 4.8, 9.6, 12., 12.],
    ),
    (
        "rock",
        "Rock",
        [8., 4.8, -5.6, -8., -3.2, 4., 8.8, 11.2, 11.2, 11.2],
    ),
    (
        "pop",
        "Pop",
        [-1.6, 4.8, 7.2, 8., 5.6, 0., -2.4, -2.4, -1.6, -1.6],
    ),
    (
        "techno",
        "Techno",
        [8., 5.6, 0., -5.6, -4.8, 0., 8., 9.6, 9.6, 8.8],
    ),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum EqCastFormat {
    #[default]
    Mp3,
    Flac,
}
impl EqCastFormat {
    fn encoding(self) -> library::AudioEncoding {
        match self {
            Self::Mp3 => library::AudioEncoding::mp3(),
            Self::Flac => library::AudioEncoding {
                codec: library::AudioCodec::Flac,
                bitrate_kbps: None,
                channels: library::AudioChannels::Preserve,
            },
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum EqPresetFormat {
    Json,
    Apo,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum EqEdit {
    Enabled { value: bool },
    BandGain { index: u32, value: f64 },
    BandFrequency { index: u32, value: f64 },
    BandWidth { index: u32, value: FilterWidth },
    BandWidthUnit { index: u32, q: bool },
    BandKind { index: u32, value: FilterKind },
    BandEnabled { index: u32, value: bool },
    AddBand,
    RemoveBand { index: u32 },
    Layout { value: BandLayout },
    Preamp { value: f64 },
    AutomaticPreamp { value: bool },
    Bass { value: f64 },
    Treble { value: f64 },
    Crossfeed { value: bool },
    Balance { value: f64 },
    Mono { value: bool },
    CastFormat { value: EqCastFormat },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EqPresetSummary {
    pub id: String,
    pub name: String,
    pub builtin: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedPreset {
    pub id: String,
    pub name: String,
    pub profile: EqProfile,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqDraft {
    pub profile: EqProfile,
    /// Preset the draft was edited from, including read-only built-ins.
    pub save_target: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputEq {
    pub enabled: bool,
    pub selection: Option<String>,
    pub draft: EqDraft,
    pub crossfeed: bool,
    pub balance: f64,
    pub mono: bool,
    pub cast_format: EqCastFormat,
}
impl Default for OutputEq {
    fn default() -> Self {
        Self {
            enabled: false,
            selection: Some("flat".into()),
            draft: EqDraft {
                profile: template(BandLayout::Classic10),
                save_target: None,
            },
            crossfeed: false,
            balance: 0.,
            mono: false,
            cast_format: EqCastFormat::Mp3,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqualizerPreferences {
    pub default_output: OutputEq,
    pub outputs: BTreeMap<String, OutputEq>,
    pub presets: Vec<SavedPreset>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EqView {
    pub output: String,
    pub enabled: bool,
    pub profile: EqProfile,
    pub preset_id: Option<String>,
    pub save_target: Option<String>,
    pub name: String,
    pub preset_builtin: bool,
    pub modified: bool,
    pub assigned: bool,
    pub crossfeed: bool,
    pub balance: f64,
    pub mono: bool,
    pub cast: bool,
    pub available: bool,
    pub pending: bool,
    pub cast_format: EqCastFormat,
    pub sample_rate: Option<u32>,
    pub preamp_db: f64,
    pub peak_db: Vec<f64>,
    pub spectrum_db: Vec<f64>,
    pub error: Option<String>,
}
#[derive(Default)]
struct ExecutionFacts {
    config: Option<ProcessingConfig>,
    sample_rate: Option<u32>,
    preamp_db: f64,
    peak_db: Vec<f64>,
    spectrum_db: Vec<f64>,
    error: Option<String>,
}
struct Observer {
    id: u64,
    output: String,
    sender: watch::Sender<EqView>,
    meter: bool,
    spectrum: bool,
}
pub(super) struct EqualizerState {
    pub preferences: EqualizerPreferences,
    current_output: String,
    local_device: Option<String>,
    observed_route: Option<String>,
    processing: BTreeMap<String, watch::Sender<ProcessingRequest>>,
    observers: Vec<Observer>,
    next_observer: u64,
    facts: BTreeMap<String, ExecutionFacts>,
    write_error: Option<String>,
}
impl EqualizerState {
    pub fn new(preferences: EqualizerPreferences, local: Option<&str>) -> Self {
        Self {
            preferences,
            current_output: local_id(local),
            local_device: local.map(str::to_string),
            observed_route: None,
            processing: BTreeMap::new(),
            observers: Vec::new(),
            next_observer: 0,
            facts: BTreeMap::new(),
            write_error: None,
        }
    }
    fn resolve(&self, output: &str) -> String {
        if output.is_empty() {
            self.current_output.clone()
        } else {
            output.into()
        }
    }
    fn request(&self, output: &str) -> ProcessingRequest {
        let settings = self.preferences.output(output);
        let (meter, spectrum) = self
            .observers
            .iter()
            .filter(|o| self.resolve(&o.output) == output)
            .fold((false, false), |(m, s), o| (m || o.meter, s || o.spectrum));
        ProcessingRequest {
            output: output.into(),
            local_device: self.local_device.clone(),
            config: self.preferences.config(output),
            meter,
            spectrum,
            cast_encoding: settings.cast_format.encoding(),
        }
    }
    fn submit(&mut self, output: &str, force: bool) {
        if output != self.current_output
            && (self.preferences.outputs.contains_key(output)
                || self.preferences.outputs.contains_key(&self.current_output))
        {
            return;
        }
        let active = self.current_output.clone();
        let output = active.as_str();
        self.processing.retain(|_, sender| !sender.is_closed());
        let request = self.request(output);
        if let Some(sender) = self.processing.get(&backend_key(output)) {
            if force {
                sender.send_replace(request);
            } else {
                sender.send_if_modified(|current| {
                    if current.config.same_processing(&request.config)
                        && current.output == request.output
                        && current.local_device == request.local_device
                        && current.meter == request.meter
                        && current.spectrum == request.spectrum
                        && current.cast_encoding == request.cast_encoding
                    {
                        false
                    } else {
                        *current = request;
                        true
                    }
                });
            }
        }
    }
    fn refresh_measurement(&self) {
        for sender in self.processing.values() {
            let output = sender.borrow().output.clone();
            let (meter, spectrum) = self
                .observers
                .iter()
                .filter(|o| self.resolve(&o.output) == output)
                .fold((false, false), |(m, s), o| (m || o.meter, s || o.spectrum));
            sender.send_if_modified(|request| {
                if request.meter == meter && request.spectrum == spectrum {
                    false
                } else {
                    request.meter = meter;
                    request.spectrum = spectrum;
                    true
                }
            });
        }
    }
    fn view(&self, output: &str, settings: &crate::settings::SettingsFile) -> EqView {
        let output = self.resolve(output);
        let entry = self.preferences.output(&output);
        let profile = self.preferences.profile(entry);
        let facts = self.facts.get(&output);
        let preset_id = entry
            .selection
            .as_ref()
            .or(entry.draft.save_target.as_ref());
        let selected = preset_id.and_then(|id| self.preferences.preset(id));
        let preset_builtin = preset_id.is_some_and(|id| builtin(id).is_some());
        let target = preset_id
            .filter(|id| self.preferences.presets.iter().any(|p| &p.id == *id))
            .cloned();
        let saved = target.as_ref().and_then(|id| settings.equalizer_preset(id));
        let modified = if preset_builtin {
            selected.as_ref().is_some_and(|p| p.profile != profile)
        } else if entry.selection.is_some() {
            selected.as_ref() != saved.as_ref()
        } else {
            saved.as_ref().is_none_or(|p| p.profile != profile)
        };
        let cast = output.starts_with("remote:");
        let pending = cast
            && (facts.is_some_and(|f| f.error.is_some())
                || self.processing.get(&backend_key(&output)).is_none_or(|tx| {
                    !tx.borrow()
                        .config
                        .same_processing(&self.preferences.config(&output))
                        || tx.borrow().cast_encoding != entry.cast_format.encoding()
                }));
        EqView {
            output: output.clone(),
            enabled: entry.enabled,
            profile: profile.clone(),
            preset_id: preset_id.cloned(),
            save_target: target,
            name: selected
                .map(|p| p.name)
                .or_else(|| saved.map(|p| p.name))
                .unwrap_or_else(|| "Custom".into()),
            preset_builtin,
            modified,
            assigned: self.preferences.outputs.contains_key(&output),
            crossfeed: entry.crossfeed,
            balance: entry.balance,
            mono: entry.mono,
            cast,
            available: output_available(&output),
            pending,
            cast_format: entry.cast_format,
            sample_rate: facts.and_then(|f| f.sample_rate),
            preamp_db: if profile.automatic_preamp {
                facts.map_or(0., |f| f.preamp_db)
            } else {
                profile.manual_preamp_db
            },
            peak_db: facts.map(|f| f.peak_db.clone()).unwrap_or_default(),
            spectrum_db: facts.map(|f| f.spectrum_db.clone()).unwrap_or_default(),
            error: self
                .write_error
                .clone()
                .or_else(|| facts.and_then(|f| f.error.clone())),
        }
    }
    fn publish(&mut self, settings: &crate::settings::SettingsFile) {
        self.observers.retain(|o| o.sender.receiver_count() > 0);
        for observer in &self.observers {
            let view = self.view(&observer.output, settings);
            observer.sender.send_if_modified(|old| {
                if *old == view {
                    false
                } else {
                    *old = view;
                    true
                }
            });
        }
    }
}

fn local_id(device: Option<&str>) -> String {
    format!("local:{}", device.unwrap_or("default"))
}
fn output_id(output: &PlaybackOutput, device: Option<&str>) -> String {
    match output {
        PlaybackOutput::Local => local_id(device),
        PlaybackOutput::Remote(remote) => format!("remote:{:?}:{}", remote.protocol, remote.id),
    }
}
fn output_available(output: &str) -> bool {
    !output.starts_with("remote:PlexCompanion:") && !output.starts_with("remote:RufinConnect:")
}
fn backend_key(output: &str) -> String {
    if output.starts_with("local:") {
        "local".into()
    } else {
        output.into()
    }
}
fn error(message: impl Into<String>) -> PlaybackError {
    PlaybackError::Message(message.into())
}
fn checked_name(name: &str) -> Result<String, PlaybackError> {
    let name = name.trim();
    if name.is_empty() {
        Err(error("Enter a preset name"))
    } else {
        Ok(name.into())
    }
}
fn identity() -> String {
    format!("preset-{}", super::random_identity())
}

pub fn template(layout: BandLayout) -> EqProfile {
    let frequencies = match layout {
        BandLayout::Classic10 => CLASSIC,
        BandLayout::Bands18 => EIGHTEEN,
        BandLayout::Bands31 => THIRTY_ONE,
        BandLayout::Parametric => &[],
    };
    let bands = frequencies
        .iter()
        .enumerate()
        .map(|(i, &frequency)| {
            let width = if layout == BandLayout::Classic10 {
                frequency - if i == 0 { 0. } else { frequencies[i - 1] }
            } else {
                let below = if i == 0 {
                    frequency * frequency / frequencies[1]
                } else {
                    frequencies[i - 1]
                };
                let above = if i + 1 == frequencies.len() {
                    frequency * frequency / frequencies[i - 1]
                } else {
                    frequencies[i + 1]
                };
                (frequency * above).sqrt() - (frequency * below).sqrt()
            };
            Filter {
                kind: FilterKind::Peak,
                frequency_hz: frequency,
                gain_db: 0.,
                width: FilterWidth::Hertz { value: width },
                enabled: true,
            }
        })
        .collect();
    EqProfile {
        layout,
        bands,
        ..Default::default()
    }
}
fn builtin(id: &str) -> Option<SavedPreset> {
    let (id, name, gains) = BUILTINS.iter().find(|(key, _, _)| *key == id)?;
    let mut profile = template(BandLayout::Classic10);
    // Existing presets keep their original gain; the new boost preset starts with headroom.
    profile.automatic_preamp = *id == "full-bass-treble";
    for (filter, gain) in profile.bands.iter_mut().zip(gains) {
        filter.gain_db = *gain;
    }
    Some(SavedPreset {
        id: (*id).into(),
        name: (*name).into(),
        profile,
    })
}
impl EqualizerPreferences {
    fn preset(&self, id: &str) -> Option<SavedPreset> {
        builtin(id).or_else(|| self.presets.iter().find(|p| p.id == id).cloned())
    }
    fn output(&self, id: &str) -> &OutputEq {
        self.outputs.get(id).unwrap_or(&self.default_output)
    }
    fn output_mut(&mut self, id: &str) -> &mut OutputEq {
        self.outputs.get_mut(id).unwrap_or(&mut self.default_output)
    }
    fn profile(&self, entry: &OutputEq) -> EqProfile {
        entry
            .selection
            .as_deref()
            .and_then(|id| self.preset(id))
            .map(|p| p.profile)
            .unwrap_or_else(|| entry.draft.profile.clone())
    }
    fn config(&self, output: &str) -> ProcessingConfig {
        let entry = self.output(output);
        ProcessingConfig {
            equalizer: entry.enabled.then(|| self.profile(entry)),
            crossfeed: entry.crossfeed,
            balance: entry.balance,
            mono: entry.mono,
        }
    }
    fn begin_edit(&mut self, output: &str) {
        let entry = self.output(output);
        if let Some(id) = entry.selection.clone() {
            let profile = self.profile(entry);
            let save_target = Some(id);
            let entry = self.output_mut(output);
            entry.selection = None;
            entry.draft = EqDraft {
                profile,
                save_target,
            };
        }
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        for p in &self.presets {
            p.profile.validate()?;
            if p.name.trim().is_empty()
                || p.id.is_empty()
                || builtin(&p.id).is_some()
                || !ids.insert(&p.id)
            {
                return Err("Invalid equalizer preset identity".into());
            }
        }
        for entry in std::iter::once(&self.default_output).chain(self.outputs.values()) {
            entry.draft.profile.validate()?;
            if let Some(id) = &entry.selection
                && self.preset(id).is_none()
            {
                return Err("Equalizer preset does not exist".into());
            }
            ProcessingConfig {
                equalizer: None,
                crossfeed: entry.crossfeed,
                balance: entry.balance,
                mono: entry.mono,
            }
            .validate()?;
        }
        Ok(())
    }
    pub(crate) fn from_legacy(raw: &serde_json::Value) -> Self {
        let mut preferences = Self::default();
        let mut profile = template(BandLayout::Classic10);
        profile.automatic_preamp = false;
        let bands = raw.get("bands").and_then(|v| v.as_array());
        for (index, filter) in profile.bands.iter_mut().enumerate() {
            filter.gain_db = bands
                .and_then(|b| b.get(index))
                .and_then(|v| v.as_f64())
                .filter(|v| v.is_finite())
                .unwrap_or(0.)
                .clamp(-12., 12.);
        }
        let name = raw
            .get("selected_preset")
            .and_then(|v| v.as_str())
            .unwrap_or("Custom");
        let selected = BUILTINS
            .iter()
            .find(|(id, title, _)| {
                *title == name && builtin(id).is_some_and(|p| p.profile == profile)
            })
            .map(|(id, _, _)| (*id).into());
        preferences.default_output.enabled = raw
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        preferences.default_output.selection = selected;
        if preferences.default_output.selection.is_none() {
            let id = identity();
            preferences.presets.push(SavedPreset {
                id: id.clone(),
                name: "Imported custom".into(),
                profile: profile.clone(),
            });
            preferences.default_output.draft = EqDraft {
                profile,
                save_target: Some(id),
            };
        } else {
            preferences.default_output.draft = EqDraft {
                profile,
                save_target: None,
            };
        }
        preferences
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct EqSubscription {
    owner: Weak<PlaybackOwner>,
    id: u64,
    first: AtomicBool,
    receiver: tokio::sync::Mutex<watch::Receiver<EqView>>,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl EqSubscription {
    pub async fn next(&self) -> Result<EqView, PlaybackError> {
        let mut rx = self.receiver.lock().await;
        if !self.first.swap(false, Ordering::AcqRel) {
            rx.changed()
                .await
                .map_err(|_| error("Equalizer subscription closed"))?;
        }
        let value = rx.borrow_and_update().clone();
        Ok(value)
    }
    pub fn set_measurement(&self, meter: bool, spectrum: bool) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(o) = state.observers.iter_mut().find(|o| o.id == self.id) {
                o.meter = meter;
                o.spectrum = spectrum;
            }
            state.refresh_measurement();
        }
    }
}
impl Drop for EqSubscription {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            state.observers.retain(|o| o.id != self.id);
            state.refresh_measurement();
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl PlaybackOwner {
    pub fn observe_local_output(&self, route: Option<String>) {
        let selection = self.output.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        if state.observed_route == route {
            return;
        }
        state.observed_route = route;
        if !selection.selected.is_local() {
            return;
        }
        let output = output_id(
            &selection.selected,
            state
                .local_device
                .as_deref()
                .or(state.observed_route.as_deref()),
        );
        state.current_output = output.clone();
        state.submit(&output, false);
        state.publish(&self.settings);
    }
    pub fn equalizer_output(&self) -> String {
        self.equalizer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current_output
            .clone()
    }
    pub fn equalizer_view(&self, output: String) -> EqView {
        self.equalizer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .view(&output, &self.settings)
    }
    pub fn subscribe_equalizer(self: Arc<Self>, output: String) -> Arc<EqSubscription> {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        let view = state.view(&output, &self.settings);
        let (sender, receiver) = watch::channel(view);
        let id = state.next_observer;
        state.next_observer += 1;
        state.observers.push(Observer {
            id,
            output,
            sender,
            meter: false,
            spectrum: false,
        });
        Arc::new(EqSubscription {
            owner: Arc::downgrade(&self),
            id,
            first: AtomicBool::new(true),
            receiver: tokio::sync::Mutex::new(receiver),
        })
    }
    pub fn equalizer_response(&self, output: String, frequencies: Vec<f64>) -> Vec<f64> {
        let view = self.equalizer_view(output);
        if !view.enabled {
            return vec![0.0; frequencies.len()];
        }
        audio_processing::effects::response_curve_db(
            &view.profile,
            view.sample_rate.unwrap_or(48_000),
            &frequencies,
        )
    }
    pub fn equalizer_presets(&self) -> Vec<EqPresetSummary> {
        let state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        BUILTINS
            .iter()
            .map(|(id, name, _)| EqPresetSummary {
                id: (*id).into(),
                name: (*name).into(),
                builtin: true,
            })
            .chain(state.preferences.presets.iter().map(|p| EqPresetSummary {
                id: p.id.clone(),
                name: p.name.clone(),
                builtin: false,
            }))
            .collect()
    }
    pub fn preview_equalizer(&self, output: String, edit: EqEdit) -> Result<(), PlaybackError> {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        let output = state.resolve(&output);
        if !output_available(&output) {
            return Err(error("Equalizer is controlled by the receiver"));
        }
        let mut entry = state.preferences.output(&output).clone();
        if let EqEdit::Layout { value } = &edit
            && state.preferences.profile(&entry).layout == *value
        {
            return Ok(());
        }
        let discrete = matches!(edit, EqEdit::Enabled { .. } | EqEdit::CastFormat { .. });
        let is_profile = !matches!(
            edit,
            EqEdit::Enabled { .. }
                | EqEdit::Crossfeed { .. }
                | EqEdit::Balance { .. }
                | EqEdit::Mono { .. }
                | EqEdit::CastFormat { .. }
        );
        if is_profile && entry.selection.is_some() {
            entry.draft = EqDraft {
                profile: state.preferences.profile(&entry),
                save_target: entry.selection.clone(),
            };
            entry.selection = None;
        }
        let rate = state
            .facts
            .get(&output)
            .and_then(|facts| facts.sample_rate)
            .unwrap_or(48_000);
        let profile = &mut entry.draft.profile;
        match edit {
            EqEdit::Enabled { value } => entry.enabled = value,
            EqEdit::Crossfeed { value } => entry.crossfeed = value,
            EqEdit::Balance { value } => entry.balance = value,
            EqEdit::Mono { value } => entry.mono = value,
            EqEdit::CastFormat { value } => entry.cast_format = value,
            EqEdit::BandGain { index, value } => band(profile, index)?.gain_db = value,
            EqEdit::BandFrequency { index, value } => {
                band(profile, index)?.frequency_hz = value;
                profile.layout = BandLayout::Parametric;
            }
            EqEdit::BandWidth { index, value } => {
                band(profile, index)?.width = value;
                profile.layout = BandLayout::Parametric;
            }
            EqEdit::BandWidthUnit { index, q } => {
                let filter = band(profile, index)?;
                filter.width = if q {
                    FilterWidth::Q {
                        value: audio_processing::effects::filter_q(filter, rate).ok_or_else(
                            || error("Cannot convert filter width at this sample rate"),
                        )?,
                    }
                } else {
                    FilterWidth::Hertz {
                        value: audio_processing::effects::native_bandwidth_hz(filter, rate),
                    }
                };
                profile.layout = BandLayout::Parametric;
            }
            EqEdit::BandKind { index, value } => {
                band(profile, index)?.kind = value;
                profile.layout = BandLayout::Parametric;
            }
            EqEdit::BandEnabled { index, value } => band(profile, index)?.enabled = value,
            EqEdit::AddBand => {
                profile.bands.push(Filter {
                    kind: FilterKind::Peak,
                    frequency_hz: 1000.,
                    gain_db: 0.,
                    width: FilterWidth::Q { value: 1. },
                    enabled: true,
                });
                profile.layout = BandLayout::Parametric;
            }
            EqEdit::RemoveBand { index } => {
                band(profile, index)?;
                profile.bands.remove(index as usize);
                profile.layout = BandLayout::Parametric;
            }
            EqEdit::Layout { value } => {
                let needs_copy = profile.bands.iter().any(|band| band.gain_db != 0.)
                    && !state
                        .preferences
                        .presets
                        .iter()
                        .any(|preset| preset.profile == *profile)
                    && !BUILTINS.iter().any(|(id, _, _)| {
                        builtin(id).is_some_and(|preset| preset.profile == *profile)
                    });
                if needs_copy {
                    state.preferences.presets.push(SavedPreset {
                        id: identity(),
                        name: "Custom copy".into(),
                        profile: profile.clone(),
                    });
                }
                profile.layout = value;
                profile.bands = template(value).bands;
            }
            EqEdit::Preamp { value } => {
                profile.manual_preamp_db = value;
                profile.automatic_preamp = false;
            }
            EqEdit::AutomaticPreamp { value } => profile.automatic_preamp = value,
            EqEdit::Bass { value } => profile.bass_db = value,
            EqEdit::Treble { value } => profile.treble_db = value,
        }
        entry.draft.profile.validate().map_err(error)?;
        ProcessingConfig {
            equalizer: None,
            crossfeed: entry.crossfeed,
            balance: entry.balance,
            mono: entry.mono,
        }
        .validate()
        .map_err(error)?;
        *state.preferences.output_mut(&output) = entry;
        if !state.current_output.starts_with("remote:") || discrete {
            state.submit(&output, false);
        }
        state.publish(&self.settings);
        Ok(())
    }
    pub async fn commit_equalizer(&self, output: String) -> Result<(), PlaybackError> {
        let receiver = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let _ = state.resolve(&output);
            self.queue_equalizer_write(&mut state)?
        };
        receiver
            .await
            .map_err(|_| error("Equalizer save stopped"))?
            .map_err(error)
    }
    pub async fn select_equalizer_preset(
        &self,
        output: String,
        preset: Option<String>,
    ) -> Result<(), PlaybackError> {
        let done = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let output = state.resolve(&output);
            if let Some(id) = &preset
                && state.preferences.preset(id).is_none()
            {
                return Err(error("Preset does not exist"));
            }
            state.preferences.output_mut(&output).selection = preset;
            state.submit(&output, false);
            state.publish(&self.settings);
            self.queue_equalizer_write(&mut state)?
        };
        await_write(done).await
    }
    pub async fn save_equalizer_preset(
        &self,
        output: String,
        name: Option<String>,
    ) -> Result<String, PlaybackError> {
        let name = name.map(|name| checked_name(&name)).transpose()?;
        let (id, done) = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let output = state.resolve(&output);
            let active = state.current_output.clone();
            let previous_processing = state.preferences.config(&active);
            state.preferences.begin_edit(&output);
            let draft = state.preferences.output(&output).draft.clone();
            let id = match draft.save_target.filter(|id| builtin(id).is_none()) {
                Some(id) => {
                    let p = state
                        .preferences
                        .presets
                        .iter_mut()
                        .find(|p| p.id == id)
                        .ok_or_else(|| error("Preset does not exist"))?;
                    p.profile = draft.profile;
                    if let Some(name) = name {
                        p.name = checked_name(&name)?;
                    }
                    id
                }
                None => {
                    let id = identity();
                    let name = checked_name(name.as_deref().unwrap_or("Custom"))?;
                    state.preferences.presets.push(SavedPreset {
                        id: id.clone(),
                        name,
                        profile: draft.profile,
                    });
                    id
                }
            };
            state.preferences.output_mut(&output).draft.save_target = Some(id.clone());
            state.preferences.output_mut(&output).selection = Some(id.clone());
            if !previous_processing.same_processing(&state.preferences.config(&active)) {
                state.submit(&active, false);
            }
            state.publish(&self.settings);
            let done = self.queue_equalizer_write(&mut state)?;
            (id, done)
        };
        await_write(done).await?;
        Ok(id)
    }
    pub async fn duplicate_equalizer_preset(
        &self,
        output: String,
        name: String,
    ) -> Result<String, PlaybackError> {
        let name = checked_name(&name)?;
        let (id, done) = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let output = state.resolve(&output);
            let profile = state.preferences.profile(state.preferences.output(&output));
            let id = identity();
            state.preferences.presets.push(SavedPreset {
                id: id.clone(),
                name,
                profile,
            });
            state.preferences.output_mut(&output).selection = Some(id.clone());
            state.publish(&self.settings);
            let done = self.queue_equalizer_write(&mut state)?;
            (id, done)
        };
        await_write(done).await?;
        Ok(id)
    }
    pub async fn rename_equalizer_preset(
        &self,
        id: String,
        name: String,
    ) -> Result<(), PlaybackError> {
        let name = checked_name(&name)?;
        let done = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            state
                .preferences
                .presets
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| error("Choose a custom preset"))?
                .name = name;
            state.publish(&self.settings);
            self.queue_equalizer_write(&mut state)?
        };
        await_write(done).await
    }
    pub async fn delete_equalizer_preset(&self, id: String) -> Result<(), PlaybackError> {
        let done = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let preset = state
                .preferences
                .presets
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or_else(|| error("Choose a custom preset"))?;
            let prefs = &mut state.preferences;
            for entry in
                std::iter::once(&mut prefs.default_output).chain(prefs.outputs.values_mut())
            {
                if entry.selection.as_deref() == Some(&id) {
                    entry.selection = None;
                    entry.draft.profile = preset.profile.clone();
                    entry.draft.save_target = None;
                }
                if entry.draft.save_target.as_deref() == Some(&id) {
                    entry.draft.save_target = None;
                }
            }
            prefs.presets.retain(|p| p.id != id);
            state.publish(&self.settings);
            self.queue_equalizer_write(&mut state)?
        };
        await_write(done).await
    }
    pub async fn assign_equalizer(
        &self,
        output: String,
        assigned: bool,
    ) -> Result<(), PlaybackError> {
        let done = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let output = state.resolve(&output);
            if assigned {
                let entry = state.preferences.default_output.clone();
                state
                    .preferences
                    .outputs
                    .entry(output.clone())
                    .or_insert(entry);
            } else {
                state.preferences.outputs.remove(&output);
            }
            state.submit(&output, false);
            state.publish(&self.settings);
            self.queue_equalizer_write(&mut state)?
        };
        await_write(done).await
    }
    pub fn apply_equalizer(&self, output: String) -> Result<(), PlaybackError> {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        let output = state.resolve(&output);
        if let Some(f) = state.facts.get_mut(&output) {
            f.error = None;
        }
        state.submit(&output, true);
        state.publish(&self.settings);
        Ok(())
    }
    pub async fn import_equalizer(
        &self,
        output: String,
        text: String,
        format: EqPresetFormat,
    ) -> Result<String, PlaybackError> {
        let (name, profile) = parse_preset(&text, format).map_err(error)?;
        profile.validate().map_err(error)?;
        let (id, done) = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            let output = state.resolve(&output);
            let id = identity();
            state.preferences.presets.push(SavedPreset {
                id: id.clone(),
                name,
                profile,
            });
            state.preferences.output_mut(&output).selection = Some(id.clone());
            state.submit(&output, false);
            state.publish(&self.settings);
            let done = self.queue_equalizer_write(&mut state)?;
            (id, done)
        };
        await_write(done).await?;
        Ok(id)
    }
    pub fn export_equalizer(
        &self,
        output: String,
        format: EqPresetFormat,
    ) -> Result<String, PlaybackError> {
        let view = self.equalizer_view(output);
        match format {
            EqPresetFormat::Json => serde_json::to_string_pretty(&PresetDocument {
                version: 1,
                name: view.name,
                profile: view.profile,
            })
            .map_err(|e| error(e.to_string())),
            EqPresetFormat::Apo => {
                export_apo(&view.profile, view.sample_rate.unwrap_or(48000)).map_err(error)
            }
        }
    }
}
impl PlaybackOwner {
    fn queue_equalizer_write(
        &self,
        state: &mut EqualizerState,
    ) -> Result<oneshot::Receiver<Result<(), String>>, PlaybackError> {
        let (reply, done) = oneshot::channel();
        state.write_error = None;
        self.store_sender
            .try_send(PlaybackStoreWork::Equalizer {
                snapshot: state.preferences.clone(),
                reply,
            })
            .map_err(|_| error("Equalizer save stopped"))?;
        Ok(done)
    }
    pub(crate) async fn restore_equalizer(
        &self,
        preferences: EqualizerPreferences,
    ) -> Result<(), PlaybackError> {
        preferences.validate().map_err(error)?;
        let done = {
            let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
            state.preferences = preferences;
            let output = state.current_output.clone();
            state.submit(&output, false);
            state.publish(&self.settings);
            self.queue_equalizer_write(&mut state)?
        };
        await_write(done).await
    }
    pub(super) fn processing_receiver(
        &self,
        output: &PlaybackOutput,
    ) -> watch::Receiver<ProcessingRequest> {
        let device = self.settings.load().ui.playback.audio_output;
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        state.local_device = device;
        let id = output_id(
            output,
            state
                .local_device
                .as_deref()
                .or(state.observed_route.as_deref()),
        );
        let (tx, rx) = watch::channel(state.request(&id));
        state.processing.retain(|_, tx| !tx.is_closed());
        state.processing.insert(backend_key(&id), tx);
        rx
    }
    pub(super) fn equalizer_output_changed(&self, local: Option<&str>) {
        let selection = self.output.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        let id = output_id(
            &selection.selected,
            local.or(state.observed_route.as_deref()),
        );
        let changed = state.current_output != id
            || (selection.selected.is_local() && state.local_device.as_deref() != local);
        state.local_device = local.map(str::to_string);
        state.current_output = id.clone();
        if changed {
            state.submit(&id, false);
        }
        state.publish(&self.settings);
    }
    pub(super) fn equalizer_applied(
        &self,
        output: String,
        config: ProcessingConfig,
        sample_rate: Option<u32>,
        preamp_db: f64,
    ) {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        let matches_request = state
            .processing
            .get(&backend_key(&output))
            .is_some_and(|sender| sender.borrow().config.same_processing(&config));
        let facts = state.facts.entry(output).or_default();
        facts.config = Some(config);
        facts.sample_rate = sample_rate;
        facts.preamp_db = preamp_db;
        if matches_request {
            facts.error = None;
        }
        state.publish(&self.settings);
    }
    pub(super) fn equalizer_failed(&self, output: String, message: String) {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        state.facts.entry(output).or_default().error = Some(message);
        state.publish(&self.settings);
    }
    pub(super) fn equalizer_meter(&self, output: String, peak_db: Vec<f64>, spectrum_db: Vec<f64>) {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        let facts = state.facts.entry(output).or_default();
        if !peak_db.is_empty() {
            facts.peak_db = peak_db;
        }
        if !spectrum_db.is_empty() {
            facts.spectrum_db = spectrum_db;
        }
        state.publish(&self.settings);
    }
    pub(super) fn equalizer_write_finished(&self, result: &Result<(), String>) {
        let mut state = self.equalizer.lock().unwrap_or_else(|e| e.into_inner());
        state.write_error = result.as_ref().err().cloned();
        state.publish(&self.settings);
    }
}
fn band(profile: &mut EqProfile, index: u32) -> Result<&mut Filter, PlaybackError> {
    profile
        .bands
        .get_mut(index as usize)
        .ok_or_else(|| error("Band does not exist"))
}
async fn await_write(done: oneshot::Receiver<Result<(), String>>) -> Result<(), PlaybackError> {
    done.await
        .map_err(|_| error("Equalizer save stopped"))?
        .map_err(error)
}

#[derive(Serialize, Deserialize)]
struct PresetDocument {
    version: u32,
    name: String,
    profile: EqProfile,
}
fn parse_preset(text: &str, format: EqPresetFormat) -> Result<(String, EqProfile), String> {
    match format {
        EqPresetFormat::Json => {
            let doc: PresetDocument = serde_json::from_str(text).map_err(|e| e.to_string())?;
            if doc.version != 1 {
                return Err("Unsupported equalizer file version".into());
            }
            if doc.name.trim().is_empty() {
                return Err("Preset name is empty".into());
            }
            Ok((doc.name, doc.profile))
        }
        EqPresetFormat::Apo => {
            let mut profile = EqProfile {
                automatic_preamp: false,
                ..Default::default()
            };
            for (index, line) in text.lines().enumerate() {
                let line = line.split('#').next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let words: Vec<_> = line.split_whitespace().collect();
                let fail = || format!("Unsupported equalizer directive on line {}", index + 1);
                if words[0].eq_ignore_ascii_case("Preamp:") {
                    if words.len() != 3 || !words[2].eq_ignore_ascii_case("dB") {
                        return Err(fail());
                    }
                    profile.manual_preamp_db = words[1].parse().map_err(|_| fail())?;
                } else if words[0].eq_ignore_ascii_case("Filter") {
                    if words.len() != 12
                        || !words[1].ends_with(':')
                        || !words[4].eq_ignore_ascii_case("Fc")
                        || !words[6].eq_ignore_ascii_case("Hz")
                        || !words[7].eq_ignore_ascii_case("Gain")
                        || !words[9].eq_ignore_ascii_case("dB")
                        || !words[10].eq_ignore_ascii_case("Q")
                    {
                        return Err(fail());
                    }
                    let enabled = match words[2] {
                        "ON" => true,
                        "OFF" => false,
                        _ => return Err(fail()),
                    };
                    let kind = match words[3] {
                        "PK" => FilterKind::Peak,
                        "LSC" => FilterKind::LowShelf,
                        "HSC" => FilterKind::HighShelf,
                        _ => return Err(fail()),
                    };
                    profile.bands.push(Filter {
                        enabled,
                        kind,
                        frequency_hz: words[5].parse().map_err(|_| fail())?,
                        gain_db: words[8].parse().map_err(|_| fail())?,
                        width: FilterWidth::Q {
                            value: words[11].parse().map_err(|_| fail())?,
                        },
                    });
                } else {
                    return Err(fail());
                }
            }
            profile.validate()?;
            Ok(("Imported preset".into(), profile))
        }
    }
}
fn export_apo(profile: &EqProfile, rate: u32) -> Result<String, String> {
    let mut bands = profile.bands.clone();
    for (kind, freq, gain) in [
        (FilterKind::LowShelf, 100., profile.bass_db),
        (FilterKind::HighShelf, 10000., profile.treble_db),
    ] {
        if gain != 0. {
            bands.push(Filter {
                kind,
                frequency_hz: freq,
                gain_db: gain,
                width: FilterWidth::Q {
                    value: std::f64::consts::FRAC_1_SQRT_2,
                },
                enabled: true,
            });
        }
    }
    let preamp = audio_processing::effects::effective_preamp_db(profile, rate);
    let mut text = format!("# Reference sample rate: {rate} Hz\nPreamp: {preamp:.4} dB\n");
    for (i, filter) in bands.iter().enumerate() {
        let q = audio_processing::effects::filter_q(filter, rate)
            .ok_or("This filter cannot be exported at the current sample rate; use Rufin JSON")?;
        let kind = match filter.kind {
            FilterKind::Peak => "PK",
            FilterKind::LowShelf => "LSC",
            FilterKind::HighShelf => "HSC",
        };
        let enabled = if filter.enabled { "ON" } else { "OFF" };
        text.push_str(&format!(
            "Filter {}: {enabled} {kind} Fc {:.4} Hz Gain {:.4} dB Q {q:.8}\n",
            i + 1,
            filter.frequency_hz,
            filter.gain_db
        ));
    }
    Ok(text)
}
