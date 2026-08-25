use std::io::{self, Cursor, Read};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use playback::ResolvedStream;

use super::{connect_server_certificate_policy, ensure_gstreamer_initialized};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

pub struct TranscodedAudioReader {
    pipeline: gst::Element,
    sink: gst_app::AppSink,
    current: Cursor<Vec<u8>>,
    finished: bool,
}

impl TranscodedAudioReader {
    pub fn mp3(stream: &ResolvedStream) -> Result<Self, String> {
        ensure_gstreamer_initialized()?;
        let encoder = ["lamemp3enc", "avenc_mp3"]
            .into_iter()
            .find(|encoder| gst::ElementFactory::find(encoder).is_some())
            .ok_or_else(|| "GStreamer MP3 encoding support is unavailable".to_string())?;
        let pipeline = gst::parse::launch(&format!(
            "uridecodebin name=decoder ! audioconvert ! audioresample ! {encoder} ! appsink name=sink sync=false"
        ))
        .map_err(|error| error.to_string())?;
        let bin = pipeline
            .downcast_ref::<gst::Bin>()
            .ok_or_else(|| "cast transcode pipeline is not a bin".to_string())?;
        let decoder = bin
            .by_name("decoder")
            .ok_or_else(|| "cast transcode pipeline is missing its decoder".to_string())?;
        let trust_invalid_certificate = stream.trust_invalid_certificate();
        connect_server_certificate_policy(&decoder, move || trust_invalid_certificate);
        decoder.set_property("uri", stream.uri());
        let sink = bin
            .by_name("sink")
            .ok_or_else(|| "cast transcode pipeline is missing its output".to_string())?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| "cast transcode output has the wrong type".to_string())?;
        sink.set_max_buffers(8);
        sink.set_drop(false);
        let bus = pipeline
            .bus()
            .ok_or_else(|| "cast transcode pipeline is missing its bus".to_string())?;

        let startup_state = if stream.window().is_some() {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        pipeline
            .set_state(startup_state)
            .map_err(|error| error.to_string())?;
        if let Some(window) = stream.window() {
            wait_for_preroll(&bus)?;
            let _ = sink.try_pull_preroll(gst::ClockTime::ZERO);
            pipeline
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
                .set_state(gst::State::Playing)
                .map_err(|error| error.to_string())?;
        }
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
            let sample = match self.sink.pull_sample() {
                Ok(sample) => sample,
                Err(_) if self.sink.is_eos() => {
                    self.finished = true;
                    return Ok(0);
                }
                Err(error) => return Err(io::Error::other(error.to_string())),
            };
            let buffer = sample
                .buffer()
                .ok_or_else(|| io::Error::other("cast transcoder produced no buffer"))?;
            let mapped = buffer
                .map_readable()
                .map_err(|_| io::Error::other("cast transcoder output could not be read"))?;
            self.current = Cursor::new(mapped.as_slice().to_vec());
        }
    }
}

impl Drop for TranscodedAudioReader {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn wait_for_preroll(bus: &gst::Bus) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if started.elapsed() >= STARTUP_TIMEOUT {
            return Err("cast transcoder did not finish preparing the stream".to_string());
        }
        let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
            continue;
        };
        match message.view() {
            gst::MessageView::AsyncDone(..) => return Ok(()),
            gst::MessageView::Error(error) => return Err(error.error().to_string()),
            gst::MessageView::Eos(..) => {
                return Err("cast source ended before its requested segment".to_string());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use super::*;

    #[test]
    fn mp3_reader_encodes_a_local_wave_file() {
        let directory = tempfile::tempdir().expect("transcode directory");
        let path = directory.path().join("tone.wav");
        write_silence_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("wave URI");
        let mut reader = TranscodedAudioReader::mp3(&ResolvedStream::new(uri.as_str()))
            .expect("start MP3 transcode");
        let mut encoded = Vec::new();

        reader.read_to_end(&mut encoded).expect("read MP3 stream");

        assert!(encoded.len() > 100);
    }

    #[test]
    fn mp3_reader_encodes_only_the_requested_cue_window() {
        let directory = tempfile::tempdir().expect("transcode directory");
        let path = directory.path().join("cue-source.wav");
        write_silence_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("wave URI");
        let stream = ResolvedStream::new(uri.as_str()).with_window(20, 80);
        let mut reader = TranscodedAudioReader::mp3(&stream).expect("start windowed transcode");
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
