use std::io::{self, Cursor, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::ResolvedStream;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use library::{AudioCodec, AudioEncoding};

use super::{configure_sources, ensure_gstreamer_initialized};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

struct TranscodePipeline {
    element: gst::Element,
    bus: gst::Bus,
    _stream: ResolvedStream,
}

impl TranscodePipeline {
    fn new(
        stream: &ResolvedStream,
        encoding: AudioEncoding,
        output: Option<&Path>,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        ensure_gstreamer_initialized()?;
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
            "uridecodebin name=decoder ! audioconvert ! audioresample ! {raw}{encoder} ! {sink} name=sink sync=false"
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
        let pipeline = Self {
            element,
            bus,
            _stream: stream.clone(),
        };
        pipeline
            .element
            .set_state(if stream.window().is_some() {
                gst::State::Paused
            } else {
                gst::State::Playing
            })
            .map_err(|error| error.to_string())?;
        if let Some(window) = stream.window() {
            let started = Instant::now();
            loop {
                if cancelled() {
                    return Err("Transcoding cancelled".into());
                }
                if started.elapsed() >= STARTUP_TIMEOUT {
                    return Err("Audio encoder did not prepare the source".into());
                }
                let Some(message) = pipeline.bus.timed_pop(POLL) else {
                    continue;
                };
                match message.view() {
                    gst::MessageView::AsyncDone(..) => break,
                    gst::MessageView::Error(error) => return Err(error.error().to_string()),
                    gst::MessageView::Eos(..) => {
                        return Err("Audio source ended before its requested segment".into());
                    }
                    _ => {}
                }
            }
            pipeline
                .element
                .seek(
                    1.0,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(window.start_millis),
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(window.end_millis),
                )
                .map_err(|error| error.to_string())?;
            pipeline
                .element
                .set_state(gst::State::Playing)
                .map_err(|error| error.to_string())?;
        }
        Ok(pipeline)
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
    encoding: AudioEncoding,
    output: &Path,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    let pipeline = TranscodePipeline::new(stream, encoding, Some(output), &cancelled)
        .map_err(io::Error::other)?;
    loop {
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Transcoding cancelled",
            ));
        }
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
}

impl TranscodedAudioReader {
    pub fn new(stream: &ResolvedStream, encoding: AudioEncoding) -> Result<Self, String> {
        let pipeline = TranscodePipeline::new(stream, encoding, None, &|| false)?;
        let sink = pipeline
            .element
            .downcast_ref::<gst::Bin>()
            .and_then(|bin| bin.by_name("sink"))
            .ok_or("Audio encoder has no output")?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "Audio encoder output has the wrong type")?;
        Ok(Self {
            pipeline,
            sink,
            current: Cursor::new(Vec::new()),
            finished: false,
        })
    }
}

impl Read for TranscodedAudioReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            let read = self.current.read(output)?;
            if read > 0 || self.finished || output.is_empty() {
                return Ok(read);
            }
            if let Some(error) = self.pipeline.bus.pop_filtered(&[gst::MessageType::Error])
                && let gst::MessageView::Error(error) = error.view()
            {
                return Err(io::Error::other(error.error().to_string()));
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
        let mut reader =
            TranscodedAudioReader::new(&ResolvedStream::new(uri.as_str()), AudioEncoding::mp3())
                .expect("start MP3 transcode");
        let mut encoded = Vec::new();

        reader.read_to_end(&mut encoded).expect("read MP3 stream");

        assert!(encoded.len() > 100);

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
                    },
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
    fn mp3_reader_encodes_only_the_requested_cue_window() {
        let directory = tempfile::tempdir().expect("transcode directory");
        let path = directory.path().join("cue-source.wav");
        write_silence_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("wave URI");
        let stream = ResolvedStream::new(uri.as_str()).with_window(20, 80);
        let mut reader = TranscodedAudioReader::new(&stream, AudioEncoding::mp3())
            .expect("start windowed transcode");
        let mut encoded = Vec::new();

        reader.read_to_end(&mut encoded).expect("read MP3 stream");

        assert!(encoded.len() > 50);
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
