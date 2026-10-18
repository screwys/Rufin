use super::*;
use gst::subclass::prelude::*;
use std::sync::{LazyLock, OnceLock};

const FACTORY: &str = "rufinfileaudioparse";
// MIDI enters before midiparse so each file also owns its event renderer.
const INPUTS: &[&str] = &[
    "gmedec",
    "openmptdec",
    "modplug",
    "sfdec",
    "siddec",
    "wildmididec",
    "midiparse",
];

fn input_caps() -> gst::Caps {
    let mut caps = gst::Caps::new_empty();
    for name in INPUTS {
        if let Some(factory) = gst::ElementFactory::find(name) {
            for pad in factory.static_pad_templates() {
                if pad.direction() == gst::PadDirection::Sink {
                    caps.merge(pad.caps());
                }
            }
        }
    }
    caps
}

pub(super) fn register() -> Result<(), String> {
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            if input_caps().is_empty() {
                return Ok(());
            }
            let rank = INPUTS
                .iter()
                .filter_map(|name| gst::ElementFactory::find(name))
                .map(|factory| factory.rank())
                .max()
                .unwrap()
                + 1;
            gst::Element::register(None, FACTORY, rank, FileAudioParse::static_type())
                .map_err(|error| error.to_string())
        })
        .clone()
}

glib::wrapper! {
    pub struct FileAudioParse(ObjectSubclass<imp::FileAudioParse>)
        @extends gst::Bin, gst::Element, gst::Object;
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileAudioParse;

    #[glib::object_subclass]
    impl ObjectSubclass for FileAudioParse {
        const NAME: &'static str = "RufinFileAudioParse";
        type Type = super::FileAudioParse;
        type ParentType = gst::Bin;
    }

    impl ObjectImpl for FileAudioParse {
        fn constructed(&self) {
            self.parent_constructed();
            let bin = self.obj();
            // Keep file decoders and MIDI renderers with their URI and native
            // pull scheduling. Input EOS must not let playbin3 reuse a decoder
            // that is still producing audio.
            let decoder = gst::ElementFactory::make("decodebin")
                .build()
                .expect("GStreamer decodebin");
            decoder
                .downcast_ref::<gst::Bin>()
                .unwrap()
                .connect_element_added(|_, element| {
                    if let Some(factory) = element.factory() {
                        match factory.name().as_str() {
                            "gmedec" | "midiparse" => report_time_seeking(element),
                            "modplug" => {
                                report_time_seeking(element);
                                configure_modplug(element);
                            }
                            _ => {}
                        }
                    }
                });
            bin.add(&decoder).expect("file audio decoder child");
            let input = gst::GhostPad::with_target(&decoder.static_pad("sink").unwrap()).unwrap();
            bin.add_pad(&input).unwrap();
            let output = gst::GhostPad::builder(gst::PadDirection::Src)
                .name("src")
                .build();
            bin.add_pad(&output).unwrap();

            decoder.connect("autoplug-select", false, |values| {
                let factory = values[3].get::<gst::ElementFactory>().unwrap();
                let selection = glib::EnumClass::with_type(
                    glib::Type::from_name("GstAutoplugSelectResult").unwrap(),
                )
                .unwrap();
                selection.to_value_by_nick(if factory.name() == FACTORY {
                    "skip"
                } else {
                    "try"
                })
            });
            let output = output.downgrade();
            decoder.connect_pad_added(move |_, pad| {
                normalize_frames(pad);
                if let Some(output) = output.upgrade() {
                    output.set_target(Some(pad)).unwrap();
                }
            });
        }
    }

    impl GstObjectImpl for FileAudioParse {}
    impl BinImpl for FileAudioParse {}
    impl ElementImpl for FileAudioParse {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
                gst::subclass::ElementMetadata::new(
                    "File audio parser",
                    "Codec/Parser/Audio",
                    "Parse file audio into timed samples for URI playback",
                    "Rufin",
                )
            });
            Some(&METADATA)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static PADS: LazyLock<Vec<gst::PadTemplate>> = LazyLock::new(|| {
                [
                    ("sink", gst::PadDirection::Sink, input_caps()),
                    (
                        "src",
                        gst::PadDirection::Src,
                        gst::Caps::builder("audio/x-raw").build(),
                    ),
                ]
                .into_iter()
                .map(|(name, direction, caps)| {
                    gst::PadTemplate::new(name, direction, gst::PadPresence::Always, &caps).unwrap()
                })
                .collect()
            });
            &PADS
        }
    }
}

fn report_time_seeking(element: &gst::Element) {
    // These loaders implement TIME seeks but do not advertise that capability.
    // Handle requests before a native reply can change the query's format.
    element.static_pad("src").unwrap().add_probe(
        gst::PadProbeType::QUERY_UPSTREAM | gst::PadProbeType::PUSH,
        |pad, probe| {
            if let Some(query) = probe.query_mut()
                && let gst::QueryViewMut::Seeking(query) = query.view_mut()
                && query.format() == gst::Format::Time
            {
                let end = pad.query_duration::<gst::ClockTime>();
                query.set(true, gst::ClockTime::ZERO, end);
                return gst::PadProbeReturn::Handled;
            }
            gst::PadProbeReturn::Ok
        },
    );
}

#[allow(unsafe_code)]
fn configure_modplug(element: &gst::Element) {
    let pad = element.static_pad("src").expect("ModPlug output");
    // SAFETY: element-added runs before this child is activated. ModPlug uses
    // default source activation; keep that push activation and supply its
    // missing stream identity before it emits caps or audio.
    unsafe {
        pad.set_activate_function(|pad, parent| {
            pad.activate_mode(gst::PadMode::Push, true)?;
            let parent = parent.unwrap().downcast_ref::<gst::Element>().unwrap();
            let id = pad.create_stream_id(parent, None);
            pad.push_event(
                gst::event::StreamStart::builder(&id)
                    .group_id(gst::GroupId::next())
                    .build(),
            );
            Ok(())
        });
    }
    pad.add_probe(gst::PadProbeType::EVENT_DOWNSTREAM, |_, probe| {
        if let Some(event) = probe.event_mut()
            && let gst::EventView::Segment(value) = event.view()
            && let Some(segment) = value.segment().downcast_ref::<gst::ClockTime>()
        {
            // ModPlug reports a rounded GetSongTime estimate as its stop. It
            // rejects explicit stop seeks; actual audio EOS owns this boundary.
            let mut segment = segment.clone();
            segment.set_stop(gst::ClockTime::NONE);
            event.make_mut().structure_mut().set("segment", segment);
        }
        gst::PadProbeReturn::Ok
    });
}

fn normalize_frames(pad: &gst::Pad) {
    let audio = Mutex::new(None::<gstreamer_audio::AudioInfo>);
    pad.add_probe(
        gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
        move |_, probe| {
            let mut audio = lock_recover(&audio);
            if let Some(event) = probe.event() {
                if let gst::EventView::Caps(caps) = event.view() {
                    *audio = Some(gstreamer_audio::AudioInfo::from_caps(caps.caps()).unwrap());
                }
            } else if let Some(buffer) = probe.buffer_mut()
                && let Some(audio) = audio.as_ref()
            {
                let mut frames = buffer.size() as u64 / u64::from(audio.bpf());
                // OpenMPT and libsndfile can leave unused final-buffer space.
                // Their frame offsets or duration identify the valid samples.
                if buffer.offset() != gst::format::Buffers::OFFSET_NONE
                    && buffer.offset_end() != gst::format::Buffers::OFFSET_NONE
                {
                    frames = frames.min(buffer.offset_end() - buffer.offset());
                } else if let Some(duration) = buffer.duration() {
                    frames = frames.min(
                        (duration.nseconds() * u64::from(audio.rate())
                            + gst::ClockTime::SECOND.nseconds() / 2)
                            / gst::ClockTime::SECOND.nseconds(),
                    );
                }
                let buffer = buffer.make_mut();
                buffer.set_size(frames as usize * audio.bpf() as usize);
                // GME omits durations needed by URI boundary synchronization.
                if buffer.duration().is_none() {
                    buffer.set_duration(gst::ClockTime::from_nseconds(
                        frames * gst::ClockTime::SECOND.nseconds() / u64::from(audio.rate()),
                    ));
                }
            }
            gst::PadProbeReturn::Ok
        },
    );
}
