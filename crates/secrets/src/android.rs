use std::sync::OnceLock;

use jni::{
    Env, JavaVM, jni_sig, jni_str,
    objects::{Global, JObject, JString, JValue},
};

use crate::{SecretError, SecretKey, SecretResult, SecretStore};

static KEYRING: OnceLock<Global<JObject<'static>>> = OnceLock::new();

/// Initialize on a Java call before constructing the shared secret store.
pub fn initialize(env: &mut Env<'_>, context: &JObject<'_>) -> jni::errors::Result<()> {
    if KEYRING.get().is_none() {
        let keyring = env.new_object(
            jni_str!("io/github/screwys/rufin/platform/AndroidKeyring"),
            jni_sig!("(Landroid/content/Context;)V"),
            &[JValue::Object(context)],
        )?;
        let _ = KEYRING.set(env.new_global_ref(keyring)?);
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(super) struct SystemKeyringBackend {
    scope_id: String,
    keyring: &'static Global<JObject<'static>>,
}

impl SystemKeyringBackend {
    pub(super) fn new(scope_id: String) -> SecretResult<Self> {
        let keyring = KEYRING.get().ok_or_else(|| {
            SecretError::Backend("Android keyring has not been initialized".into())
        })?;
        Ok(Self { scope_id, keyring })
    }

    fn run<T>(
        &self,
        operation: impl FnOnce(&mut Env<'_>) -> jni::errors::Result<T>,
    ) -> SecretResult<T> {
        JavaVM::singleton()
            .and_then(|vm| vm.attach_current_thread(operation))
            .map_err(|error| SecretError::Backend(error.to_string()))
    }
}

impl SecretStore for SystemKeyringBackend {
    fn save_secret(&self, key: &SecretKey, secret: &str) -> SecretResult<()> {
        self.run(|env| {
            let key = JString::from_str(env, key.scoped_config_key(&self.scope_id))?;
            let secret = JString::from_str(env, secret)?;
            env.call_method(
                self.keyring,
                jni_str!("save"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
                &[JValue::Object(&key), JValue::Object(&secret)],
            )?;
            Ok(())
        })
    }

    fn load_secret(&self, key: &SecretKey) -> SecretResult<Option<String>> {
        self.run(|env| {
            let key = JString::from_str(env, key.scoped_config_key(&self.scope_id))?;
            let secret = env
                .call_method(
                    self.keyring,
                    jni_str!("load"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                    &[JValue::Object(&key)],
                )?
                .l()?;
            if secret.is_null() {
                Ok(None)
            } else {
                env.cast_local::<JString>(secret)?
                    .try_to_string(env)
                    .map(Some)
            }
        })
    }

    fn delete_secret(&self, key: &SecretKey) -> SecretResult<()> {
        self.run(|env| {
            let key = JString::from_str(env, key.scoped_config_key(&self.scope_id))?;
            env.call_method(
                self.keyring,
                jni_str!("delete"),
                jni_sig!("(Ljava/lang/String;)V"),
                &[JValue::Object(&key)],
            )?;
            Ok(())
        })
    }
}
