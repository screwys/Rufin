#![doc = include_str!("../docs/index.md")]
#![forbid(unsafe_code)]

#[cfg(not(any(
    feature = "async",
    feature = "single_threaded",
    feature = "multi_threaded"
)))]
compile_error!(
    "You must enable exactly one of the following features: async, single_threaded, multi_threaded"
);
#[cfg(any(
    all(feature = "async", feature = "single_threaded"),
    all(feature = "async", feature = "multi_threaded"),
    all(feature = "single_threaded", feature = "multi_threaded")
))]
compile_error!(
    "You must enable exactly one of the following features: async, single_threaded, multi_threaded"
);

pub mod client;
pub mod compression;
pub mod connection;
pub mod crypto;
pub mod dialects;
pub mod docs;
pub mod error;
pub mod msg_handler;
pub mod resource;
pub mod session;
pub mod tree;

pub use client::{Client, ClientConfig, UncPath};
pub use connection::{Connection, ConnectionConfig};
pub use error::Error;
pub use resource::{
    Directory, File, FileCreateArgs, GetLen, Pipe, PipeRpcConnection, ReadAt, ReadAtChannel,
    Resource, ResourceHandle, WriteAt, WriteAtChannel,
};
pub use session::Session;
pub use tree::{DfsRootTreeRef, Tree};

pub use smb_dtyp::*;
pub use smb_fscc::*;
pub use smb_msg::*;
pub use smb_transport as transport;

/// SMB Result type
pub type Result<T> = std::result::Result<T, crate::Error>;

// Re-exports of some dependencies for convenience
pub mod sync_helpers;
