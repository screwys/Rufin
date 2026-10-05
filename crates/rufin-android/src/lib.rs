#[cfg(target_os = "android")]
mod backups;
#[cfg(target_os = "android")]
mod browse;
#[cfg(target_os = "android")]
mod connect;
#[cfg(target_os = "android")]
mod controller;
#[cfg(target_os = "android")]
mod discovery;
#[cfg(target_os = "android")]
mod documents;
#[cfg(target_os = "android")]
mod downloads;
#[cfg(target_os = "android")]
mod host;
#[cfg(target_os = "android")]
mod library;
#[cfg(target_os = "android")]
mod library_events;
#[cfg(target_os = "android")]
mod media_session;
#[cfg(target_os = "android")]
mod metadata;
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
#[cfg(target_os = "android")]
mod source_setup;
#[cfg(target_os = "android")]
mod source_state;

uniffi::setup_scaffolding!();
#[cfg(target_os = "android")]
mod audio;
