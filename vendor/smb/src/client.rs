//! High-level SMB client interface.

mod config;
mod smb_client;
mod unc_path;

pub use config::ClientConfig;
pub use smb_client::Client;
pub use unc_path::UncPath;
