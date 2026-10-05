use std::sync::{Arc, Mutex, OnceLock};

use glib::{subclass::prelude::*, translate::*};
use gstreamer::{self as gst, prelude::*, subclass::prelude::*};
use gstreamer_audio::{self as audio, subclass::prelude::*};
use jni::{
    Env, JavaVM, jni_sig, jni_str,
    objects::{Global, JClass, JObject, JValue},
};

static TRACK_CLASS: OnceLock<Global<JClass<'static>>> = OnceLock::new();

pub(crate) fn register(env: &mut Env<'_>) -> jni::errors::Result<()> {
    let class = env.find_class(jni_str!(
        "io/github/screwys/rufin/platform/AndroidAudioTrack"
    ))?;
    let _ = TRACK_CLASS.set(env.new_global_ref(class)?);
    gst::Element::register(
        None,
        "rufinandroidaudiosink",
        gst::Rank::PRIMARY + 1,
        AudioTrackSink::static_type(),
    )
    .or_else(|error| env.throw(error.to_string()))
}

glib::wrapper! {
    pub struct AudioTrackSink(ObjectSubclass<imp::AudioTrackSink>)
        @extends audio::AudioSink, audio::AudioBaseSink, audio::gst_base::BaseSink, gst::Element, gst::Object;
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AudioTrackSink {
        pub device: Mutex<Option<Arc<Global<JObject<'static>>>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AudioTrackSink {
        const NAME: &'static str = "RufinAndroidAudioSink";
        type Type = super::AudioTrackSink;
        type ParentType = audio::AudioSink;

        #[allow(unsafe_code)]
        fn class_init(class: &mut Self::Class) {
            // These existing AudioSink hooks are missing from the Rust trait.
            let class = (class as *mut Self::Class).cast::<audio::ffi::GstAudioSinkClass>();
            unsafe {
                let extension = glib::gobject_ffi::g_type_class_get_private(
                    class.cast(),
                    audio::AudioSink::static_type().into_glib(),
                )
                .cast::<audio::ffi::GstAudioSinkClassExtension>();
                (*class).pause = Some(super::pause);
                (*class).resume = Some(super::resume);
                (*class).stop = Some(super::stop);
                (*extension).clear_all = Some(super::clear);
                (*class).extension = extension;
            }
        }
    }

    impl ObjectImpl for AudioTrackSink {}
    impl GstObjectImpl for AudioTrackSink {}
    impl ElementImpl for AudioTrackSink {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static DATA: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(DATA.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "Android audio output",
                    "Sink/Audio",
                    "Plays audio through Android",
                    "Rufin contributors",
                )
            }))
        }
        fn pad_templates() -> &'static [gst::PadTemplate] {
            static PADS: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            PADS.get_or_init(|| vec![gst::PadTemplate::new("sink", gst::PadDirection::Sink,
                gst::PadPresence::Always, &"audio/x-raw,format=(string){S16LE,F32LE},layout=(string)interleaved,channels=(int)[1,2],rate=(int){8000,11025,12000,16000,22050,24000,32000,44100,48000}".parse::<gst::Caps>().unwrap()).unwrap()])
        }
    }
    impl BaseSinkImpl for AudioTrackSink {}
    impl AudioBaseSinkImpl for AudioTrackSink {}
    impl AudioSinkImpl for AudioTrackSink {
        fn prepare(&self, spec: &mut audio::AudioRingBufferSpec) -> Result<(), gst::LoggableError> {
            let info = spec.audio_info();
            let track = JavaVM::singleton()
                .and_then(|vm| {
                    vm.attach_current_thread(|env| {
                        let track = env.new_object(
                            TRACK_CLASS.get().unwrap(),
                            jni_sig!("(IIII)V"),
                            &[
                                JValue::Int(info.rate() as i32),
                                JValue::Int(info.channels() as i32),
                                JValue::Int(if info.format() == audio::AudioFormat::F32le {
                                    4
                                } else {
                                    2
                                }),
                                JValue::Int(spec.segsize() * spec.segtotal()),
                            ],
                        )?;
                        env.call_method(&track, jni_str!("resume"), jni_sig!("()V"), &[])?;
                        env.new_global_ref(track)
                    })
                })
                .map_err(|error| gst::loggable_error!(gst::CAT_RUST, "{error}"))?;
            *self.device.lock().unwrap() = Some(Arc::new(track));
            Ok(())
        }
        fn unprepare(&self) -> Result<(), gst::LoggableError> {
            let device = self.device.lock().unwrap().take();
            if let Some(device) = device {
                JavaVM::singleton()
                    .and_then(|vm| {
                        vm.attach_current_thread(|env| {
                            env.call_method(&*device, jni_str!("close"), jni_sig!("()V"), &[])?;
                            Ok::<_, jni::errors::Error>(())
                        })
                    })
                    .map_err(|error| gst::loggable_error!(gst::CAT_RUST, "{error}"))?;
            }
            Ok(())
        }
        #[allow(unsafe_code)]
        fn write(&self, data: &[u8]) -> Result<i32, gst::LoggableError> {
            let result = self.with_device(|env, track| {
                // The helper copies PCM, including an interrupted write's tail.
                // It does not retain this borrowed buffer.
                let buffer =
                    unsafe { env.new_direct_byte_buffer(data.as_ptr().cast_mut(), data.len()) }?;
                env.call_method(
                    track,
                    jni_str!("write"),
                    jni_sig!("(Ljava/nio/ByteBuffer;I)I"),
                    &[JValue::Object(&buffer), JValue::Int(data.len() as i32)],
                )?
                .i()
            });
            match result {
                Ok(Some(count)) if count >= 0 => {
                    if count as usize != data.len() {
                        tracing::debug!(
                            requested_bytes = data.len(),
                            written_bytes = count,
                            "Android audio short write"
                        );
                    }
                    Ok(count)
                }
                result => {
                    let error = format!("Android audio write failed: {result:?}");
                    self.failed(&error);
                    Err(gst::loggable_error!(gst::CAT_RUST, "{error}"))
                }
            }
        }
        fn delay(&self) -> u32 {
            match self.with_device(|env, track| {
                env.call_method(track, jni_str!("delay"), jni_sig!("()I"), &[])?
                    .i()
            }) {
                Ok(value) => value.unwrap_or(0) as u32,
                Err(error) => {
                    self.failed(&error);
                    0
                }
            }
        }
        fn reset(&self) {
            self.command(jni_str!("clear"));
        }
    }

    impl AudioTrackSink {
        fn with_device<T>(
            &self,
            action: impl FnOnce(&mut Env<'_>, &Global<JObject<'static>>) -> jni::errors::Result<T>,
        ) -> jni::errors::Result<Option<T>> {
            let device = self.device.lock().unwrap().clone();
            device
                .map(|device| {
                    JavaVM::singleton()?.attach_current_thread(|env| action(env, &device))
                })
                .transpose()
        }
        pub fn command(&self, method: &'static jni::strings::JNIStr) {
            if let Err(error) = self.with_device(|env, track| {
                env.call_method(track, method, jni_sig!("()V"), &[])
                    .map(|_| ())
            }) {
                self.failed(&error);
            }
        }
        fn failed(&self, error: &impl std::fmt::Display) {
            gst::element_error!(
                self.obj(),
                gst::ResourceError::Write,
                ("Could not play this track"),
                ["{error}"]
            );
        }
    }
}

macro_rules! device_hook {
    ($name:ident, $method:literal) => {
        #[allow(unsafe_code)]
        unsafe extern "C" fn $name(pointer: *mut audio::ffi::GstAudioSink) {
            let instance =
                unsafe { &*pointer.cast::<<imp::AudioTrackSink as ObjectSubclass>::Instance>() };
            let sink = instance.imp();
            gst::panic_to_error!(sink, (), {
                sink.command(jni_str!($method));
            });
        }
    };
}
device_hook!(pause, "pause");
device_hook!(resume, "resume");
device_hook!(stop, "stop");
device_hook!(clear, "clear");
