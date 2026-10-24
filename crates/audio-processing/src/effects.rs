//! Shared native effects and the response of the filters they execute.

use gstreamer as gst;
use gstreamer_audio as audio;
use gstreamer_controller::{self as controller, prelude::*};
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FilterKind {
    Peak,
    LowShelf,
    HighShelf,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FilterWidth {
    Q { value: f64 },
    Hertz { value: f64 },
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum BandLayout {
    #[default]
    Classic10,
    Bands18,
    Bands31,
    Parametric,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Filter {
    pub kind: FilterKind,
    pub frequency_hz: f64,
    pub gain_db: f64,
    pub width: FilterWidth,
    pub enabled: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EqProfile {
    pub layout: BandLayout,
    pub bands: Vec<Filter>,
    pub manual_preamp_db: f64,
    pub automatic_preamp: bool,
    pub bass_db: f64,
    pub treble_db: f64,
}
impl Default for EqProfile {
    fn default() -> Self {
        Self {
            layout: BandLayout::Parametric,
            bands: Vec::new(),
            manual_preamp_db: 0.0,
            automatic_preamp: true,
            bass_db: 0.0,
            treble_db: 0.0,
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProcessingConfig {
    pub equalizer: Option<EqProfile>,
    pub crossfeed: bool,
    pub balance: f64,
    pub mono: bool,
}
fn range(value: f64, min: f64, max: f64, name: &str) -> Result<(), String> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be between {min} and {max}"))
    }
}
impl Filter {
    pub fn validate(&self) -> Result<(), String> {
        range(self.frequency_hz, f64::MIN_POSITIVE, 100_000.0, "Frequency")?;
        range(self.gain_db, -24.0, 12.0, "Band gain")?;
        match self.width {
            FilterWidth::Q { value } if value.is_finite() && value > 0.0 => Ok(()),
            FilterWidth::Hertz { value } => range(value, f64::MIN_POSITIVE, 100_000.0, "Bandwidth"),
            _ => Err("Q must be finite and greater than zero".into()),
        }
    }
}
impl EqProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self.bands.len() > 64 {
            return Err("An equalizer can have at most 64 bands".into());
        }
        for band in &self.bands {
            band.validate()?;
        }
        range(self.manual_preamp_db, -36.0, 12.0, "Preamp")?;
        range(self.bass_db, -12.0, 12.0, "Bass")?;
        range(self.treble_db, -12.0, 12.0, "Treble")
    }
}
impl ProcessingConfig {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(profile) = &self.equalizer {
            profile.validate()?;
        }
        range(self.balance, -1.0, 1.0, "Balance")
    }
    pub fn is_neutral(&self) -> bool {
        !self.crossfeed
            && !self.mono
            && self.balance == 0.0
            && self.equalizer.as_ref().is_none_or(|p| {
                p.bass_db == 0.0
                    && p.treble_db == 0.0
                    && (p.automatic_preamp || p.manual_preamp_db == 0.0)
                    && p.bands.iter().all(|b| !b.enabled || b.gain_db == 0.0)
            })
    }
    pub fn same_processing(&self, other: &Self) -> bool {
        let mut a = self.clone();
        let mut b = other.clone();
        if let Some(p) = &mut a.equalizer {
            p.layout = BandLayout::Parametric;
        }
        if let Some(p) = &mut b.equalizer {
            p.layout = BandLayout::Parametric;
        }
        a == b
    }
}
pub fn native_bandwidth_hz(filter: &Filter, sample_rate: u32) -> f64 {
    let rate = f64::from(sample_rate);
    match filter.width {
        FilterWidth::Hertz { value } => value,
        FilterWidth::Q { value } => {
            rate / PI * ((2.0 * PI * filter.frequency_hz / rate).sin() / (2.0 * value)).atan()
        }
    }
}
pub fn filter_q(filter: &Filter, sample_rate: u32) -> Option<f64> {
    if sample_rate == 0 || filter.frequency_hz >= f64::from(sample_rate) / 2.0 {
        return None;
    }
    let q = match filter.width {
        FilterWidth::Q { value } => value,
        FilterWidth::Hertz { value } => {
            if value >= f64::from(sample_rate) / 2.0 {
                return None;
            }
            (2.0 * PI * filter.frequency_hz / f64::from(sample_rate)).sin()
                / (2.0 * (PI * value / f64::from(sample_rate)).tan())
        }
    };
    (q.is_finite() && q > 0.0).then_some(q)
}
fn tone(profile: &EqProfile) -> [Filter; 2] {
    [
        (FilterKind::LowShelf, 100.0, profile.bass_db),
        (FilterKind::HighShelf, 10_000.0, profile.treble_db),
    ]
    .map(|(kind, frequency_hz, gain_db)| Filter {
        kind,
        frequency_hz,
        gain_db,
        width: FilterWidth::Q {
            value: std::f64::consts::FRAC_1_SQRT_2,
        },
        enabled: true,
    })
}
#[derive(Clone, Copy)]
struct Coefficients {
    numerator: [f64; 3],
    denominator: [f64; 3],
}
impl Coefficients {
    fn new(filter: &Filter, rate: u32) -> Option<Self> {
        if !filter.enabled || filter.gain_db == 0.0 || filter.frequency_hz >= f64::from(rate) / 2.0
        {
            return None;
        }
        let a = 10_f64.powf(filter.gain_db / 40.0);
        let c = (2.0 * PI * filter.frequency_hz / f64::from(rate)).cos();
        let width = (2.0 * PI * native_bandwidth_hz(filter, rate) / f64::from(rate)).min(PI - 1e-8);
        let alpha = (width / 2.0).tan();
        let p = a + 1.0;
        let m = a - 1.0;
        let d = 2.0 * a.sqrt() * alpha;
        let (numerator, denominator) = match filter.kind {
            FilterKind::Peak => (
                [1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a],
                [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a],
            ),
            FilterKind::LowShelf => (
                [
                    a * (p - m * c + d),
                    2.0 * a * (m - p * c),
                    a * (p - m * c - d),
                ],
                [p + m * c + d, -2.0 * (m + p * c), p + m * c - d],
            ),
            FilterKind::HighShelf => (
                [
                    a * (p + m * c + d),
                    -2.0 * a * (m + p * c),
                    a * (p + m * c - d),
                ],
                [p - m * c + d, 2.0 * (m - p * c), p - m * c - d],
            ),
        };
        Some(Self {
            numerator,
            denominator,
        })
    }
    fn db(&self, omega: f64) -> f64 {
        let power = |v: [f64; 3]| {
            let re = v[0] + v[1] * omega.cos() + v[2] * (2.0 * omega).cos();
            let im = -v[1] * omega.sin() - v[2] * (2.0 * omega).sin();
            re * re + im * im
        };
        10.0 * (power(self.numerator) / power(self.denominator)).log10()
    }
}
fn coefficients(profile: &EqProfile, rate: u32) -> Vec<Coefficients> {
    profile
        .bands
        .iter()
        .chain(tone(profile).iter())
        .filter_map(|f| Coefficients::new(f, rate))
        .collect()
}
fn response(coefficients: &[Coefficients], rate: u32, frequency: f64) -> f64 {
    coefficients
        .iter()
        .map(|c| c.db(2.0 * PI * frequency / f64::from(rate)))
        .sum()
}
pub fn effective_preamp_db(profile: &EqProfile, rate: u32) -> f64 {
    if !profile.automatic_preamp {
        return profile.manual_preamp_db;
    }
    if rate == 0 {
        return 0.0;
    }
    let filters = coefficients(profile, rate);
    if filters.is_empty() {
        return 0.0;
    }
    let nyquist = f64::from(rate) / 2.0;
    let low = 0.01_f64.min(nyquist);
    let mut points = (0..2048)
        .map(|i| low * (nyquist / low).powf(i as f64 / 2047.0))
        .collect::<Vec<_>>();
    points.extend([0.0, nyquist]);
    for band in profile
        .bands
        .iter()
        .chain(tone(profile).iter())
        .filter(|b| b.enabled && b.frequency_hz < nyquist)
    {
        points.push(band.frequency_hz);
        for offset in [0.125, 0.25, 0.5, 1.0, 2.0] {
            let width = native_bandwidth_hz(band, rate) * offset;
            points.extend([
                (band.frequency_hz - width).max(0.0),
                (band.frequency_hz + width).min(nyquist),
            ]);
        }
    }
    points.sort_by(f64::total_cmp);
    points.dedup();
    let values = points
        .iter()
        .map(|f| response(&filters, rate, *f))
        .collect::<Vec<_>>();
    let mut maximum = values.iter().copied().fold(0.0, f64::max);
    for i in 1..points.len() - 1 {
        if values[i] < values[i - 1] || values[i] < values[i + 1] {
            continue;
        }
        let (mut lo, mut hi) = (points[i - 1], points[i + 1]);
        let mut previous = values[i];
        for _ in 0..32 {
            let a = lo + (hi - lo) / 3.0;
            let b = hi - (hi - lo) / 3.0;
            let va = response(&filters, rate, a);
            let vb = response(&filters, rate, b);
            maximum = maximum.max(va).max(vb);
            if va < vb {
                lo = a;
            } else {
                hi = b;
            }
            let best = va.max(vb);
            if (best - previous).abs() < 0.001 && (hi - lo) < 0.01 {
                break;
            }
            previous = best;
        }
    }
    -(maximum.max(0.0) * 10.0).ceil() / 10.0
}
/// Combined EQ and tone response, including the selected preamp.
pub fn response_db(profile: &EqProfile, rate: u32, frequency: f64) -> f64 {
    if rate == 0 {
        return 0.0;
    }
    response(&coefficients(profile, rate), rate, frequency) + effective_preamp_db(profile, rate)
}
/// Evaluate a whole plot with one headroom search.
pub fn response_curve_db(profile: &EqProfile, rate: u32, frequencies: &[f64]) -> Vec<f64> {
    if rate == 0 {
        return vec![0.0; frequencies.len()];
    }
    let filters = coefficients(profile, rate);
    let preamp = effective_preamp_db(profile, rate);
    frequencies
        .iter()
        .map(|f| response(&filters, rate, *f) + preamp)
        .collect()
}

#[derive(Clone)]
struct Control {
    object: gst::Object,
    property: &'static str,
    curve: controller::InterpolationControlSource,
    target: f64,
}
impl Control {
    fn new(object: &gst::Object, property: &'static str, target: f64) -> Result<Self, String> {
        if property != "gain" && property != "volume" {
            object.set_property(property, target);
        }
        let curve = controller::InterpolationControlSource::new();
        curve.set_mode(controller::InterpolationMode::Linear);
        let binding = controller::DirectControlBinding::new_absolute(object, property, &curve);
        object
            .add_control_binding(&binding)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            object: object.clone(),
            property,
            curve,
            target,
        })
    }
    fn set(&mut self, time: gst::ClockTime, target: f64, ramp: bool, delay: bool) {
        let current = ControlSourceExt::value(&self.curve, time)
            .unwrap_or_else(|| self.object.property::<f64>(self.property));
        self.curve.unset_all();
        if ramp && current != target {
            self.curve.set(time, current);
            let start = time
                + if delay {
                    gst::ClockTime::from_mseconds(50)
                } else {
                    gst::ClockTime::ZERO
                };
            if delay {
                self.curve.set(start, current);
            }
            self.curve
                .set(start + gst::ClockTime::from_mseconds(50), target);
        } else {
            self.curve.set(time, target);
        }
        self.target = target;
    }
    fn rebase(&self, previous: gst::ClockTime, start: gst::ClockTime) {
        let current = ControlSourceExt::value(&self.curve, previous).unwrap_or(self.target);
        let remaining = self
            .curve
            .list_control_points()
            .last()
            .map_or(gst::ClockTime::ZERO, |p| {
                p.timestamp().saturating_sub(previous)
            });
        self.curve.unset_all();
        self.curve.set(start, current);
        self.curve.set(start + remaining, self.target);
    }
}
struct Prepared {
    config: ProcessingConfig,
    stages: [Option<gst::Element>; 5],
    controls: Vec<Control>,
    targets: Vec<f64>,
    preamp_db: f64,
    rate: Option<u32>,
    bank_shape: Vec<FilterKind>,
}
struct EffectsState {
    config: ProcessingConfig,
    bank_shape: Vec<FilterKind>,
    balance_curve: controller::InterpolationControlSource,
    last_balance: f64,
    stages: [Option<gst::Element>; 5],
    controls: Vec<Control>,
    info: Option<audio::AudioInfo>,
    prepared_rate: Option<u32>,
    pending: Option<Prepared>,
    applied: Option<(ProcessingConfig, Option<u32>, f64)>,
    frontier: gst::ClockTime,
    started: bool,
    block: Option<gst::PadProbeId>,
}
/// One chain shared by local playback and generated cast audio.
pub struct EffectsBin {
    root: gst::Element,
    input: gst::Pad,
    slots: [gst::Element; 5],
    channels: gst::Element,
    mono_caps: gst::Element,
    state: Arc<Mutex<EffectsState>>,
    requested: ProcessingConfig,
    meter: bool,
}
impl EffectsBin {
    pub fn validate_available(config: &ProcessingConfig) -> Result<(), String> {
        config.validate()?;
        if config.equalizer.is_some() && gst::ElementFactory::find("equalizer-nbands").is_none() {
            return Err("Equalizer is unavailable: equalizer-nbands is not installed".into());
        }
        if config.crossfeed && gst::ElementFactory::find("bs2b").is_none() {
            return Err("Crossfeed is unavailable: bs2b is not installed".into());
        }
        Ok(())
    }
    pub fn new(config: &ProcessingConfig) -> Result<Self, String> {
        Self::validate_available(config)?;
        let root = gst::Bin::new();
        let convert = element("audioconvert", "rufin-effects-convert")?;
        let floating = element("capsfilter", "rufin-effects-floating")?;
        floating.set_property(
            "caps",
            gst::Caps::builder("audio/x-raw")
                .field(
                    "format",
                    if cfg!(target_endian = "little") {
                        "F64LE"
                    } else {
                        "F64BE"
                    },
                )
                .field("layout", "interleaved")
                .build(),
        );
        let slots = [
            element("insertbin", "rufin-preamp-slot")?,
            element("insertbin", "rufin-eq-slot")?,
            element("insertbin", "rufin-tone-slot")?,
            element("insertbin", "rufin-crossfeed-slot")?,
            element("insertbin", "rufin-meter-slot")?,
        ];
        let channels = element("audioconvert", "rufin-channel-controls")?;
        let mono = element("audioconvert", "rufin-mono")?;
        let mono_caps = element("capsfilter", "rufin-mono-caps")?;
        let mut chain = vec![&convert, &floating];
        chain.extend(slots[..4].iter());
        chain.extend([&channels, &mono, &mono_caps, &slots[4]]);
        root.add_many(&chain).map_err(|e| e.to_string())?;
        gst::Element::link_many(&chain).map_err(|e| e.to_string())?;
        for (name, target) in [
            ("sink", convert.static_pad("sink").unwrap()),
            ("src", slots[4].static_pad("src").unwrap()),
        ] {
            let pad = gst::GhostPad::builder_with_target(&target)
                .map_err(|e| e.to_string())?
                .name(name)
                .build();
            pad.set_active(true).map_err(|e| e.to_string())?;
            root.add_pad(&pad).map_err(|e| e.to_string())?;
        }
        let state = Arc::new(Mutex::new(EffectsState {
            config: ProcessingConfig::default(),
            bank_shape: Vec::new(),
            balance_curve: {
                let curve = controller::InterpolationControlSource::new();
                curve.set_mode(controller::InterpolationMode::Linear);
                curve
            },
            last_balance: 0.0,
            stages: std::array::from_fn(|_| None),
            controls: Vec::new(),
            info: None,
            prepared_rate: None,
            pending: None,
            applied: None,
            frontier: gst::ClockTime::ZERO,
            started: false,
            block: None,
        }));
        let input = convert.static_pad("sink").unwrap();
        let weak = Arc::downgrade(&state);
        let probe_slots = slots.clone();
        let probe_channels = channels.clone();
        let probe_mono = mono_caps.clone();
        input.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
            move |pad, probe| {
                let Some(state) = weak.upgrade() else {
                    return gst::PadProbeReturn::Remove;
                };
                let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(event) = probe.event() {
                    match event.view() {
                        gst::EventView::Caps(caps) => {
                            if let Ok(info) = audio::AudioInfo::from_caps(caps.caps()) {
                                if state.info.as_ref() != Some(&info) {
                                    state.started = false;
                                    state.prepared_rate = None;
                                    // Match the matrix before converters negotiate the new channels.
                                    probe_channels.set_property(
                                        "mix-matrix",
                                        balance_matrix(&info, state.last_balance),
                                    );
                                }
                                if state.info.as_ref() != Some(&info)
                                    && state.block.is_none()
                                    && !state.config.is_neutral()
                                {
                                    state.block = pad.add_probe(
                                        gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER,
                                        |_, _| gst::PadProbeReturn::Ok,
                                    );
                                }
                                state.info = Some(info);
                                notify_changed(&probe_slots[0]);
                            }
                        }
                        gst::EventView::Segment(segment) => {
                            if let Some(segment) =
                                segment.segment().downcast_ref::<gst::ClockTime>()
                            {
                                let start = segment
                                    .position()
                                    .and_then(|p| segment.to_stream_time(p))
                                    .unwrap_or(gst::ClockTime::ZERO);
                                for control in &state.controls {
                                    control.rebase(state.frontier, start);
                                }
                                let current =
                                    ControlSourceExt::value(&state.balance_curve, state.frontier)
                                        .unwrap_or(state.last_balance);
                                let remaining = state
                                    .balance_curve
                                    .list_control_points()
                                    .last()
                                    .map_or(gst::ClockTime::ZERO, |p| {
                                        p.timestamp().saturating_sub(state.frontier)
                                    });
                                state.balance_curve.unset_all();
                                state.balance_curve.set(start, current);
                                state
                                    .balance_curve
                                    .set(start + remaining, state.config.balance);
                                state.frontier = start;
                            }
                        }
                        gst::EventView::FlushStop(_) => state.started = false,
                        _ => {}
                    }
                }
                if let Some(buffer) = probe.buffer() {
                    let time = pad
                        .sticky_event::<gst::event::Segment>(0)
                        .and_then(|s| {
                            s.segment()
                                .downcast_ref::<gst::ClockTime>()
                                .and_then(|s| buffer.pts().and_then(|p| s.to_stream_time(p)))
                        })
                        .unwrap_or(state.frontier);
                    if let Some(prepared) = state.pending.take() {
                        install(&mut state, prepared, &probe_slots, &probe_mono, time);
                    }
                    // Equalizer passthrough skips its own controller update. Synchronize before its dirty-coefficient check.
                    for control in &state.controls {
                        let _ = control.object.sync_values(time);
                    }
                    let balance = ControlSourceExt::value(&state.balance_curve, time)
                        .unwrap_or(state.config.balance);
                    if balance != state.last_balance
                        && let Some(info) = &state.info
                    {
                        probe_channels.set_property("mix-matrix", balance_matrix(info, balance));
                        state.last_balance = balance;
                    }
                    state.frontier = time + buffer.duration().unwrap_or(gst::ClockTime::ZERO);
                    state.started = true;
                }
                gst::PadProbeReturn::Ok
            },
        );
        let mut bin = Self {
            root: root.upcast(),
            input,
            slots,
            channels,
            mono_caps,
            state,
            requested: config.clone(),
            meter: false,
        };
        bin.update(config)?;
        Ok(bin)
    }
    pub fn element(&self) -> &gst::Element {
        &self.root
    }
    pub fn measurement_pad(&self) -> gst::Pad {
        self.root.static_pad("src").unwrap()
    }
    pub fn update(&mut self, config: &ProcessingConfig) -> Result<(), String> {
        Self::validate_available(config)?;
        self.prepare(config)?;
        self.requested = config.clone();
        Ok(())
    }
    pub fn reset(&mut self) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.info = None;
            state.started = false;
            state.frontier = gst::ClockTime::ZERO;
        }
        self.channels
            .set_property("mix-matrix", gst::Array::new([] as [f32; 0]));
        self.prepare(&self.requested.clone())
    }
    pub fn set_meter(&mut self, enabled: bool) -> Result<(), String> {
        if self.meter == enabled {
            return Ok(());
        }
        self.meter = enabled;
        self.prepare(&self.requested.clone())
    }
    fn prepare(&self, config: &ProcessingConfig) -> Result<(), String> {
        let (info, mut stages, mut controls, bank_shape) = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let (stages, controls, shape) = state
                .pending
                .as_ref()
                .map_or((&state.stages, &state.controls, &state.bank_shape), |p| {
                    (&p.stages, &p.controls, &p.bank_shape)
                });
            (
                state.info.clone(),
                stages.clone(),
                controls.clone(),
                shape.clone(),
            )
        };
        let rate = info.as_ref().map(audio::AudioInfo::rate);
        let processing_rate = rate.unwrap_or(48_000);
        if config.crossfeed
            && let Some(info) = &info
            && (info.channels() > 2
                || (info.channels() == 2 && !(2000..=384_000).contains(&info.rate())))
        {
            return Err(format!(
                "Crossfeed is unavailable for {} channels at {} Hz",
                info.channels(),
                info.rate()
            ));
        }
        let profile = config.equalizer.as_ref();
        let preamp_db = profile.map_or(0.0, |p| effective_preamp_db(p, processing_rate));
        // Keep a bypassed bank so enabling it again preserves its native filter history.
        if profile.is_some() && stages[0].is_none() {
            stages[0] = Some(element("volume", "rufin-eq-preamp")?);
        }
        if let Some(profile) = profile {
            let shape_changed =
                bank_shape != profile.bands.iter().map(|b| b.kind).collect::<Vec<_>>();
            if stages[1].is_none() || shape_changed {
                stages[1] = if profile.bands.is_empty() {
                    None
                } else {
                    Some(bank("rufin-equalizer", &profile.bands)?)
                };
            }
            if stages[2].is_none() {
                stages[2] = Some(bank("rufin-tone", &tone(profile))?);
            }
        }
        if config.crossfeed {
            if info.is_some() {
                if stages[3].is_none() {
                    stages[3] = Some(element("bs2b", "rufin-crossfeed")?);
                }
            } else {
                if gst::ElementFactory::find("bs2b").is_none() {
                    return Err("Crossfeed is unavailable: bs2b is not installed".into());
                }
                stages[3] = None;
            }
        } else {
            stages[3] = None;
        }
        if self.meter {
            if stages[4].is_none() {
                let level = element("level", "rufin-processing-level")?;
                level.set_property("interval", 100_000_000_u64);
                level.set_property("post-messages", true);
                stages[4] = Some(level);
            }
        } else {
            stages[4] = None;
        }
        let mut targets = Vec::new();
        let mut desired = Vec::new();
        if let Some(preamp) = &stages[0] {
            desired.push((
                preamp.clone().upcast::<gst::Object>(),
                "volume",
                10_f64.powf(preamp_db / 20.0),
            ));
        }
        for (index, filters) in [
            (1, profile.map(|p| p.bands.clone())),
            (2, profile.map(|p| tone(p).to_vec())),
        ] {
            let Some(bank) = &stages[index] else {
                continue;
            };
            let proxy = bank.dynamic_cast_ref::<gst::ChildProxy>().unwrap();
            for i in 0..proxy.children_count() {
                let band = proxy
                    .child_by_index(i)
                    .unwrap()
                    .downcast::<gst::Object>()
                    .unwrap();
                let filter = filters.as_ref().and_then(|v| v.get(i as usize));
                let active = filter
                    .filter(|f| f.enabled && f.frequency_hz < f64::from(processing_rate) / 2.0);
                desired.push((band.clone(), "gain", active.map_or(0.0, |f| f.gain_db)));
                if let Some(filter) = filter {
                    desired.push((band.clone(), "freq", filter.frequency_hz));
                    desired.push((
                        band,
                        "bandwidth",
                        active.map_or(0.0, |f| native_bandwidth_hz(f, processing_rate)),
                    ));
                } else {
                    for property in ["freq", "bandwidth"] {
                        let value = controls
                            .iter()
                            .find(|c| c.object == band && c.property == property)
                            .map_or_else(|| band.property::<f64>(property), |c| c.target);
                        desired.push((band.clone(), property, value));
                    }
                }
            }
        }
        let mut next_controls = Vec::with_capacity(desired.len());
        for (object, property, target) in desired {
            let control = if let Some(i) = controls
                .iter()
                .position(|c| c.object == object && c.property == property)
            {
                controls.swap_remove(i)
            } else {
                Control::new(&object, property, target)?
            };
            targets.push(target);
            next_controls.push(control);
        }
        let prepared = Prepared {
            config: config.clone(),
            stages,
            controls: next_controls,
            targets,
            preamp_db,
            rate,
            bank_shape: profile.map_or(bank_shape, |p| p.bands.iter().map(|b| b.kind).collect()),
        };
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.info != info {
            return Ok(());
        }
        state.pending = None;
        state.prepared_rate = rate;
        if self.root.current_state() < gst::State::Paused {
            state.started = false;
        }
        if self.root.current_state() < gst::State::Paused || state.block.is_some() {
            let time = state.frontier;
            install(&mut state, prepared, &self.slots, &self.mono_caps, time);
        } else {
            let structural =
                state.stages != prepared.stages || state.config.mono != prepared.config.mono;
            state.pending = Some(prepared);
            if structural {
                let weak = Arc::downgrade(&self.state);
                let slots = self.slots.clone();
                let mono = self.mono_caps.clone();
                self.input
                    .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |pad, _| {
                        if let Some(state) = weak.upgrade() {
                            let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                            if let Some(prepared) = state.pending.take() {
                                let time = state.frontier;
                                install(&mut state, prepared, &slots, &mono, time);
                                pad.mark_reconfigure();
                            }
                        }
                        gst::PadProbeReturn::Remove
                    });
            }
        }
        if let Some(block) = state.block.take() {
            self.input.remove_probe(block);
        }
        Ok(())
    }
    /// Called by the existing backend or transcoder worker, never by an audio probe.
    pub fn poll(&mut self) -> Result<Option<(ProcessingConfig, Option<u32>, f64)>, String> {
        let changed = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.info.as_ref().map(audio::AudioInfo::rate) != state.prepared_rate
                || state.block.is_some()
        };
        if changed && let Err(error) = self.prepare(&self.requested.clone()) {
            if self.requested.crossfeed {
                let mut supported = self.requested.clone();
                supported.crossfeed = false;
                self.prepare(&supported)?;
            }
            return Err(error);
        }
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .applied
            .take())
    }
    pub fn is_change_message(message: &gst::MessageRef) -> bool {
        message.type_() == gst::MessageType::Application
            && message
                .structure()
                .is_some_and(|structure| structure.name() == CHANGED)
    }
    pub fn meter_message(&self, message: &gst::MessageRef) -> Option<Vec<f64>> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let level = state.stages[4].as_ref()?;
        if message.src() != Some(level.upcast_ref()) {
            return None;
        }
        let structure = message.structure()?;
        if structure.name() != "level" {
            return None;
        }
        structure
            .get::<gst::glib::ValueArray>("peak")
            .ok()
            .map(|v| v.iter().filter_map(|v| v.get::<f64>().ok()).collect())
    }
}
fn element(factory: &str, name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|e| format!("{factory} is unavailable: {e}"))
}
fn bank(name: &str, filters: &[Filter]) -> Result<gst::Element, String> {
    let bank = element("equalizer-nbands", name)?;
    bank.set_property("num-bands", filters.len() as u32);
    let proxy = bank.dynamic_cast_ref::<gst::ChildProxy>().unwrap();
    for (i, filter) in filters.iter().enumerate() {
        let band = proxy.child_by_index(i as u32).unwrap();
        band.set_property_from_str(
            "type",
            match filter.kind {
                FilterKind::Peak => "peak",
                FilterKind::LowShelf => "low-shelf",
                FilterKind::HighShelf => "high-shelf",
            },
        );
    }
    Ok(bank)
}
fn install(
    state: &mut EffectsState,
    mut prepared: Prepared,
    slots: &[gst::Element; 5],
    mono: &gst::Element,
    time: gst::ClockTime,
) {
    let null = std::ptr::null_mut::<std::ffi::c_void>();
    let structural = state.stages != prepared.stages;
    for ((slot, old), new) in slots.iter().zip(&state.stages).zip(&prepared.stages) {
        if old == new {
            continue;
        }
        if let Some(old) = old {
            slot.emit_by_name::<()>("remove", &[old, &null, &null]);
        }
        if let Some(new) = new {
            slot.emit_by_name::<()>("append", &[new, &null, &null]);
        }
    }
    let ramp = state.started && !structural;
    let first_enable = state.stages[1].is_none() && prepared.stages[1].is_some();
    for (control, target) in prepared.controls.iter_mut().zip(prepared.targets) {
        let preamp = control.property == "volume";
        let current = ControlSourceExt::value(&control.curve, time)
            .unwrap_or_else(|| control.object.property::<f64>(control.property));
        let automatic = prepared
            .config
            .equalizer
            .as_ref()
            .is_some_and(|p| p.automatic_preamp)
            || (prepared.config.equalizer.is_none()
                && state
                    .config
                    .equalizer
                    .as_ref()
                    .is_some_and(|p| p.automatic_preamp));
        control.set(
            time,
            target,
            (ramp
                || (state.started
                    && (first_enable
                        || state
                            .controls
                            .iter()
                            .any(|old| old.object == control.object))))
                && !(preamp && automatic && target < current),
            preamp && automatic && target > current,
        );
        let _ = control.object.sync_values(time);
    }
    let current = ControlSourceExt::value(&state.balance_curve, time).unwrap_or(state.last_balance);
    state.balance_curve.unset_all();
    state.balance_curve.set(
        time,
        if ramp {
            current
        } else {
            prepared.config.balance
        },
    );
    state.balance_curve.set(
        time + gst::ClockTime::from_mseconds(50),
        prepared.config.balance,
    );
    if prepared.config.mono != state.config.mono {
        mono.set_property(
            "caps",
            if prepared.config.mono {
                gst::Caps::builder("audio/x-raw")
                    .field("channels", 1_i32)
                    .build()
            } else {
                gst::Caps::new_any()
            },
        );
    }
    state.bank_shape = prepared.bank_shape;
    state.config = prepared.config.clone();
    state.stages = prepared.stages;
    state.controls = prepared.controls;
    state.applied = Some((prepared.config, prepared.rate, prepared.preamp_db));
    notify_changed(&slots[0]);
}

const CHANGED: &str = "rufin-effects-changed";

// Format changes and applied settings need `EffectsBin::poll` on the owning worker.
fn notify_changed(element: &gst::Element) {
    let _ = element.post_message(gst::message::Application::new(gst::Structure::new_empty(
        CHANGED,
    )));
}
fn balance_matrix(info: &audio::AudioInfo, balance: f64) -> gst::Array {
    use audio::AudioChannelPosition::*;
    let positions = info.positions();
    gst::Array::new((0..info.channels() as usize).map(|row| {
        let side = positions.and_then(|p| p.get(row)).map_or(0, |p| match p {
            FrontLeft | RearLeft | SideLeft | FrontLeftOfCenter | TopFrontLeft | TopRearLeft => -1,
            FrontRight | RearRight | SideRight | FrontRightOfCenter | TopFrontRight
            | TopRearRight => 1,
            _ => 0,
        });
        let gain = if side < 0 {
            1.0 - balance.max(0.0)
        } else if side > 0 {
            1.0 + balance.min(0.0)
        } else {
            1.0
        };
        gst::Array::new(
            (0..info.channels() as usize)
                .map(|column| if row == column { gain as f32 } else { 0.0 }),
        )
    }))
}
