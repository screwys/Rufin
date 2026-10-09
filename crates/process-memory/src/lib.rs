#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod file_pages;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub use glibc::{configure_allocator, request_reclaim};

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn configure_allocator() -> bool {
    true
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn request_reclaim() {}
