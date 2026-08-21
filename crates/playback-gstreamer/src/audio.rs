use super::ensure_gstreamer_initialized;
use gst::prelude::*;
use gstreamer as gst;
#[cfg(test)]
use gstreamer_app as gst_app;
use library::TrackLoudness;
use playback::{
    AudioOutput, BackendAudioSettings, EQUALIZER_BAND_COUNT, EqualizerSettings,
    LOUDNESS_NORMALIZATION_TARGET_LUFS, LoudnessNormalizationMode,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

const AUDIO_OUTPUT_DEVICE_PREFIX: &str = "gst-device:";
const CLASSIC_EQUALIZER_FREQUENCIES: [f64; EQUALIZER_BAND_COUNT] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];
const EQUALIZER_DUMMY_LOW_FREQUENCY: f64 = 20.0;
const EQUALIZER_DUMMY_HIGH_FREQUENCY: f64 = 20_000.0;

#[derive(Clone, Debug, PartialEq)]
struct AudioGraphConfig {
    loudness_normalization: LoudnessNormalizationMode,
    audio_output: Option<String>,
    preserve_pitch: bool,
}

impl AudioGraphConfig {
    fn new(settings: &BackendAudioSettings) -> Self {
        Self {
            loudness_normalization: settings.loudness_normalization,
            audio_output: settings.audio_output.clone(),
            preserve_pitch: settings.preserve_pitch,
        }
    }
}

pub(super) struct AudioGraph {
    root: gst::Element,
    config: AudioGraphConfig,
    output: gst::Element,
    equalizer: gst::Element,
    loudness_tags: Option<LoudnessTags>,
    visualizer_pad: Option<gst::Pad>,
}

impl AudioGraph {
    pub(super) fn new(settings: &BackendAudioSettings) -> Result<Self, String> {
        let normalizes_loudness = settings.loudness_normalization != LoudnessNormalizationMode::Off;
        let bin = gst::Bin::new();
        let convert_in = make_element("audioconvert", "rufin-audio-convert-in")?;
        let convert_out = make_element("audioconvert", "rufin-audio-convert-out")?;
        let resample = make_element("audioresample", "rufin-audio-resample")?;
        let output = make_audio_output(settings.audio_output.as_deref())?;
        #[cfg(test)]
        configure_test_output(&output);
        let mut elements = vec![convert_in.clone()];

        let equalizer = make_element("equalizer-nbands", "rufin-equalizer")?;
        equalizer.set_property("num-bands", (EQUALIZER_BAND_COUNT + 2) as u32);
        configure_equalizer(&equalizer, &settings.equalizer);
        elements.push(equalizer.clone());

        let mut loudness_tags = None;
        if normalizes_loudness {
            let (tags, handle) = make_loudness_tags(settings.loudness_normalization)?;
            elements.push(tags);
            loudness_tags = Some(handle);

            let rgvolume = make_element("rgvolume", "rufin-loudness-normalization")?;
            rgvolume.set_property(
                "album-mode",
                settings.loudness_normalization == LoudnessNormalizationMode::Album,
            );
            elements.push(rgvolume);
        }

        if settings.preserve_pitch {
            let scaletempo = make_element("scaletempo", "rufin-playback-rate")?;
            elements.push(scaletempo);
        }

        let visualizer_pad = convert_out.static_pad("src");
        elements.push(convert_out.clone());
        elements.push(resample);
        elements.push(output.clone());
        for element in &elements {
            bin.add(element).map_err(|error| error.to_string())?;
        }
        let refs = elements.iter().collect::<Vec<_>>();
        gst::Element::link_many(&refs).map_err(|error| error.to_string())?;

        let sink_pad = convert_in
            .static_pad("sink")
            .ok_or_else(|| "audio chain is missing an input pad".to_string())?;
        let ghost_sink =
            gst::GhostPad::with_target(&sink_pad).map_err(|error| error.to_string())?;
        ghost_sink
            .set_active(true)
            .map_err(|error| error.to_string())?;
        bin.add_pad(&ghost_sink)
            .map_err(|error| error.to_string())?;

        Ok(Self {
            root: bin.upcast(),
            config: AudioGraphConfig::new(settings),
            output,
            equalizer,
            loudness_tags,
            visualizer_pad,
        })
    }

    pub(super) fn root(&self) -> &gst::Element {
        &self.root
    }

    pub(super) fn reconfigure(&mut self, settings: &BackendAudioSettings) -> Result<bool, String> {
        let config = AudioGraphConfig::new(settings);
        if self.config.loudness_normalization != config.loudness_normalization
            || self.config.preserve_pitch != config.preserve_pitch
        {
            return Ok(false);
        }
        if self.config.audio_output != config.audio_output
            && !set_output_target(
                &self.output,
                config
                    .audio_output
                    .as_deref()
                    .and_then(audio_output_device_selector),
            )
        {
            return Ok(false);
        }
        self.config = config;
        self.apply_equalizer(&settings.equalizer);
        Ok(true)
    }

    pub(super) fn visualizer_pad(&self) -> Option<&gst::Pad> {
        self.visualizer_pad.as_ref()
    }

    pub(super) fn output_factory(&self) -> Option<String> {
        self.output
            .factory()
            .map(|factory| factory.name().to_string())
    }

    pub(super) fn apply_loudness(&self, loudness: &TrackLoudness) {
        if let Some(tags) = self.loudness_tags.as_ref() {
            tags.apply(loudness);
        }
    }

    pub(super) fn loudness_tags(&self) -> Option<LoudnessTags> {
        self.loudness_tags.clone()
    }

    fn apply_equalizer(&self, settings: &EqualizerSettings) {
        configure_equalizer(&self.equalizer, settings);
    }
}

#[cfg(test)]
fn configure_test_output(output: &gst::Element) {
    if output
        .factory()
        .is_some_and(|factory| factory.name() == "fakesink")
    {
        output.set_property("async", false);
        output.set_property("sync", false);
        return;
    }
    let Some(sink) = output.downcast_ref::<gst_app::AppSink>() else {
        return;
    };
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 1_i32)
        .field("rate", 8_000_i32)
        .build();
    sink.set_caps(Some(&caps));
    sink.set_max_buffers(8);
    sink.set_drop(false);
    sink.set_sync(false);
}

pub(super) type SharedLoudnessTags = Arc<Mutex<Option<LoudnessTags>>>;

pub(super) fn apply_shared_loudness(shared: &SharedLoudnessTags, loudness: &TrackLoudness) {
    if let Some(tags) = shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
    {
        tags.apply(loudness);
    }
}

#[derive(Clone)]
pub(super) struct LoudnessTags {
    state: Arc<Mutex<LoudnessTagState>>,
}

struct LoudnessTagState {
    mode: LoudnessNormalizationMode,
    internal: Option<gst::TagList>,
    fallback: Option<gst::TagList>,
    sent: bool,
}

impl LoudnessTags {
    fn apply(&self, loudness: &TrackLoudness) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.internal = internal_loudness_tags(state.mode, loudness);
        state.sent = false;
    }
}

fn make_loudness_tags(
    mode: LoudnessNormalizationMode,
) -> Result<(gst::Element, LoudnessTags), String> {
    let element = make_element("identity", "rufin-loudness-tags")?;
    let state = Arc::new(Mutex::new(LoudnessTagState {
        mode,
        internal: None,
        fallback: None,
        sent: false,
    }));
    let handle = LoudnessTags {
        state: Arc::clone(&state),
    };
    let sink_pad = element
        .static_pad("sink")
        .ok_or_else(|| "loudness tag handoff is missing its input pad".to_string())?;
    sink_pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        handle_loudness_event(info, &state);
        gst::PadProbeReturn::Ok
    });

    let state_for_handoff = Arc::clone(&handle.state);
    let src_pad = element
        .static_pad("src")
        .ok_or_else(|| "loudness tag handoff is missing its output pad".to_string())?;
    element.connect("handoff", false, move |_| {
        push_loudness_tags(&src_pad, &state_for_handoff);
        None
    });
    Ok((element, handle))
}

fn handle_loudness_event(info: &mut gst::PadProbeInfo<'_>, state: &Mutex<LoudnessTagState>) {
    let Some(event) = info.event().cloned() else {
        return;
    };
    match event.view() {
        gst::EventView::StreamStart(_) => {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.fallback = None;
            state.sent = false;
        }
        gst::EventView::Tag(tag) => {
            let incoming = tag.tag_owned();
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(fallback) = selected_loudness_tags(state.mode, &incoming) {
                state.fallback = Some(fallback);
            }
            if let Some(internal) = state.internal.as_ref() {
                let merged = incoming.merge(internal, gst::TagMergeMode::Replace);
                let replacement = gst::event::Tag::builder(merged)
                    .seqnum(event.seqnum())
                    .build();
                info.data = Some(gst::PadProbeData::Event(replacement));
                state.sent = true;
            } else if state.fallback.is_some() {
                state.sent = true;
            }
        }
        _ => {}
    }
}

fn push_loudness_tags(pad: &gst::Pad, state: &Mutex<LoudnessTagState>) {
    let tags = {
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.sent {
            return;
        }
        state.sent = true;
        state
            .internal
            .clone()
            .or_else(|| state.fallback.clone())
            .unwrap_or_else(|| neutral_loudness_tags(state.mode))
    };
    pad.push_event(gst::event::Tag::new(tags));
}

fn internal_loudness_tags(
    mode: LoudnessNormalizationMode,
    loudness: &TrackLoudness,
) -> Option<gst::TagList> {
    let measurement = match mode {
        LoudnessNormalizationMode::Off => None,
        LoudnessNormalizationMode::Track => loudness.track,
        LoudnessNormalizationMode::Album => loudness.album,
    }?;
    let gain = measurement
        .integrated_lufs
        .map_or(0.0, |lufs| LOUDNESS_NORMALIZATION_TARGET_LUFS - lufs);
    Some(loudness_tag_list(mode, gain, measurement.true_peak_ratio))
}

fn neutral_loudness_tags(mode: LoudnessNormalizationMode) -> gst::TagList {
    loudness_tag_list(mode, 0.0, 1.0)
}

fn loudness_tag_list(mode: LoudnessNormalizationMode, gain: f64, peak: f64) -> gst::TagList {
    let mut tags = gst::TagList::new();
    let tags = tags.make_mut();
    match mode {
        LoudnessNormalizationMode::Off | LoudnessNormalizationMode::Track => {
            tags.add::<gst::tags::TrackGain>(&gain, gst::TagMergeMode::Replace);
            tags.add::<gst::tags::TrackPeak>(&peak, gst::TagMergeMode::Replace);
        }
        LoudnessNormalizationMode::Album => {
            tags.add::<gst::tags::AlbumGain>(&gain, gst::TagMergeMode::Replace);
            tags.add::<gst::tags::AlbumPeak>(&peak, gst::TagMergeMode::Replace);
        }
    }
    tags.to_owned()
}

fn selected_loudness_tags(
    mode: LoudnessNormalizationMode,
    incoming: &gst::TagListRef,
) -> Option<gst::TagList> {
    match mode {
        LoudnessNormalizationMode::Off | LoudnessNormalizationMode::Track => {
            incoming.get::<gst::tags::TrackGain>().map(|gain| {
                loudness_tag_list(
                    mode,
                    gain.get(),
                    incoming
                        .get::<gst::tags::TrackPeak>()
                        .map_or(1.0, |peak| peak.get()),
                )
            })
        }
        LoudnessNormalizationMode::Album => incoming
            .get::<gst::tags::AlbumGain>()
            .map(|gain| {
                loudness_tag_list(
                    mode,
                    gain.get(),
                    incoming
                        .get::<gst::tags::AlbumPeak>()
                        .map_or(1.0, |peak| peak.get()),
                )
            })
            .or_else(|| {
                incoming.get::<gst::tags::TrackGain>().map(|gain| {
                    loudness_tag_list(
                        LoudnessNormalizationMode::Track,
                        gain.get(),
                        incoming
                            .get::<gst::tags::TrackPeak>()
                            .map_or(1.0, |peak| peak.get()),
                    )
                })
            }),
    }
}

pub fn available_audio_outputs() -> Vec<AudioOutput> {
    if ensure_gstreamer_initialized().is_err() {
        return Vec::new();
    }
    available_audio_output_devices()
        .into_iter()
        .map(|output| AudioOutput {
            id: output.id,
            name: output.name,
        })
        .collect()
}

fn audio_output_device_id(node_name: &str) -> String {
    format!("{AUDIO_OUTPUT_DEVICE_PREFIX}{node_name}")
}

fn audio_output_device_selector(id: &str) -> Option<&str> {
    id.strip_prefix(AUDIO_OUTPUT_DEVICE_PREFIX)
        .filter(|target| !target.is_empty())
}

pub(super) fn audio_output_is_available(selected: &str) -> bool {
    audio_output_device_selector(selected).is_none()
        || available_audio_output_devices()
            .into_iter()
            .any(|output| output.id == selected)
}

struct AudioOutputDevice {
    id: String,
    name: String,
    device: gst::Device,
}

fn available_audio_output_devices() -> Vec<AudioOutputDevice> {
    #[cfg(target_os = "linux")]
    {
        let outputs = pulse_audio_output_devices();
        if !outputs.is_empty() {
            return outputs;
        }
    }

    let monitor = gst::DeviceMonitor::new();
    let _filter_id = monitor.add_filter(Some("Audio/Sink"), None);
    if monitor.start().is_err() {
        return Vec::new();
    }
    let outputs = collect_audio_output_devices(monitor.devices());
    monitor.stop();
    outputs
}

#[cfg(target_os = "linux")]
fn pulse_audio_output_devices() -> Vec<AudioOutputDevice> {
    let Some(provider) =
        gst::DeviceProviderFactory::find("pulsedeviceprovider").and_then(|factory| factory.get())
    else {
        return Vec::new();
    };
    if provider.start().is_err() {
        return Vec::new();
    }
    let outputs = collect_audio_output_devices(
        provider
            .devices()
            .into_iter()
            .filter(|device| device.device_class() == "Audio/Sink"),
    );
    provider.stop();
    outputs
}

fn collect_audio_output_devices(
    devices: impl IntoIterator<Item = gst::Device>,
) -> Vec<AudioOutputDevice> {
    let mut seen = HashSet::new();
    let mut outputs = devices
        .into_iter()
        .filter_map(|device| {
            let selector = audio_output_device_selector_for(&device)?;
            let id = audio_output_device_id(&selector);
            if !seen.insert(id.clone()) {
                return None;
            }
            Some(AudioOutputDevice {
                id,
                name: device.display_name().to_string(),
                device,
            })
        })
        .collect::<Vec<_>>();
    outputs.sort_by_key(|output| output.name.to_lowercase());
    outputs
}

fn audio_output_device_selector_for(device: &gst::Device) -> Option<String> {
    device
        .properties()
        .as_deref()
        .and_then(audio_output_selector_from_properties)
        .or_else(|| audio_output_selector_from_element(device))
}

fn audio_output_selector_from_properties(properties: &gst::StructureRef) -> Option<String> {
    [
        "node.name",
        "unique-id",
        "device.strid",
        "device.id",
        "device.guid",
    ]
    .into_iter()
    .find_map(|name| properties.get::<String>(name).ok())
    .filter(|selector| !selector.trim().is_empty())
}

fn audio_output_selector_from_element(device: &gst::Device) -> Option<String> {
    let output = device.create_element(None).ok()?;
    ["unique-id", "device", "target-object"]
        .into_iter()
        .find_map(|name| {
            output
                .find_property(name)
                .filter(|property| property.value_type() == String::static_type())?;
            output.property::<Option<String>>(name)
        })
        .filter(|selector| !selector.trim().is_empty())
}

fn make_audio_output(selected: Option<&str>) -> Result<gst::Element, String> {
    match selected {
        None => make_element(default_audio_output_factory(), "rufin-audio-output"),
        Some(selected) => {
            if audio_output_device_selector(selected).is_some() {
                let output = available_audio_output_devices()
                    .into_iter()
                    .find(|output| output.id == selected)
                    .ok_or_else(|| selected_output_unavailable(selected))?;
                return output
                    .device
                    .create_element(Some("rufin-audio-output"))
                    .map_err(|_| selected_output_unavailable(selected));
            }
            if gst::ElementFactory::find(selected).is_none() {
                return Err(selected_output_unavailable(selected));
            }
            make_element(selected, "rufin-audio-output")
                .map_err(|_| selected_output_unavailable(selected))
        }
    }
}

#[cfg(target_os = "macos")]
fn default_audio_output_factory() -> &'static str {
    "osxaudiosink"
}

#[cfg(not(target_os = "macos"))]
fn default_audio_output_factory() -> &'static str {
    "autoaudiosink"
}

fn selected_output_unavailable(selected: &str) -> String {
    format!("Selected audio output is unavailable: {selected}")
}

fn set_output_target(output: &gst::Element, target: Option<&str>) -> bool {
    let apply = |element: &gst::glib::Object| {
        if element.find_property("device").is_some() {
            element.set_property("device", target.unwrap_or("@DEFAULT_SINK@"));
            true
        } else if element.find_property("target-object").is_some() {
            element.set_property("target-object", target);
            true
        } else {
            false
        }
    };
    if apply(output.upcast_ref()) {
        return true;
    }
    output
        .dynamic_cast_ref::<gst::ChildProxy>()
        .and_then(|proxy| proxy.child_by_index(0))
        .is_some_and(|child| apply(&child))
}

fn set_equalizer_band(
    equalizer: &gst::Element,
    index: usize,
    frequency: f64,
    bandwidth: f64,
    gain: f64,
) {
    let Some(proxy) = equalizer.dynamic_cast_ref::<gst::ChildProxy>() else {
        return;
    };
    if let Some(band) = proxy.child_by_index(index as u32) {
        band.set_property("freq", frequency);
        band.set_property("bandwidth", bandwidth);
        band.set_property("gain", gain);
    }
}

fn configure_equalizer(equalizer: &gst::Element, settings: &EqualizerSettings) {
    set_equalizer_band(equalizer, 0, EQUALIZER_DUMMY_LOW_FREQUENCY, 0.0, 0.0);
    set_equalizer_band(
        equalizer,
        EQUALIZER_BAND_COUNT + 1,
        EQUALIZER_DUMMY_HIGH_FREQUENCY,
        0.0,
        0.0,
    );
    let mut previous = 0.0;
    for (index, frequency) in CLASSIC_EQUALIZER_FREQUENCIES.iter().copied().enumerate() {
        let gain = if settings.enabled {
            settings.bands.get(index).copied().unwrap_or(0.0)
        } else {
            0.0
        };
        set_equalizer_band(equalizer, index + 1, frequency, frequency - previous, gain);
        previous = frequency;
    }
}

fn make_element(factory: &str, name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(factory)
        .name(name)
        .build()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use library::LoudnessMeasurement;

    use super::*;

    fn initialize_gstreamer() {
        ensure_gstreamer_initialized().expect("initialize GStreamer");
    }

    #[test]
    fn explicit_unavailable_output_does_not_fall_back_to_default() {
        initialize_gstreamer();
        let result = make_audio_output(Some("rufin-output-that-does-not-exist"));
        assert!(result.is_err_and(|error| error.contains("unavailable")));
    }

    #[test]
    fn no_output_preference_uses_the_system_default_sink() {
        initialize_gstreamer();
        let expected = default_audio_output_factory();
        assert!(
            gst::ElementFactory::find(expected).is_some(),
            "required system audio output is unavailable: {expected}"
        );
        let output = make_audio_output(None).expect("system default output");
        assert_eq!(
            output.factory().map(|factory| factory.name().to_string()),
            Some(expected.to_string())
        );
    }

    #[test]
    fn device_identity_uses_the_platform_provider_property() {
        initialize_gstreamer();
        let cases = [
            ("node.name", "alsa_output.test"),
            ("unique-id", "macos-output-id"),
            ("device.strid", "windows-device-interface"),
            ("device.id", "windows-endpoint-id"),
            ("device.guid", "directsound-device-guid"),
        ];

        for (property, expected) in cases {
            let properties = gst::Structure::builder("audio-device-properties")
                .field(property, expected)
                .build();
            assert_eq!(
                audio_output_selector_from_properties(&properties).as_deref(),
                Some(expected)
            );
        }

        let properties = gst::Structure::builder("audio-device-properties")
            .field("node.name", "alsa_output.persisted")
            .field("device.id", "different-fallback")
            .build();
        assert_eq!(
            audio_output_selector_from_properties(&properties).as_deref(),
            Some("alsa_output.persisted")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn device_change_retargets_the_selected_sink() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            audio_output: Some("pulsesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let mut graph = AudioGraph::new(&settings).expect("Pulse output graph");
        let mut changed = settings;
        changed.audio_output = Some(audio_output_device_id("alsa_output.selected"));

        assert!(graph.reconfigure(&changed).expect("retarget output"));
        assert_eq!(
            graph
                .output
                .factory()
                .map(|factory| factory.name().to_string()),
            Some("pulsesink".to_string())
        );
        assert_eq!(
            graph.output.property::<Option<String>>("device").as_deref(),
            Some("alsa_output.selected")
        );
    }

    #[test]
    fn equalizer_changes_apply_live_and_disable_to_zero_gain() {
        initialize_gstreamer();
        let equalizer =
            make_element("equalizer-nbands", "test-live-equalizer").expect("packaged equalizer");
        equalizer.set_property("num-bands", (EQUALIZER_BAND_COUNT + 2) as u32);
        let mut settings = EqualizerSettings {
            enabled: true,
            bands: vec![5.0; EQUALIZER_BAND_COUNT],
            ..EqualizerSettings::default()
        };
        configure_equalizer(&equalizer, &settings);
        assert_eq!(equalizer_band_gain(&equalizer, 1), Some(5.0));

        settings.enabled = false;
        configure_equalizer(&equalizer, &settings);
        assert_eq!(equalizer_band_gain(&equalizer, 1), Some(0.0));
    }

    #[test]
    fn track_normalization_disables_album_mode_without_a_limiter() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalizationMode::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };

        let graph = AudioGraph::new(&settings).expect("track normalization graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        let rgvolume = bin
            .by_name("rufin-loudness-normalization")
            .expect("loudness normalization volume element");

        assert!(!rgvolume.property::<bool>("album-mode"));
        assert!(bin.by_name("rufin-replaygain-limiter").is_none());
    }

    #[test]
    fn preserve_pitch_controls_the_scaletempo_stage() {
        initialize_gstreamer();
        let enabled = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            preserve_pitch: true,
            ..BackendAudioSettings::default()
        };
        let mut graph = AudioGraph::new(&enabled).expect("pitch-preserving audio graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-playback-rate").is_some());

        let disabled = BackendAudioSettings {
            preserve_pitch: false,
            ..enabled
        };
        assert!(
            !graph
                .reconfigure(&disabled)
                .expect("pitch preservation configuration change")
        );
        let graph = AudioGraph::new(&disabled).expect("pitch-shifting audio graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-playback-rate").is_none());
    }

    #[test]
    fn stored_r128_measurement_replaces_the_selected_replaygain_scope() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalizationMode::Album,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let graph = AudioGraph::new(&settings).expect("album normalization graph");
        graph.apply_loudness(&TrackLoudness {
            track: LoudnessMeasurement::new(Some(-21.0), 0.4).ok(),
            album: LoudnessMeasurement::new(Some(-23.0), 0.8).ok(),
        });

        let tags = graph
            .loudness_tags
            .as_ref()
            .expect("loudness tag handoff")
            .state
            .lock()
            .expect("loudness tag state")
            .internal
            .clone()
            .expect("stored album loudness tags");
        assert_eq!(
            tags.get::<gst::tags::AlbumGain>().map(|gain| gain.get()),
            Some(5.0)
        );
        assert_eq!(
            tags.get::<gst::tags::AlbumPeak>().map(|peak| peak.get()),
            Some(0.8)
        );

        graph.apply_loudness(&TrackLoudness::default());
        assert!(
            graph
                .loudness_tags
                .as_ref()
                .expect("loudness tag handoff")
                .state
                .lock()
                .expect("loudness tag state")
                .internal
                .is_none()
        );
    }

    #[test]
    fn album_mode_keeps_embedded_track_gain_as_its_fallback() {
        initialize_gstreamer();
        let embedded = loudness_tag_list(LoudnessNormalizationMode::Track, -3.0, 0.7);

        let fallback = selected_loudness_tags(LoudnessNormalizationMode::Album, &embedded)
            .expect("embedded track fallback");

        assert_eq!(
            fallback
                .get::<gst::tags::TrackGain>()
                .map(|gain| gain.get()),
            Some(-3.0)
        );
        assert!(fallback.get::<gst::tags::AlbumGain>().is_none());
    }

    #[test]
    fn stored_r128_gain_reaches_rgvolume_before_audio() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalizationMode::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let graph = AudioGraph::new(&settings).expect("normalization graph");
        graph.apply_loudness(&TrackLoudness {
            track: LoudnessMeasurement::new(Some(-23.0), 0.1).ok(),
            album: None,
        });
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("num-buffers", 4_i32)
            .build()
            .expect("test audio source");
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&source, graph.root()])
            .expect("test normalization pipeline");
        source
            .link(graph.root())
            .expect("test normalization pipeline link");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        let rgvolume = bin
            .by_name("rufin-loudness-normalization")
            .expect("loudness normalization volume element");
        let observed_gain = Arc::new(Mutex::new(None));
        let observed_gain_for_probe = Arc::clone(&observed_gain);
        let rgvolume_for_probe = rgvolume.clone();
        rgvolume
            .static_pad("src")
            .expect("loudness normalization output pad")
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                observed_gain_for_probe
                    .lock()
                    .expect("observed gain")
                    .get_or_insert_with(|| rgvolume_for_probe.property::<f64>("result-gain"));
                gst::PadProbeReturn::Ok
            });

        pipeline
            .set_state(gst::State::Playing)
            .expect("start test normalization pipeline");
        let bus = pipeline.bus().expect("test normalization bus");
        let error = loop {
            let message = bus
                .timed_pop(gst::ClockTime::from_seconds(5))
                .expect("test normalization pipeline completion");
            match message.view() {
                gst::MessageView::Eos(..) => break None,
                gst::MessageView::Error(error) => break Some(error.error().to_string()),
                _ => {}
            }
        };
        pipeline
            .set_state(gst::State::Null)
            .expect("stop test normalization pipeline");
        assert!(error.is_none(), "{}", error.unwrap_or_default());
        let observed_gain = observed_gain
            .lock()
            .expect("observed gain")
            .expect("gain before the first audio buffer");
        assert!((observed_gain - 5.0).abs() < 0.001);
    }

    fn equalizer_band_gain(equalizer: &gst::Element, index: usize) -> Option<f64> {
        equalizer
            .dynamic_cast_ref::<gst::ChildProxy>()?
            .child_by_index(index as u32)
            .map(|band| band.property::<f64>("gain"))
    }
}
