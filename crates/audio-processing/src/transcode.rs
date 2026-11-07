use std::io::{self, Cursor, Read};
use std::path::Path;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use crate::ResolvedStream;
use crate::effects::{EffectsBin, ProcessingConfig};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use library::{AudioCodec, AudioEncoding};

use super::{configure_sources, ensure_gstreamer_initialized};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

#[derive(Clone)]
pub struct TranscodeOptions {
    pub encoding: AudioEncoding,
    pub processing: Option<ProcessingConfig>,
    pub start_offset_millis: u64,
    pub duration_millis: Option<u64>,
    pub visualizer_fft_size: Option<usize>,
}

impl From<AudioEncoding> for TranscodeOptions {
    fn from(encoding: AudioEncoding) -> Self {
        Self {
            encoding,
            processing: None,
            start_offset_millis: 0,
            duration_millis: None,
            visualizer_fft_size: None,
        }
    }
}

pub struct VisualizerFrame {
    pub position_millis: u64,
    pub levels: Vec<f64>,
}

struct TranscodeVisualizer {
    samples: Receiver<(u64, Vec<f32>, u32)>,
    fft: crate::VisualizerFft,
    next_millis: u64,
}

struct TranscodePipeline {
    effects: Option<EffectsBin>,
    applied: Option<(ProcessingConfig, u32, f64)>,
    peak_db: Vec<f64>,
    element: gst::Element,
    bus: gst::Bus,
    visualizer: Option<TranscodeVisualizer>,
    _stream: ResolvedStream,
}

impl TranscodePipeline {
    fn new(
        stream: &ResolvedStream,
        options: TranscodeOptions,
        output: Option<&Path>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        ensure_gstreamer_initialized()?;
        let encoding = options.encoding;
        let start = stream
            .start_millis()
            .saturating_add(options.start_offset_millis);
        let end = stream.end_millis();
        let duration = end
            .map(|end| end.saturating_sub(start))
            .or(options.duration_millis);
        let bitrate = encoding
            .bitrate_kbps
            .map(|value| value.saturating_mul(1000));
        let encoder = match encoding.codec {
            AudioCodec::Opus => format!("opusenc bitrate={} ! oggmux", bitrate.unwrap_or(128000)),
            AudioCodec::Vorbis => "vorbisenc name=encoder ! oggmux".into(),
            AudioCodec::Aac => format!(
                "avenc_aac bitrate={} ! aacparse ! audio/mpeg,mpegversion=4,stream-format=adts",
                bitrate.unwrap_or(128000)
            ),
            AudioCodec::Flac => "flacenc".into(),
            AudioCodec::Mp3 => {
                let encoder = ["lamemp3enc", "avenc_mp3"]
                    .into_iter()
                    .find(|name| gst::ElementFactory::find(name).is_some())
                    .ok_or("GStreamer MP3 encoding support is unavailable")?;
                match encoding.bitrate_kbps {
                    Some(value) if encoder == "lamemp3enc" => {
                        format!("{encoder} target=bitrate bitrate={value}")
                    }
                    Some(value) => format!("{encoder} bitrate={}", value.saturating_mul(1000)),
                    None => encoder.into(),
                }
            }
        };
        let mut raw = Vec::new();
        if encoding.codec != AudioCodec::Flac {
            raw.push("rate=48000");
        } else {
            raw.push("format=S24LE");
        }
        if encoding.stereo() {
            raw.push("channels=2");
        }
        let raw = if raw.is_empty() {
            String::new()
        } else {
            format!("audio/x-raw,{} ! ", raw.join(","))
        };
        let sink = if output.is_some() {
            "filesink"
        } else {
            "appsink max-buffers=8 drop=false"
        };
        let element = gst::parse::launch(&format!(
            "uridecodebin name=decoder ! identity name=incoming audioconvert name=outgoing ! audioresample ! {raw}identity name=bounds single-segment=true ! {encoder} ! {sink} name=sink sync=false"
        )).map_err(|error| error.to_string())?;
        let bin = element
            .downcast_ref::<gst::Bin>()
            .ok_or("Audio encoder is not a bin")?;
        let decoder = bin
            .by_name("decoder")
            .ok_or("Audio encoder has no decoder")?;
        if let Some(encoder) = bin.by_name("encoder") {
            let weak = encoder.downgrade();
            encoder.static_pad("sink").expect("Vorbis input").add_probe(
                gst::PadProbeType::EVENT_DOWNSTREAM,
                move |_, info| {
                    if let Some(gst::EventView::Caps(caps)) = info.event().map(|event| event.view())
                        && let Some(channels) = caps
                            .caps()
                            .structure(0)
                            .and_then(|caps| caps.get::<i32>("channels").ok())
                        && let Some(encoder) = weak.upgrade()
                    {
                        encoder.set_property(
                            "quality",
                            vorbis_quality(bitrate.unwrap_or(128000), channels),
                        );
                    }
                    gst::PadProbeReturn::Ok
                },
            );
        }
        let incoming = bin
            .by_name("incoming")
            .ok_or("Audio encoder has no input")?;
        let outgoing = bin
            .by_name("outgoing")
            .ok_or("Audio encoder has no output conversion")?;
        let mut effects = options
            .processing
            .as_ref()
            .map(EffectsBin::new)
            .transpose()?;
        if let Some(effects) = &mut effects {
            effects.set_meter(true)?;
        }
        if let Some(effects) = &effects {
            bin.add(effects.element())
                .map_err(|error| error.to_string())?;
            gst::Element::link_many([&incoming, effects.element(), &outgoing])
                .map_err(|error| error.to_string())?;
        } else {
            incoming
                .link(&outgoing)
                .map_err(|error| error.to_string())?;
        }
        let boundary = incoming
            .static_pad("sink")
            .ok_or("Audio encoder has no input pad")?;
        let (ready, first_buffer) = std::sync::mpsc::sync_channel(1);
        let hold = (start > 0 || end.is_some()).then(|| {
            boundary
                .add_probe(
                    gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER,
                    move |_, _| {
                        let _ = ready.try_send(());
                        gst::PadProbeReturn::Ok
                    },
                )
                .expect("source buffer block")
        });
        bin.by_name("bounds")
            .expect("encoder bounds")
            .static_pad("src")
            .expect("encoder input")
            .add_probe(gst::PadProbeType::QUERY_UPSTREAM, move |pad, info| {
                if let Some(gst::QueryViewMut::Duration(query)) =
                    info.query_mut().map(|query| query.view_mut())
                {
                    let format = query.format();
                    if matches!(format, gst::Format::Time | gst::Format::Default) {
                        let value = duration
                            .and_then(|millis| {
                                if format == gst::Format::Time {
                                    i64::try_from(millis.saturating_mul(1_000_000)).ok()
                                } else {
                                    let rate = pad
                                        .current_caps()?
                                        .structure(0)?
                                        .get::<i32>("rate")
                                        .ok()?;
                                    i64::try_from(millis.saturating_mul(rate as u64) / 1000).ok()
                                }
                            })
                            .unwrap_or(-1);
                        query.set(gst::GenericFormattedValue::new(format, value));
                        return gst::PadProbeReturn::Handled;
                    }
                }
                gst::PadProbeReturn::Ok
            });
        let visualizer = options.visualizer_fft_size.map(|size| {
            let (send, samples) = sync_channel(8);
            bin.by_name("bounds")
                .expect("encoder bounds")
                .static_pad("src")
                .expect("encoder input")
                .add_probe(gst::PadProbeType::BUFFER, move |pad, probe| {
                    if let Some(buffer) = probe.buffer()
                        && let Some(position) = buffer.pts()
                        && let Some((samples, rate)) =
                            crate::copy_visualizer_samples(pad, buffer, size)
                    {
                        let _ = send.try_send((position.mseconds(), samples, rate));
                    }
                    gst::PadProbeReturn::Ok
                });
            TranscodeVisualizer {
                samples,
                fft: crate::VisualizerFft::with_size(size),
                next_millis: 0,
            }
        });
        let trust = stream.trust_invalid_certificate();
        configure_sources(&decoder, move || trust);
        decoder.set_property("uri", stream.uri());
        if let Some(path) = output {
            let uri = gst::glib::filename_to_uri(path, None).map_err(|error| error.to_string())?;
            bin.by_name("sink")
                .ok_or("Audio encoder has no output")?
                .dynamic_cast::<gst::URIHandler>()
                .map_err(|_| "Audio output cannot open a file")?
                .set_uri(uri.as_str())
                .map_err(|error| error.to_string())?;
        }
        let bus = element.bus().ok_or("Audio encoder has no bus")?;
        let mut pipeline = Self {
            effects,
            applied: None,
            peak_db: Vec::new(),
            element,
            bus,
            visualizer,
            _stream: stream.clone(),
        };
        pipeline
            .element
            .set_state(if hold.is_some() {
                gst::State::Paused
            } else {
                gst::State::Playing
            })
            .map_err(|error| error.to_string())?;
        if let Some(hold) = hold {
            let started = Instant::now();
            loop {
                if cancelled() {
                    return Err("Transcoding cancelled".into());
                }
                if started.elapsed() >= STARTUP_TIMEOUT {
                    return Err("Audio encoder did not prepare the source".into());
                }
                pipeline.poll_effects()?;
                if first_buffer
                    .recv_timeout(Duration::from_millis(100))
                    .is_ok()
                {
                    break;
                }
                if let Some(message) = pipeline
                    .bus
                    .pop_filtered(&[gst::MessageType::Error, gst::MessageType::Eos])
                {
                    match message.view() {
                        gst::MessageView::Error(error) => return Err(error.error().to_string()),
                        gst::MessageView::Eos(..) => {
                            return Err("Audio source ended before its requested segment".into());
                        }
                        _ => {}
                    }
                }
            }
            pipeline
                .element
                .seek(
                    1.0,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(start),
                    if end.is_some() {
                        gst::SeekType::Set
                    } else {
                        gst::SeekType::None
                    },
                    end.map(gst::ClockTime::from_mseconds),
                )
                .map_err(|error| error.to_string())?;
            boundary.remove_probe(hold);
            pipeline
                .element
                .set_state(gst::State::Playing)
                .map_err(|error| error.to_string())?;
        }
        Ok(pipeline)
    }

    fn poll_effects(&mut self) -> Result<(), String> {
        if let Some(effects) = &mut self.effects
            && let Some((config, Some(rate), preamp)) = effects.poll()?
        {
            self.applied = Some((config, rate, preamp));
        }
        Ok(())
    }
}

fn vorbis_quality(bitrate: u32, channels: i32) -> f32 {
    // Xiph's 44.1/48 kHz nominal rates per channel, from setup_44.h and setup_44u.h.
    let rates = if channels == 2 {
        [
            22.5, 32., 40., 48., 56., 64., 80., 96., 112., 128., 160., 250.,
        ]
    } else {
        [
            32., 48., 60., 70., 80., 86., 96., 110., 120., 140., 160., 240.,
        ]
    };
    let rate = bitrate as f32 / 1000. / channels.max(1) as f32;
    for (index, pair) in rates.windows(2).enumerate() {
        if rate < pair[1] {
            return ((index as f32 - 1. + (rate - pair[0]) / (pair[1] - pair[0])) / 10.).max(-0.1);
        }
    }
    1.
}

impl Drop for TranscodePipeline {
    fn drop(&mut self) {
        let _ = self.element.set_state(gst::State::Null);
    }
}

pub fn transcode_audio(
    stream: &ResolvedStream,
    options: TranscodeOptions,
    output: &Path,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    let mut pipeline =
        TranscodePipeline::new(stream, options, Some(output), &cancelled).map_err(|error| {
            if cancelled() {
                io::Error::new(io::ErrorKind::Interrupted, error)
            } else {
                io::Error::other(error)
            }
        })?;
    loop {
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Transcoding cancelled",
            ));
        }
        pipeline.poll_effects().map_err(io::Error::other)?;
        let Some(message) = pipeline.bus.timed_pop(POLL) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Eos(..) => return Ok(()),
            gst::MessageView::Error(error) => {
                let http_source = error
                    .src()
                    .and_then(|source| source.downcast_ref::<gst::Element>())
                    .and_then(|source| source.factory())
                    .is_some_and(|factory| factory.name() == "souphttpsrc");
                let error = error.error();
                let kind = match error.kind::<gst::ResourceError>() {
                    Some(gst::ResourceError::NotAuthorized) => io::ErrorKind::PermissionDenied,
                    Some(gst::ResourceError::NotFound) => io::ErrorKind::NotFound,
                    Some(gst::ResourceError::OpenRead | gst::ResourceError::Read)
                        if !stream.local_input =>
                    {
                        io::ErrorKind::ConnectionAborted
                    }
                    None if http_source
                        && !stream.local_input
                        && error.kind::<gst::StreamError>() == Some(gst::StreamError::Failed) =>
                    {
                        io::ErrorKind::ConnectionAborted
                    }
                    _ => io::ErrorKind::Other,
                };
                return Err(io::Error::new(kind, error));
            }
            _ => {}
        }
    }
}

pub struct TranscodedAudioReader {
    pipeline: TranscodePipeline,
    sink: gst_app::AppSink,
    current: Cursor<Vec<u8>>,
    finished: bool,
    cancelled: Box<dyn Fn() -> bool + Send>,
}

impl TranscodedAudioReader {
    pub fn new(
        stream: &ResolvedStream,
        options: TranscodeOptions,
        cancelled: impl Fn() -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let pipeline =
            TranscodePipeline::new(stream, options, None, &cancelled).map_err(|error| {
                if cancelled() {
                    io::Error::new(io::ErrorKind::Interrupted, error)
                } else {
                    io::Error::other(error)
                }
            })?;
        let sink = pipeline
            .element
            .downcast_ref::<gst::Bin>()
            .and_then(|bin| bin.by_name("sink"))
            .ok_or_else(|| io::Error::other("Audio encoder has no output"))?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| io::Error::other("Audio encoder output has the wrong type"))?;
        Ok(Self {
            pipeline,
            sink,
            current: Cursor::new(Vec::new()),
            finished: false,
            cancelled: Box::new(cancelled),
        })
    }
    pub fn applied_processing(&self) -> Option<&(ProcessingConfig, u32, f64)> {
        self.pipeline.applied.as_ref()
    }

    pub fn encoding_peak_db(&self) -> &[f64] {
        &self.pipeline.peak_db
    }

    pub fn source_seekable(&self) -> Option<bool> {
        let mut query = gst::query::Seeking::new(gst::Format::Time);
        self.pipeline
            .element
            .query(&mut query)
            .then(|| query.result().0)
    }

    pub fn take_visualizer_frames(&mut self) -> Vec<VisualizerFrame> {
        let mut frames = Vec::new();
        if let Some(visualizer) = &mut self.pipeline.visualizer {
            for (position_millis, samples, rate) in visualizer.samples.try_iter() {
                visualizer.fft.push_samples(&samples);
                if position_millis >= visualizer.next_millis
                    && let Some(levels) = visualizer.fft.levels(rate)
                {
                    frames.push(VisualizerFrame {
                        position_millis,
                        levels,
                    });
                    visualizer.next_millis = position_millis.saturating_add(33);
                }
            }
        }
        frames
    }
}

impl Read for TranscodedAudioReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            if (self.cancelled)() {
                let _ = self.pipeline.element.set_state(gst::State::Null);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Transcoding cancelled",
                ));
            }
            self.pipeline.poll_effects().map_err(io::Error::other)?;
            while let Some(message) = self.pipeline.bus.pop() {
                if let gst::MessageView::Error(error) = message.view() {
                    return Err(io::Error::other(error.error().to_string()));
                }
                if let Some(peaks) = self
                    .pipeline
                    .effects
                    .as_ref()
                    .and_then(|effects| effects.meter_message(&message))
                {
                    if self.pipeline.peak_db.len() != peaks.len() {
                        self.pipeline.peak_db = peaks;
                    } else {
                        for (peak, next) in self.pipeline.peak_db.iter_mut().zip(peaks) {
                            *peak = peak.max(next);
                        }
                    }
                }
            }
            let read = self.current.read(output)?;
            if read > 0 || self.finished || output.is_empty() {
                return Ok(read);
            }
            if let Some(sample) = self.sink.try_pull_sample(POLL) {
                let buffer = sample
                    .buffer()
                    .ok_or_else(|| io::Error::other("Audio encoder produced no buffer"))?;
                let mapped = buffer
                    .map_readable()
                    .map_err(|_| io::Error::other("Encoded audio could not be read"))?;
                self.current = Cursor::new(mapped.as_slice().to_vec());
            } else if self.sink.is_eos() {
                self.finished = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use super::*;

    #[test]
    fn reader_and_file_encoders_produce_audio() {
        let directory = tempfile::tempdir().expect("transcode directory");
        let path = directory.path().join("tone.wav");
        write_silence_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("wave URI");
        let mut reader = TranscodedAudioReader::new(
            &ResolvedStream::new(uri.as_str()),
            AudioEncoding::mp3().into(),
            || false,
        )
        .expect("start MP3 transcode");
        let mut encoded = Vec::new();

        reader.read_to_end(&mut encoded).expect("read MP3 stream");

        assert!(encoded.len() > 100);
        assert_eq!(reader.source_seekable(), Some(true));

        for (rate, channels) in [
            (64, library::AudioChannels::Preserve),
            (144, library::AudioChannels::Stereo),
            (320, library::AudioChannels::Preserve),
        ] {
            for codec in [
                AudioCodec::Opus,
                AudioCodec::Aac,
                AudioCodec::Mp3,
                AudioCodec::Vorbis,
                AudioCodec::Flac,
            ] {
                let output = directory
                    .path()
                    .join(format!("encoded.{}", codec.extension()));
                transcode_audio(
                    &ResolvedStream::new(uri.as_str()),
                    AudioEncoding {
                        codec,
                        bitrate_kbps: Some(rate),
                        channels,
                    }
                    .into(),
                    &output,
                    || false,
                )
                .unwrap_or_else(|error| panic!("{codec:?}: {error}"));
                let decoder = gst::parse::launch(
                    "uridecodebin name=input ! audioconvert ! fakesink sync=false",
                )
                .unwrap();
                decoder
                    .downcast_ref::<gst::Bin>()
                    .unwrap()
                    .by_name("input")
                    .unwrap()
                    .set_property(
                        "uri",
                        gst::glib::filename_to_uri(&output, None).unwrap().as_str(),
                    );
                decoder.set_state(gst::State::Playing).unwrap();
                let result = decoder.bus().unwrap().timed_pop_filtered(
                    gst::ClockTime::from_seconds(5),
                    &[gst::MessageType::Error, gst::MessageType::Eos],
                );
                decoder.set_state(gst::State::Null).unwrap();
                assert!(
                    result
                        .is_some_and(|message| matches!(message.view(), gst::MessageView::Eos(_))),
                    "decode {codec:?}"
                );
            }
        }
    }

    #[test]
    fn reader_encodes_only_the_requested_source_window() {
        let directory = tempfile::tempdir().expect("transcode directory");
        let path = directory.path().join("cue-source.wav");
        write_silence_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("wave URI");
        let stream = ResolvedStream::new(uri.as_str()).with_window(20, 80);
        for codec in [AudioCodec::Mp3, AudioCodec::Flac] {
            let mut reader = TranscodedAudioReader::new(
                &stream,
                TranscodeOptions {
                    encoding: AudioEncoding {
                        codec,
                        ..AudioEncoding::mp3()
                    },
                    processing: Some(ProcessingConfig {
                        equalizer: Some(crate::effects::EqProfile {
                            bass_db: 3.0,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    start_offset_millis: 10,
                    duration_millis: None,
                    visualizer_fft_size: None,
                },
                || false,
            )
            .expect("start windowed transcode");
            let mut encoded = Vec::new();
            reader
                .read_to_end(&mut encoded)
                .expect("read encoded stream");
            assert!(encoded.len() > 50);
            assert_eq!(reader.source_seekable(), Some(true), "{codec:?}");
            assert_eq!(
                reader.applied_processing().map(|(_, rate, _)| *rate),
                Some(8000)
            );
            assert!(!reader.encoding_peak_db().is_empty());
            if codec == AudioCodec::Flac {
                assert_eq!(&encoded[..4], b"fLaC");
                let packed =
                    u64::from_be_bytes(encoded[18..26].try_into().expect("FLAC stream info"));
                assert_eq!(packed & 0xfffffffff, 400);
            }
        }
    }

    fn write_silence_wave(path: &std::path::Path) {
        let sample_rate = 8_000_u32;
        let samples = 800_u32;
        let data_size = samples * 2;
        let mut file = File::create(path).expect("create wave");
        file.write_all(b"RIFF").expect("RIFF");
        file.write_all(&(36 + data_size).to_le_bytes())
            .expect("wave size");
        file.write_all(b"WAVEfmt ").expect("wave format");
        file.write_all(&16_u32.to_le_bytes()).expect("PCM size");
        file.write_all(&1_u16.to_le_bytes()).expect("PCM format");
        file.write_all(&1_u16.to_le_bytes()).expect("channels");
        file.write_all(&sample_rate.to_le_bytes())
            .expect("sample rate");
        file.write_all(&(sample_rate * 2).to_le_bytes())
            .expect("byte rate");
        file.write_all(&2_u16.to_le_bytes()).expect("block align");
        file.write_all(&16_u16.to_le_bytes()).expect("sample bits");
        file.write_all(b"data").expect("data marker");
        file.write_all(&data_size.to_le_bytes()).expect("data size");
        file.write_all(&vec![0_u8; data_size as usize])
            .expect("wave samples");
    }
}
