//! Temporary probe for the app ID change, run by `.github/workflows/rename-probe.yml`; both go
//! once that workflow has reported.
//!
//! `write SERVICE USER SECRET` and `read SERVICE USER SECRET` use the platform store as Rufin
//! does. `read` exits 0 when it finds SECRET, 2 when there is no entry, 3 when macOS would have
//! to ask the user, 4 for another value and 1 for any other failure. `paths` prints the data
//! folders of the current name.

macro_rules! say {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout().lock(), $($argument)*);
    }};
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    say!("build {}", option_env!("RUFIN_PROBE_BUILD").unwrap_or("1"));
    std::process::exit(run(&arguments));
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn run(arguments: &[String]) -> i32 {
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match arguments.as_slice() {
        ["paths"] => {
            let directories = directories::ProjectDirs::from("io.github", "screwys", "Rufin")
                .expect("project folders");
            say!("config {}", directories.config_dir().display());
            say!("data {}", directories.data_dir().display());
            say!("cache {}", directories.cache_dir().display());
            0
        }
        ["write", service, user, secret] => {
            match entry(service, user).and_then(|entry| entry.set_password(secret)) {
                Ok(()) => 0,
                Err(error) => {
                    say!("write failed: {error:?}");
                    1
                }
            }
        }
        ["read", service, user, expected] => {
            #[cfg(target_os = "macos")]
            let _no_prompts =
                security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
                    .expect("disable keychain prompts");
            match entry(service, user).and_then(|entry| entry.get_password()) {
                Ok(secret) if secret == *expected => 0,
                Ok(_) => 4,
                Err(keyring_core::Error::NoEntry) => 2,
                Err(error) => {
                    let text = format!("{error:?}");
                    say!("read failed: {text}");
                    if text.contains("-25308") { 3 } else { 1 }
                }
            }
        }
        ["delete", service, user] => {
            match entry(service, user).and_then(|entry| entry.delete_credential()) {
                Ok(()) | Err(keyring_core::Error::NoEntry) => 0,
                Err(error) => {
                    say!("delete failed: {error:?}");
                    1
                }
            }
        }
        _ => {
            say!(
                "usage: paths | write SERVICE USER SECRET | read SERVICE USER SECRET | delete SERVICE USER"
            );
            1
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn entry(service: &str, user: &str) -> keyring_core::Result<keyring_core::Entry> {
    #[cfg(target_os = "windows")]
    let store: std::sync::Arc<keyring_core::CredentialStore> =
        windows_native_keyring_store::Store::new()?;
    #[cfg(target_os = "macos")]
    let store: std::sync::Arc<keyring_core::CredentialStore> =
        apple_native_keyring_store::keychain::Store::new()?;
    store.build(service, user, None)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn run(_arguments: &[String]) -> i32 {
    say!("this probe runs on Windows and macOS");
    1
}
