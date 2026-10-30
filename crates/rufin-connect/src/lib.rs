//! Shared Connect transport, verified enrollment, record sync and media.
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

pub mod media;
mod nearby;
pub mod network;
pub mod pairing;
pub mod store;
pub mod sync;

pub use iroh::SecretKey as Credentials;
pub use iroh::dns;
pub use network::{ConnectNetwork, NetworkConfig, NetworkEvent, Profile};
pub use store::Store;
