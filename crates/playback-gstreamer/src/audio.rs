use super::ensure_gstreamer_initialized;
use gst::prelude::*;
use gstreamer as gst;
#[cfg(test)]
use gstreamer_app as gst_app;
use playback::{
    AudioOutput, BackendAudioSettings, DEFAULT_PLAYBACK_RATE, EQUALIZER_BAND_COUNT,
    EqualizerSettings, LoudnessNormalization, LoudnessNormalizationScope,
};
use playback::{PreparedStream, TrackLoudness};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const AUDIO_OUTPUT_DEVICE_PREFIX: &str = "gst-device:";
const CLASSIC_EQUALIZER_FREQUENCIES: [f64; EQUALIZER_BAND_COUNT] = [
    60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0,
];
const EQUALIZER_DUMMY_LOW_FREQUENCY: f64 = 20.0;
const EQUALIZER_DUMMY_HIGH_FREQUENCY: f64 = 20_000.0;

pub(super) type SharedQueuedStream = Arc<Mutex<PreparedStream>>;

pub(super) struct AudioGraph {
    root: gst::Element,
    input: gst::Element,
    effect_bins: [gst::Element; 3],
    output: gst::Element,
    audio_output: Option<String>,
    state: Arc<Mutex<AudioGraphState>>,
    visualizer_pad: Option<gst::Pad>,
}

#[derive(Clone, Default)]
struct AudioEffects {
    equalizer: Option<gst::Element>,
    volume: Option<gst::Element>,
    tempo: Option<gst::Element>,
}

impl AudioEffects {
    fn stages(&self) -> [Option<&gst::Element>; 3] {
        [
            self.equalizer.as_ref(),
            self.volume.as_ref(),
            self.tempo.as_ref(),
        ]
    }
}

struct AudioGraphState {
    settings: BackendAudioSettings,
    effects: AudioEffects,
    current_loudness: TrackLoudness,
    tags: gst::TagList,
    pending: Option<(BackendAudioSettings, AudioEffects)>,
}

impl AudioGraphState {
    fn apply_loudness(&self) {
        let Some(volume) = self.effects.volume.as_ref() else {
            return;
        };
        let gain = selected_loudness(&self.settings, &self.current_loudness)
            .map(|(gain, peak)| clipping_safe_gain_db(gain, peak))
            .unwrap_or_else(|| {
                embedded_replaygain(&self.tags, self.settings.loudness_normalization_scope)
            });
        apply_loudness_gain(volume, gain);
    }
}

impl AudioGraph {
    pub(super) fn new(
        settings: &BackendAudioSettings,
        playback_rate: f64,
        current_loudness: TrackLoudness,
        queued_stream: SharedQueuedStream,
    ) -> Result<Self, String> {
        let bin = gst::Bin::new();
        let convert_in = make_element("audioconvert", "rufin-audio-convert-in")?;
        let convert_out = make_element("audioconvert", "rufin-audio-convert-out")?;
        let split = make_element("audiobuffersplit", "rufin-audio-buffer-split")?;
        // Decoder packets can span half a second. Keep output and analysis paced in
        // short blocks, including for WavPack, without changing the audio samples.
        split.set_property("output-buffer-duration", gst::Fraction::new(1, 60));
        let new_segment = AtomicBool::new(false);
        split
            .static_pad("sink")
            .expect("audio splitter input")
            .add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
                move |_, info| {
                    if info
                        .event()
                        .is_some_and(|event| matches!(event.view(), gst::EventView::Segment(_)))
                    {
                        new_segment.store(true, Ordering::Relaxed);
                    } else if let Some(buffer) = info.buffer_mut()
                        && new_segment.swap(false, Ordering::Relaxed)
                    {
                        // The splitter otherwise delays a contiguous CUE segment until
                        // its end, leaving downstream sinks clipping to the previous stop.
                        buffer.make_mut().set_flags(gst::BufferFlags::DISCONT);
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        let resample = make_element("audioresample", "rufin-audio-resample")?;
        let output = make_audio_output(settings.audio_output.as_deref())?;
        #[cfg(test)]
        configure_test_output(&output);
        let effects = prepare_effects(settings, playback_rate, &AudioEffects::default())?;
        let effect_bins = [
            make_element("insertbin", "rufin-equalizer-bin")?,
            make_element("insertbin", "rufin-normalization-bin")?,
            make_element("insertbin", "rufin-tempo-bin")?,
        ];
        apply_effects(&effect_bins, &AudioEffects::default(), &effects);
        let mut elements = vec![convert_in.clone()];
        elements.extend(effect_bins.iter().cloned());
        let state = Arc::new(Mutex::new(AudioGraphState {
            settings: settings.clone(),
            effects,
            current_loudness,
            tags: gst::TagList::new(),
            pending: None,
        }));
        state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .apply_loudness();
        install_loudness_boundary(&convert_in, Arc::clone(&state), queued_stream)?;

        // Sample after playbin's queue so visualization follows audible output.
        let visualizer_pad = output.static_pad("sink");
        elements.push(convert_out.clone());
        elements.push(split);
        elements.push(resample.clone());
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
        let src_pad = resample
            .static_pad("src")
            .ok_or_else(|| "audio chain is missing an output pad".to_string())?;
        let ghost_src = gst::GhostPad::with_target(&src_pad).map_err(|error| error.to_string())?;
        ghost_src
            .set_active(true)
            .map_err(|error| error.to_string())?;
        bin.add_pad(&ghost_src).map_err(|error| error.to_string())?;

        Ok(Self {
            root: bin.upcast(),
            input: convert_in,
            effect_bins,
            output,
            audio_output: settings.audio_output.clone(),
            state,
            visualizer_pad,
        })
    }

    pub(super) fn root(&self) -> &gst::Element {
        &self.root
    }

    pub(super) fn output(&self) -> &gst::Element {
        &self.output
    }

    pub(super) fn reconfigure(
        &mut self,
        settings: &BackendAudioSettings,
        playback_rate: f64,
    ) -> Result<bool, String> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut available = state.effects.clone();
        if let Some((_, pending)) = state.pending.as_ref() {
            available.equalizer = available.equalizer.or_else(|| pending.equalizer.clone());
            available.volume = available.volume.or_else(|| pending.volume.clone());
            available.tempo = available.tempo.or_else(|| pending.tempo.clone());
        }
        let effects = prepare_effects(settings, playback_rate, &available)?;
        if self.audio_output != settings.audio_output
            && !set_output_target(
                &self.output,
                settings
                    .audio_output
                    .as_deref()
                    .and_then(audio_output_device_selector),
            )
        {
            return Ok(false);
        }
        self.audio_output = settings.audio_output.clone();
        if state.pending.is_none()
            && (state.effects.stages() == effects.stages()
                || self.root.current_state() < gst::State::Paused)
        {
            apply_effects(&self.effect_bins, &state.effects, &effects);
            state.settings = settings.clone();
            state.effects = effects;
            if let Some(equalizer) = state.effects.equalizer.as_ref() {
                configure_equalizer(equalizer, &settings.equalizer);
            }
            state.apply_loudness();
            return Ok(true);
        }
        let scheduled = state.pending.is_some();
        state.pending = Some((settings.clone(), effects));
        drop(state);
        if !scheduled {
            let state = Arc::clone(&self.state);
            let root = self.root.downgrade();
            let bins = self.effect_bins.clone();
            // Wait for the current push and its queries to finish. An IDLE probe
            // can run while a paused downstream queue still blocks a query.
            self.input
                .static_pad("src")
                .expect("audio converter output")
                .add_probe(gst::PadProbeType::BLOCK_DOWNSTREAM, move |pad, _| {
                    let Some(_root) = root.upgrade() else {
                        return gst::PadProbeReturn::Remove;
                    };
                    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                    let Some((settings, effects)) = state.pending.take() else {
                        return gst::PadProbeReturn::Remove;
                    };
                    apply_effects(&bins, &state.effects, &effects);
                    state.settings = settings;
                    state.effects = effects;
                    if let Some(equalizer) = state.effects.equalizer.as_ref() {
                        configure_equalizer(equalizer, &state.settings.equalizer);
                    }
                    state.apply_loudness();
                    pad.mark_reconfigure();
                    gst::PadProbeReturn::Remove
                });
        }
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
}

fn prepare_effects(
    settings: &BackendAudioSettings,
    playback_rate: f64,
    available: &AudioEffects,
) -> Result<AudioEffects, String> {
    let equalizer = if settings.equalizer.enabled {
        Some(match available.equalizer.as_ref() {
            Some(element) => element.clone(),
            None => {
                let element = make_element("equalizer-nbands", "rufin-equalizer")?;
                element.set_property("num-bands", (EQUALIZER_BAND_COUNT + 2) as u32);
                configure_equalizer(&element, &settings.equalizer);
                element
            }
        })
    } else {
        None
    };
    let volume = if settings.loudness_normalization != LoudnessNormalization::Off {
        Some(match available.volume.as_ref() {
            Some(element) => element.clone(),
            None => make_element("volume", "rufin-loudness-normalization")?,
        })
    } else {
        None
    };
    let tempo = if tempo_enabled(settings, playback_rate) {
        Some(match available.tempo.as_ref() {
            Some(element) => element.clone(),
            None => make_element("scaletempo", "rufin-playback-rate")?,
        })
    } else {
        None
    };
    Ok(AudioEffects {
        equalizer,
        volume,
        tempo,
    })
}

fn apply_effects(bins: &[gst::Element; 3], before: &AudioEffects, after: &AudioEffects) {
    let no_callback = std::ptr::null_mut::<std::ffi::c_void>();
    for ((bin, old), new) in bins.iter().zip(before.stages()).zip(after.stages()) {
        if old == new {
            continue;
        }
        if let Some(element) = old {
            bin.emit_by_name::<()>("remove", &[element, &no_callback, &no_callback]);
        }
        if let Some(element) = new {
            bin.emit_by_name::<()>("append", &[element, &no_callback, &no_callback]);
        }
    }
}

fn install_loudness_boundary(
    input: &gst::Element,
    state: Arc<Mutex<AudioGraphState>>,
    stream: SharedQueuedStream,
) -> Result<(), String> {
    let input = input
        .static_pad("sink")
        .ok_or_else(|| "audio chain is missing an input pad".to_string())?;
    input.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, move |_, info| {
        let Some(event) = info.event() else {
            return gst::PadProbeReturn::Ok;
        };
        match event.view() {
            gst::EventView::StreamStart(_) => {
                let stream = stream
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                state.current_loudness = stream.loudness.clone();
                state.tags = gst::TagList::new();
                state.apply_loudness();
            }
            gst::EventView::Tag(tag) => {
                let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                state
                    .tags
                    .make_mut()
                    .insert(tag.tag(), gst::TagMergeMode::Replace);
                state.apply_loudness();
            }
            _ => {}
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

fn apply_loudness_gain(volume: &gst::Element, gain_db: f64) {
    volume.set_property("volume", 10_f64.powf(gain_db / 20.0).clamp(0.0, 10.0));
}

fn embedded_replaygain(tags: &gst::TagListRef, scope: LoudnessNormalizationScope) -> f64 {
    let value = |name: &str| {
        tags.generic(name)
            .and_then(|value| value.get::<f64>().ok())
            .filter(|value| value.is_finite())
    };
    let gain = |album| {
        let (gain_tag, peak_tag, r128_tag) = if album {
            (
                "replaygain-album-gain",
                "replaygain-album-peak",
                "r128-album-gain",
            )
        } else {
            (
                "replaygain-track-gain",
                "replaygain-track-peak",
                "r128-track-gain",
            )
        };
        // Opus R128 gains use a -23 LUFS reference; embedded ReplayGain uses -18.
        if let Some(gain) = value(r128_tag) {
            return Some(clipping_safe_gain_db(gain + 5.0, None));
        }
        let gain = value(gain_tag)?;
        let reference_adjustment = value("replaygain-reference-level")
            .filter(|level| *level > 0.0)
            .map_or(0.0, |level| 89.0 - level);
        let gain = gain + reference_adjustment;
        (-60.0 < gain && gain < 60.0).then(|| clipping_safe_gain_db(gain, value(peak_tag)))
    };
    let album = scope == LoudnessNormalizationScope::Album;
    gain(album).or_else(|| gain(!album)).unwrap_or_default()
}

fn selected_loudness(
    settings: &BackendAudioSettings,
    loudness: &TrackLoudness,
) -> Option<(f64, Option<f64>)> {
    let measurement = match settings.loudness_normalization_scope {
        LoudnessNormalizationScope::Track => loudness.track.as_ref(),
        LoudnessNormalizationScope::Album => loudness.album.as_ref(),
    }?;
    match settings.loudness_normalization {
        LoudnessNormalization::Off => None,
        LoudnessNormalization::ReplayGain => measurement
            .replay_gain_db
            .map(|gain| (gain, measurement.replay_gain_peak))
            .or_else(|| {
                measurement
                    .integrated_lufs
                    .map(|lufs| (-23.0 - lufs, measurement.true_peak))
            }),
        LoudnessNormalization::EbuR128 => measurement
            .integrated_lufs
            .map(|lufs| (settings.ebu_r128_target_lufs - lufs, measurement.true_peak))
            .or_else(|| {
                measurement
                    .replay_gain_db
                    .map(|gain| (gain, measurement.replay_gain_peak))
            }),
    }
}

fn clipping_safe_gain_db(gain_db: f64, peak: Option<f64>) -> f64 {
    let peak = peak
        .filter(|peak| peak.is_finite() && *peak > 0.0)
        .map_or(1.0, |peak| peak.min(1.0));
    gain_db.min(-20.0 * peak.log10())
}

fn tempo_enabled(settings: &BackendAudioSettings, playback_rate: f64) -> bool {
    settings.preserve_pitch && (playback_rate - DEFAULT_PLAYBACK_RATE).abs() > f64::EPSILON
}

#[cfg(test)]
fn configure_test_output(output: &gst::Element) {
    if output
        .factory()
        .is_some_and(|factory| factory.name() == "fakesink")
    {
        // Keep normal sink preroll: ASYNC_DONE must mean decoded audio is ready,
        // including when a paused session is restored at a nonzero position.
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

    fn measurement(integrated_lufs: f64, true_peak: Option<f64>) -> LoudnessMeasurement {
        LoudnessMeasurement {
            analysis_key: [1; 32],
            integrated_lufs: Some(integrated_lufs),
            true_peak,
            replay_gain_db: None,
            replay_gain_peak: None,
        }
    }

    fn test_stream(loudness: TrackLoudness) -> SharedQueuedStream {
        Arc::new(Mutex::new(PreparedStream::new(
            playback::ResolvedStream::new("file:///test.flac"),
            loudness,
        )))
    }

    fn empty_stream() -> SharedQueuedStream {
        test_stream(TrackLoudness::default())
    }

    fn test_graph(
        settings: &BackendAudioSettings,
        playback_rate: f64,
        stream: SharedQueuedStream,
    ) -> Result<AudioGraph, String> {
        let current_loudness = stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .loudness
            .clone();
        AudioGraph::new(settings, playback_rate, current_loudness, stream)
    }

    #[test]
    fn large_audio_packets_keep_every_sample_and_the_short_final_block() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            audio_output: Some("appsink".to_string()),
            loudness_normalization: LoudnessNormalization::Off,
            ..BackendAudioSettings::default()
        };
        let graph =
            test_graph(&settings, DEFAULT_PLAYBACK_RATE, empty_stream()).expect("audio graph");
        let sink = graph.output.clone().downcast::<gst_app::AppSink>().unwrap();
        let source = gst_app::AppSrc::builder()
            .caps(&sink.caps().unwrap())
            .format(gst::Format::Time)
            .build();
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([source.upcast_ref(), graph.root(), graph.output()])
            .unwrap();
        source.link(graph.root()).unwrap();
        graph.root().link(graph.output()).unwrap();
        let expected = (0..4_017)
            .flat_map(|index| ((index as f32 - 2_000.0) / 8_000.0).to_le_bytes())
            .collect::<Vec<_>>();
        let mut buffer = gst::Buffer::from_slice(expected.clone());
        {
            let buffer = buffer.get_mut().unwrap();
            buffer.set_pts(gst::ClockTime::ZERO);
            buffer.set_duration(gst::ClockTime::from_useconds(502_125));
        }
        pipeline.set_state(gst::State::Playing).unwrap();
        source.push_buffer(buffer).unwrap();
        source.end_of_stream().unwrap();
        let mut actual = Vec::new();
        let mut end = gst::ClockTime::ZERO;
        let mut blocks = 0;
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
            let buffer = sample.buffer().unwrap();
            assert_eq!(buffer.pts(), Some(end));
            let duration = buffer.duration().unwrap();
            assert!(duration <= gst::ClockTime::from_mseconds(17));
            end += duration;
            actual.extend_from_slice(buffer.map_readable().unwrap().as_slice());
            blocks += 1;
        }
        let eos = sink.is_eos();
        pipeline.set_state(gst::State::Null).unwrap();
        assert!(eos);
        assert_eq!(blocks, 31);
        assert_eq!(actual, expected);
        assert_eq!(end, gst::ClockTime::from_useconds(502_125));
    }

    #[test]
    fn selected_normalizer_owns_tracks_with_both_fact_families() {
        initialize_gstreamer();
        let loudness = TrackLoudness {
            track: Some(Box::new(LoudnessMeasurement {
                analysis_key: [1; 32],
                integrated_lufs: Some(-20.0),
                true_peak: Some(0.8),
                replay_gain_db: Some(2.0),
                replay_gain_peak: Some(0.9),
            })),
            album: None,
        };

        let replay = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::ReplayGain,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            ..BackendAudioSettings::default()
        };
        assert_eq!(
            selected_loudness(&replay, &loudness),
            Some((2.0, Some(0.9)))
        );

        let ebu = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            ebu_r128_target_lufs: -23.0,
            ..BackendAudioSettings::default()
        };
        assert_eq!(selected_loudness(&ebu, &loudness), Some((-3.0, Some(0.8))));
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
        let mut graph = test_graph(&settings, DEFAULT_PLAYBACK_RATE, empty_stream())
            .expect("Pulse output graph");
        let mut changed = settings;
        changed.audio_output = Some(audio_output_device_id("alsa_output.selected"));

        assert!(
            graph
                .reconfigure(&changed, DEFAULT_PLAYBACK_RATE)
                .expect("retarget output")
        );
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
    fn disabled_equalizer_is_absent_and_enabling_updates_the_audio_graph() {
        initialize_gstreamer();
        let disabled = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let mut graph = test_graph(&disabled, DEFAULT_PLAYBACK_RATE, empty_stream())
            .expect("audio graph without equalizer");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-equalizer").is_none());

        let mut enabled = disabled;
        enabled.equalizer.enabled = true;
        assert!(
            graph
                .reconfigure(&enabled, DEFAULT_PLAYBACK_RATE)
                .expect("equalizer activation boundary")
        );
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-equalizer").is_some());
    }

    #[test]
    fn loudness_scope_and_ebu_target_update_the_audio_graph() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let mut graph =
            test_graph(&settings, DEFAULT_PLAYBACK_RATE, empty_stream()).expect("audio graph");

        let album = BackendAudioSettings {
            loudness_normalization_scope: LoudnessNormalizationScope::Album,
            ..settings.clone()
        };
        assert!(graph.reconfigure(&album, DEFAULT_PLAYBACK_RATE).unwrap());

        let target = BackendAudioSettings {
            ebu_r128_target_lufs: -18.0,
            ..settings
        };
        assert!(graph.reconfigure(&target, DEFAULT_PLAYBACK_RATE).unwrap());
    }

    #[test]
    fn preserve_pitch_controls_the_scaletempo_stage() {
        initialize_gstreamer();
        let enabled = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            preserve_pitch: true,
            ..BackendAudioSettings::default()
        };
        let graph = test_graph(&enabled, DEFAULT_PLAYBACK_RATE, empty_stream())
            .expect("normal-speed audio graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-playback-rate").is_none());

        let mut graph =
            test_graph(&enabled, 1.25, empty_stream()).expect("pitch-preserving audio graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-playback-rate").is_some());

        let disabled = BackendAudioSettings {
            preserve_pitch: false,
            ..enabled
        };
        assert!(
            graph
                .reconfigure(&disabled, 1.25)
                .expect("pitch preservation configuration change")
        );
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        assert!(bin.by_name("rufin-playback-rate").is_none());
    }

    #[test]
    fn stored_r128_measurement_uses_the_direct_ebu_normalizer() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Album,
            ebu_r128_target_lufs: -18.0,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let loudness = TrackLoudness {
            track: Some(Box::new(measurement(-21.0, Some(0.4)))),
            album: Some(Box::new(measurement(-23.0, Some(0.8)))),
        };
        let graph = test_graph(&settings, DEFAULT_PLAYBACK_RATE, test_stream(loudness))
            .expect("album normalization graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        let volume = bin
            .by_name("rufin-loudness-normalization")
            .expect("direct loudness normalization volume");

        assert_eq!(
            volume.factory().map(|factory| factory.name().to_string()),
            Some("volume".to_string())
        );
        assert!((volume.property::<f64>("volume") - 1.25).abs() < 0.001);
    }

    #[test]
    fn provider_replaygain_uses_direct_gain_and_peak_protection() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::ReplayGain,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let graph = test_graph(
            &settings,
            DEFAULT_PLAYBACK_RATE,
            test_stream(TrackLoudness {
                track: Some(Box::new(LoudnessMeasurement {
                    analysis_key: [1; 32],
                    integrated_lufs: None,
                    true_peak: None,
                    replay_gain_db: Some(6.0),
                    replay_gain_peak: Some(0.8),
                })),
                album: None,
            }),
        )
        .expect("provider ReplayGain graph");
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        let volume = bin
            .by_name("rufin-loudness-normalization")
            .expect("direct provider ReplayGain volume");

        assert_eq!(
            volume.factory().map(|factory| factory.name().to_string()),
            Some("volume".to_string())
        );
        assert!((volume.property::<f64>("volume") - 1.25).abs() < 0.001);
    }

    #[test]
    fn stored_r128_gain_reaches_the_direct_normalizer_before_audio() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let graph = test_graph(
            &settings,
            DEFAULT_PLAYBACK_RATE,
            test_stream(TrackLoudness {
                track: Some(Box::new(measurement(-28.0, Some(0.1)))),
                album: None,
            }),
        )
        .expect("normalization graph");
        let observed_gain = observe_direct_gains(&graph, |_| {})[0];
        assert!((observed_gain - 10_f64.powf(5.0 / 20.0)).abs() < 0.001);
    }

    #[test]
    fn preparing_gapless_loudness_does_not_change_the_current_stream() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let stream = test_stream(TrackLoudness {
            track: Some(Box::new(measurement(-28.0, Some(0.1)))),
            album: None,
        });
        let graph = test_graph(&settings, DEFAULT_PLAYBACK_RATE, Arc::clone(&stream))
            .expect("normalization graph");
        let next = TrackLoudness {
            track: Some(Box::new(measurement(-18.0, Some(1.0)))),
            album: None,
        };
        let observed = observe_direct_gains(&graph, move |index| {
            if index == 0 {
                stream.lock().expect("prepared stream").loudness = next.clone();
            }
        });
        assert!(observed.len() >= 2, "observed gains: {observed:?}");
        let current_gain = 10_f64.powf(5.0 / 20.0);
        assert!(
            observed
                .iter()
                .all(|gain| (*gain - current_gain).abs() < 0.001),
            "gapless preparation changed the current stream: {observed:?}"
        );
    }

    #[test]
    fn stream_start_applies_staged_direct_gain_before_audio() {
        initialize_gstreamer();
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::EbuR128,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let stream = test_stream(TrackLoudness {
            track: Some(Box::new(measurement(-28.0, Some(0.1)))),
            album: None,
        });
        let graph = test_graph(&settings, DEFAULT_PLAYBACK_RATE, Arc::clone(&stream))
            .expect("normalization graph");
        stream.lock().expect("prepared stream").loudness = TrackLoudness {
            track: Some(Box::new(measurement(-18.0, Some(1.0)))),
            album: None,
        };
        let observed = observe_direct_gains(&graph, |_| {})[0];
        assert!((observed - 10_f64.powf(-5.0 / 20.0)).abs() < 0.001);
    }

    fn observe_direct_gains(
        graph: &AudioGraph,
        on_buffer: impl Fn(usize) + Send + Sync + 'static,
    ) -> Vec<f64> {
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("num-buffers", 4_i32)
            .build()
            .expect("test audio source");
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&source, graph.root(), graph.output()])
            .expect("test normalization pipeline");
        source
            .link(graph.root())
            .expect("test normalization pipeline link");
        graph.root().link(graph.output()).unwrap();
        let bin = graph.root.downcast_ref::<gst::Bin>().expect("audio bin");
        let volume = bin
            .by_name("rufin-loudness-normalization")
            .expect("loudness normalization volume element");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_for_probe = Arc::clone(&observed);
        let volume_for_probe = volume.clone();
        volume
            .static_pad("src")
            .expect("loudness normalization output pad")
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                let index = {
                    let mut observed = observed_for_probe.lock().expect("observed gains");
                    let index = observed.len();
                    observed.push(volume_for_probe.property::<f64>("volume"));
                    index
                };
                on_buffer(index);
                gst::PadProbeReturn::Ok
            });

        pipeline
            .set_state(gst::State::Playing)
            .expect("start test normalization pipeline");
        let bus = pipeline.bus().expect("test normalization bus");
        loop {
            let message = bus
                .timed_pop(gst::ClockTime::from_seconds(5))
                .expect("test normalization pipeline completion");
            match message.view() {
                gst::MessageView::Eos(..) => break,
                gst::MessageView::Error(error) => panic!("{}", error.error()),
                _ => {}
            }
        }
        pipeline
            .set_state(gst::State::Null)
            .expect("stop test normalization pipeline");
        let values = observed.lock().expect("observed gains").clone();
        values
    }

    #[test]
    fn switching_stored_and_embedded_gain_preserves_the_first_samples() {
        initialize_gstreamer();
        let loudness = TrackLoudness {
            track: Some(Box::new(LoudnessMeasurement {
                analysis_key: [0; 32],
                integrated_lufs: None,
                true_peak: None,
                replay_gain_db: Some(-6.020599913279624),
                replay_gain_peak: Some(0.5),
            })),
            album: None,
        };
        let stream = test_stream(loudness.clone());
        let settings = BackendAudioSettings {
            loudness_normalization: LoudnessNormalization::ReplayGain,
            loudness_normalization_scope: LoudnessNormalizationScope::Track,
            audio_output: Some("appsink".into()),
            ..BackendAudioSettings::default()
        };
        let graph = test_graph(&settings, DEFAULT_PLAYBACK_RATE, Arc::clone(&stream)).unwrap();
        let source = gst::ElementFactory::make("audiotestsrc")
            .property("num-buffers", 4_i32)
            .property("samplesperbuffer", 800_i32)
            .property_from_str("wave", "square")
            .property("freq", 100_f64)
            .property("volume", 0.5_f64)
            .build()
            .unwrap();
        let index = std::sync::atomic::AtomicUsize::new(0);
        source
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |pad, _| {
                let index = index.fetch_add(1, Ordering::Relaxed);
                if index > 0 {
                    stream.lock().unwrap().loudness = if index == 1 || index == 3 {
                        TrackLoudness::default()
                    } else {
                        loudness.clone()
                    };
                    pad.push_event(
                        gst::event::StreamStart::builder(&format!("gain-{index}"))
                            .group_id(gst::GroupId::next())
                            .build(),
                    );
                }
                if index == 3 {
                    return gst::PadProbeReturn::Ok;
                }
                let mut tags = gst::TagList::new();
                tags.get_mut()
                    .unwrap()
                    .add::<gst::tags::TrackGain>(&-12.041199826559248, gst::TagMergeMode::Replace);
                tags.get_mut()
                    .unwrap()
                    .add::<gst::tags::TrackPeak>(&0.5, gst::TagMergeMode::Replace);
                tags.get_mut()
                    .unwrap()
                    .add::<gst::tags::ReferenceLevel>(&-18.0, gst::TagMergeMode::Replace);
                pad.push_event(gst::event::Tag::new(tags));
                gst::PadProbeReturn::Ok
            });
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&source, graph.root(), graph.output()])
            .unwrap();
        let caps = gst::Caps::builder("audio/x-raw")
            .field("rate", 8000_i32)
            .field("channels", 1_i32)
            .build();
        source.link_filtered(graph.root(), &caps).unwrap();
        graph.root().link(graph.output()).unwrap();
        let sink = graph.output.clone().downcast::<gst_app::AppSink>().unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let mut samples = Vec::new();
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(5)) {
            let map = sample.buffer().unwrap().map_readable().unwrap();
            samples.extend(
                map.as_slice()
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
            );
        }
        pipeline.set_state(gst::State::Null).unwrap();
        assert_eq!(samples.len(), 3200);
        for (block, expected) in samples.chunks_exact(800).zip([0.25, 0.125, 0.25, 0.5]) {
            assert!(
                block
                    .iter()
                    .all(|sample| (sample.abs() - expected).abs() < 0.00001),
                "gain changed within a track: first={}, last={}, expected={expected}",
                block[0],
                block[799]
            );
        }
    }

    #[test]
    fn embedded_gain_respects_scope_reference_levels_and_peak_protection() {
        initialize_gstreamer();
        // Older rgvolume versions mistake negative LUFS references for dBSPL,
        // so the installed plugin is not a portable oracle for these gains.
        let cases: &[(&[(&str, f64)], [f64; 2])] = &[
            (&[], [0.0, 0.0]),
            (&[("replaygain-track-gain", -6.0)], [-6.0, -6.0]),
            (&[("replaygain-album-gain", -4.0)], [-4.0, -4.0]),
            (
                &[
                    ("replaygain-track-gain", -6.0),
                    ("replaygain-album-gain", -4.0),
                ],
                [-6.0, -4.0],
            ),
            (
                &[
                    ("replaygain-track-gain", 6.0),
                    ("replaygain-track-peak", 0.8),
                ],
                [1.938200260161128, 1.938200260161128],
            ),
            (
                &[
                    ("replaygain-track-gain", -3.0),
                    ("replaygain-reference-level", 83.0),
                ],
                [0.0, 0.0],
            ),
            (
                &[
                    ("replaygain-track-gain", -3.0),
                    ("replaygain-reference-level", 83.0),
                    ("replaygain-track-peak", 0.5),
                ],
                [3.0, 3.0],
            ),
            (
                &[
                    ("replaygain-track-gain", -3.0),
                    ("replaygain-reference-level", -18.0),
                ],
                [-3.0, -3.0],
            ),
        ];
        for (case, expected) in cases {
            for (scope, expected) in [
                LoudnessNormalizationScope::Track,
                LoudnessNormalizationScope::Album,
            ]
            .into_iter()
            .zip(expected)
            {
                let mut tags = gst::TagList::new();
                for (name, value) in *case {
                    tags.make_mut()
                        .add_generic(*name, *value, gst::TagMergeMode::Replace)
                        .unwrap();
                }
                let actual = embedded_replaygain(&tags, scope);
                assert!(
                    (actual - expected).abs() < 0.00001,
                    "{case:?}, {scope:?}: expected {expected}, got {actual}"
                );
            }
        }
    }

    fn equalizer_band_gain(equalizer: &gst::Element, index: usize) -> Option<f64> {
        equalizer
            .dynamic_cast_ref::<gst::ChildProxy>()?
            .child_by_index(index as u32)
            .map(|band| band.property::<f64>("gain"))
    }
}
