use std::sync::Arc;

use jni::{
    Env, JavaVM, jni_sig, jni_str,
    objects::{Global, JByteArray, JObject, JString, JValue},
};
use sources::{SourceError, SourceResult};

pub(crate) fn initialize(env: &mut Env<'_>, documents: &JObject<'_>) -> jni::errors::Result<()> {
    sources::install_document_host(Arc::new(DocumentHost(env.new_global_ref(documents)?)));
    Ok(())
}

struct DocumentHost(Global<JObject<'static>>);

impl sources::DocumentHost for DocumentHost {
    fn request(&self, request: &str) -> SourceResult<String> {
        JavaVM::singleton()
            .and_then(|vm| {
                vm.attach_current_thread(|env| {
                    let request = JString::from_str(env, request)?;
                    let result = env
                        .call_method(
                            &self.0,
                            jni_str!("request"),
                            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                            &[JValue::Object(&request)],
                        )?
                        .l()?;
                    env.cast_local::<JString>(result)?.try_to_string(env)
                })
            })
            .map_err(|error| SourceError::Other(error.to_string()))
    }

    fn read(&self, handle: u64, offset: u64, length: u32) -> SourceResult<Vec<u8>> {
        JavaVM::singleton()
            .and_then(|vm| {
                vm.attach_current_thread(|env| {
                    let result = env
                        .call_method(
                            &self.0,
                            jni_str!("read"),
                            jni_sig!("(JJI)[B"),
                            &[
                                JValue::Long(handle as i64),
                                JValue::Long(offset as i64),
                                JValue::Int(length as i32),
                            ],
                        )?
                        .l()?;
                    let result = env.cast_local::<JByteArray>(result)?;
                    env.convert_byte_array(&result)
                })
            })
            .map_err(|error| SourceError::Other(error.to_string()))
    }

    fn close(&self, handle: u64) {
        let _ = JavaVM::singleton().and_then(|vm| {
            vm.attach_current_thread(|env| {
                env.call_method(
                    &self.0,
                    jni_str!("close"),
                    jni_sig!("(J)V"),
                    &[JValue::Long(handle as i64)],
                )?;
                Ok::<(), jni::errors::Error>(())
            })
        });
    }
}
