#[cfg(target_os = "android")]
mod browse;
#[cfg(target_os = "android")]
mod discovery;
#[cfg(target_os = "android")]
mod documents;
#[cfg(target_os = "android")]
mod host;
#[cfg(target_os = "android")]
mod library;
#[cfg(target_os = "android")]
mod library_events;
#[cfg(target_os = "android")]
mod media_session;
#[cfg(target_os = "android")]
mod more;
#[cfg(target_os = "android")]
mod network;
#[cfg(target_os = "android")]
mod pins;
#[cfg(target_os = "android")]
mod player;
#[cfg(target_os = "android")]
mod preferences;

uniffi::setup_scaffolding!();
#[cfg(target_os = "android")]
mod audio;
