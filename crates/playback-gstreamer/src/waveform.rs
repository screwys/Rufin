//! Cancellable waveform decoding for one prepared audio stream.

use super::ensure_gstreamer_initialized;
use gst::prelude::*;
use gstreamer as gst;
use playback::ResolvedStream;
use std::time::{Duration, Instant};
const WAVEFORM_GENERATION_TIMEOUT: Duration = Duration::from_secs(180);
const WAVEFORM_BUS_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(250);

pub fn generate_waveform_peaks_cancellable(
    stream: &ResolvedStream,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<(f64, f64)>, String> {
    ensure_gstreamer_initialized()?;
    if cancelled() {
        return Err("waveform generation cancelled".to_string());
    }

    let pipeline =
        gst::parse::launch("uridecodebin name=decoder ! audioconvert ! audio/x-raw,channels=2 ! level name=level interval=250000000 ! fakesink name=sink")
            .map_err(|error| error.to_string())?;
    let bin = pipeline
        .downcast_ref::<gst::Bin>()
        .ok_or_else(|| "waveform pipeline is not a bin".to_string())?;
    let decoder = bin
        .by_name("decoder")
        .ok_or_else(|| "waveform pipeline is missing decoder".to_string())?;
    let trust_invalid_certificate = stream.trust_invalid_certificate();
    super::connect_server_certificate_policy(&decoder, move || trust_invalid_certificate);
    decoder.set_property("uri", stream.uri());

    let sink = bin
        .by_name("sink")
        .ok_or_else(|| "waveform pipeline is missing sink".to_string())?;
    sink.set_property("qos", false);
    sink.set_property("sync", false);

    let bus = pipeline
        .bus()
        .ok_or_else(|| "waveform pipeline is missing bus".to_string())?;

    let started = Instant::now();
    let result = (|| {
        let startup_state = if stream.window().is_some() {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        pipeline
            .set_state(startup_state)
            .map_err(|error| error.to_string())?;

        if let Some(window) = stream.window() {
            wait_for_preroll(&bus, &cancelled, started)?;
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

        let mut peaks = Vec::new();
        loop {
            check_waveform_deadline(&cancelled, started)?;
            let Some(message) = bus.timed_pop(WAVEFORM_BUS_POLL) else {
                continue;
            };
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(..) => return Ok(peaks),
                MessageView::Error(error) => return Err(error.error().to_string()),
                MessageView::Element(element) => {
                    if let Some(structure) = element.structure()
                        && structure.has_name("level")
                        && let Ok(values) = structure.get::<&gst::glib::ValueArray>("peak")
                        && values.len() >= 2
                        && let (Ok(left), Ok(right)) =
                            (values[0].get::<f64>(), values[1].get::<f64>())
                    {
                        peaks.push((db_to_amplitude(left), db_to_amplitude(right)));
                    }
                }
                _ => {}
            }
        }
    })();

    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn wait_for_preroll(
    bus: &gst::Bus,
    cancelled: &impl Fn() -> bool,
    started: Instant,
) -> Result<(), String> {
    loop {
        check_waveform_deadline(cancelled, started)?;
        let Some(message) = bus.timed_pop(WAVEFORM_BUS_POLL) else {
            continue;
        };
        use gst::MessageView;
        match message.view() {
            MessageView::AsyncDone(..) => return Ok(()),
            MessageView::Error(error) => return Err(error.error().to_string()),
            MessageView::Eos(..) => {
                return Err("waveform source ended before its requested window".to_string());
            }
            _ => {}
        }
    }
}

fn check_waveform_deadline(cancelled: &impl Fn() -> bool, started: Instant) -> Result<(), String> {
    if cancelled() {
        return Err("waveform generation cancelled".to_string());
    }
    if started.elapsed() > WAVEFORM_GENERATION_TIMEOUT {
        return Err("waveform generation timed out".to_string());
    }
    Ok(())
}

fn db_to_amplitude(value: f64) -> f64 {
    10.0_f64.powf(value / 20.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn waveform_decode_is_bounded_to_the_prepared_stream_window() {
        let directory = tempfile::tempdir().expect("waveform fixture directory");
        let path = directory.path().join("window.wav");
        write_stereo_wave(&path);
        let uri = gst::glib::filename_to_uri(&path, None).expect("waveform fixture URI");
        let stream = ResolvedStream::new(uri.as_str()).with_window(1_000, 1_500);

        let peaks = generate_waveform_peaks_cancellable(&stream, || false)
            .expect("decode waveform source window");

        assert!(!peaks.is_empty());
        assert!(peaks.len() <= 3, "unexpected full-file waveform: {peaks:?}");
        assert!(
            peaks
                .iter()
                .take(2)
                .all(|(left, right)| *left > 0.5 && *right > 0.5),
            "unexpected pre-window waveform: {peaks:?}"
        );
    }

    fn write_stereo_wave(path: &std::path::Path) {
        const SAMPLE_RATE: u32 = 8_000;
        const CHANNELS: u16 = 2;
        const BITS_PER_SAMPLE: u16 = 16;
        let frames = SAMPLE_RATE * 2;
        let bytes_per_frame = u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
        let data_len = frames * bytes_per_frame;
        let mut file = File::create(path).expect("create waveform fixture");
        file.write_all(b"RIFF").expect("write RIFF");
        file.write_all(&(36 + data_len).to_le_bytes())
            .expect("write RIFF size");
        file.write_all(b"WAVEfmt ").expect("write WAVE format");
        file.write_all(&16_u32.to_le_bytes())
            .expect("write PCM format size");
        file.write_all(&1_u16.to_le_bytes())
            .expect("write PCM format");
        file.write_all(&CHANNELS.to_le_bytes())
            .expect("write channels");
        file.write_all(&SAMPLE_RATE.to_le_bytes())
            .expect("write sample rate");
        file.write_all(&(SAMPLE_RATE * bytes_per_frame).to_le_bytes())
            .expect("write byte rate");
        file.write_all(&(bytes_per_frame as u16).to_le_bytes())
            .expect("write block alignment");
        file.write_all(&BITS_PER_SAMPLE.to_le_bytes())
            .expect("write sample depth");
        file.write_all(b"data").expect("write data tag");
        file.write_all(&data_len.to_le_bytes())
            .expect("write data length");

        for frame in 0..frames {
            let sample = if frame < SAMPLE_RATE {
                0_i16
            } else if frame % 2 == 0 {
                26_000_i16
            } else {
                -26_000_i16
            };
            file.write_all(&sample.to_le_bytes())
                .expect("write left sample");
            file.write_all(&sample.to_le_bytes())
                .expect("write right sample");
        }
    }
}
