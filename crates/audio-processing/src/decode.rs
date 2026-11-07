use crate::{ResolvedStream, configure_sources, ensure_gstreamer_initialized};
use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app::AppSink;
use std::fmt;

pub(crate) const SAMPLE_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

/// Why decoding a stream failed.
#[derive(Debug)]
pub enum DecodeError {
    /// The audio itself cannot be decoded, so every read of the same audio fails this way.
    Content(String),
    /// Anything else: the source, network or pipeline failed, the stream ended before its
    /// window, or the job was cancelled or stopped making progress.
    Stopped(String),
}

impl DecodeError {
    fn from_bus(error: &gst::message::Error) -> Self {
        let cause = error.error();
        let element = error
            .src()
            .and_then(|source| source.downcast_ref::<gst::Element>());
        match content_failure(&cause, element) {
            Some(_) => Self::Content(cause.to_string()),
            None => Self::Stopped(cause.to_string()),
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Content(message) | Self::Stopped(message)) = self;
        formatter.write_str(message)
    }
}

impl From<DecodeError> for String {
    fn from(error: DecodeError) -> Self {
        error.to_string()
    }
}

/// A GStreamer error that the audio itself causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentFailure {
    /// No installed element can read this audio's container or codec.
    Unsupported,
    /// The audio's data is broken.
    Corrupt,
}

/// What `error`, posted by `element`, says about the audio itself: a stream error or a missing
/// plugin. None for everything else, and for anything a network source posts: it stops its
/// stream when the connection fails.
pub fn content_failure(
    error: &glib::Error,
    element: Option<&gst::Element>,
) -> Option<ContentFailure> {
    let network = element
        .and_then(|element| element.factory())
        .is_some_and(|factory| factory.klass().split('/').any(|part| part == "Network"));
    if network {
        return None;
    }
    if let Some(error) = error.kind::<gst::StreamError>() {
        return Some(match error {
            gst::StreamError::CodecNotFound
            | gst::StreamError::TypeNotFound
            | gst::StreamError::WrongType
            | gst::StreamError::Format => ContentFailure::Unsupported,
            _ => ContentFailure::Corrupt,
        });
    }
    (error.kind::<gst::CoreError>() == Some(gst::CoreError::MissingPlugin))
        .then_some(ContentFailure::Unsupported)
}

pub(crate) struct DecodePipeline {
    pub pipeline: gst::Element,
    pub bus: gst::Bus,
    pub sink: gst::Element,
    window: Option<crate::StreamWindow>,
}

impl Drop for DecodePipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl DecodePipeline {
    pub fn new(stream: &ResolvedStream, output: &str) -> Result<Self, DecodeError> {
        ensure_gstreamer_initialized().map_err(DecodeError::Stopped)?;
        let pipeline = gst::parse::launch(&format!(
            "uridecodebin name=decoder ! audioconvert ! {output}"
        ))
        .map_err(|error| DecodeError::Stopped(error.to_string()))?;
        let bin = pipeline
            .downcast_ref::<gst::Bin>()
            .expect("analysis pipeline");
        let decoder = bin.by_name("decoder").expect("analysis decoder");
        let trust = stream.trust_invalid_certificate();
        configure_sources(&decoder, move || trust);
        decoder.set_property("uri", stream.uri());
        let sink = bin.by_name("sink").expect("analysis sink");
        let bus = pipeline.bus().expect("analysis bus");
        Ok(Self {
            pipeline,
            bus,
            sink,
            window: stream.window().cloned(),
        })
    }

    pub fn start(&self, check: impl Fn() -> Result<(), String>) -> Result<(), DecodeError> {
        let check = || check().map_err(DecodeError::Stopped);
        check()?;
        self.pipeline
            .set_state(if self.window.is_some() {
                gst::State::Paused
            } else {
                gst::State::Playing
            })
            .map_err(|error| DecodeError::Stopped(error.to_string()))?;
        if let Some(window) = &self.window {
            loop {
                check()?;
                let Some(message) = self.bus.timed_pop(SAMPLE_POLL) else {
                    continue;
                };
                match message.view() {
                    gst::MessageView::AsyncDone(..) => break,
                    gst::MessageView::Error(error) => return Err(DecodeError::from_bus(error)),
                    gst::MessageView::Eos(..) => {
                        return Err(DecodeError::Stopped(
                            "audio source ended before its requested window".into(),
                        ));
                    }
                    _ => {}
                }
            }
            if let Some(sink) = self.sink.downcast_ref::<AppSink>() {
                let _ = sink.try_pull_preroll(gst::ClockTime::ZERO);
            }
            self.pipeline
                .seek(
                    1.0,
                    gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(window.start_millis),
                    gst::SeekType::Set,
                    gst::ClockTime::from_mseconds(window.end_millis),
                )
                .map_err(|error| DecodeError::Stopped(error.to_string()))?;
            self.pipeline
                .set_state(gst::State::Playing)
                .map_err(|error| DecodeError::Stopped(error.to_string()))?;
        }
        Ok(())
    }

    pub fn sample(
        &self,
        sink: &AppSink,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Option<gst::Sample>, DecodeError> {
        loop {
            check().map_err(DecodeError::Stopped)?;
            if let Some(sample) = sink.try_pull_sample(SAMPLE_POLL) {
                return Ok(Some(sample));
            }
            while let Some(message) = self.bus.pop() {
                if let gst::MessageView::Error(error) = message.view() {
                    return Err(DecodeError::from_bus(error));
                }
            }
            if sink.is_eos() {
                return Ok(None);
            }
        }
    }

    pub fn duration_millis(&self) -> Option<u64> {
        self.window
            .as_ref()
            .map(|window| window.end_millis - window.start_millis)
            .or_else(|| {
                self.pipeline
                    .query_duration::<gst::ClockTime>()
                    .map(|value| value.mseconds())
            })
    }
}
