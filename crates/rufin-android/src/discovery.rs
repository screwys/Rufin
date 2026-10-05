use std::io;
use std::sync::{Arc, Mutex};

use jni::{
    Env, JavaVM, jni_sig, jni_str,
    objects::{Global, JByteArray, JClass, JObject, JString, JValue},
};

use crate::host::{AndroidError, error};

pub(crate) fn initialize(env: &mut Env<'_>, context: &JObject<'_>) -> jni::errors::Result<()> {
    let client_class = env.find_class(jni_str!(
        "io/github/screwys/rufin/platform/AndroidDiscoveryClient"
    ))?;
    sources::install_discovery_host(Arc::new(DiscoveryHost {
        context: env.new_global_ref(context)?,
        client_class: env.new_global_ref(client_class)?,
    }));
    Ok(())
}

struct DiscoveryHost {
    context: Global<JObject<'static>>,
    client_class: Global<JClass<'static>>,
}
struct DiscoverySession(Global<JObject<'static>>);

impl sources::DiscoveryHost for DiscoveryHost {
    fn start(&self, timeout_seconds: u64) -> io::Result<Box<dyn sources::DiscoverySession>> {
        let client = JavaVM::singleton()
            .and_then(|vm| {
                vm.attach_current_thread(|env| {
                    let client = env.new_object(
                        &self.client_class,
                        jni_sig!("(Landroid/content/Context;J)V"),
                        &[
                            JValue::Object(&self.context),
                            JValue::Long(timeout_seconds as i64),
                        ],
                    )?;
                    env.new_global_ref(client)
                })
            })
            .map_err(io::Error::other)?;
        Ok(Box::new(DiscoverySession(client)))
    }
}

impl sources::DiscoverySession for DiscoverySession {
    fn request(&mut self, request: &str) -> io::Result<Vec<u8>> {
        JavaVM::singleton()
            .and_then(|vm| {
                vm.attach_current_thread(|env| {
                    let request = JString::from_str(env, request)?;
                    let result = env
                        .call_method(
                            &self.0,
                            jni_str!("request"),
                            jni_sig!("(Ljava/lang/String;)[B"),
                            &[JValue::Object(&request)],
                        )?
                        .l()?;
                    let result = env.cast_local::<JByteArray>(result)?;
                    env.convert_byte_array(&result)
                })
            })
            .map_err(io::Error::other)
    }
}

impl Drop for DiscoverySession {
    fn drop(&mut self) {
        let _ = JavaVM::singleton().and_then(|vm| {
            vm.attach_current_thread(|env| {
                env.call_method(&self.0, jni_str!("close"), jni_sig!(() -> void), &[])?;
                Ok::<(), jni::errors::Error>(())
            })
        });
    }
}

/// Created only by the separate Android discovery service process.
#[derive(uniffi::Object)]
pub struct AndroidDiscoveryWorker(Mutex<sources::DiscoveryWorker>);

#[uniffi::export]
impl AndroidDiscoveryWorker {
    #[uniffi::constructor]
    pub fn new(timeout_seconds: u64) -> Result<Arc<Self>, AndroidError> {
        Ok(Arc::new(Self(Mutex::new(
            sources::DiscoveryWorker::new(timeout_seconds).map_err(error)?,
        ))))
    }

    pub fn request(&self, request: String) -> Result<Vec<u8>, AndroidError> {
        self.0
            .lock()
            .map_err(error)?
            .request(&request)
            .map_err(error)
    }
}
