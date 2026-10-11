#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub use glibc::{configure_allocator, request_reclaim};

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn configure_allocator() {}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn request_reclaim() {}
