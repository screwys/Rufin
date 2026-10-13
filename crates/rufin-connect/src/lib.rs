//! Shared Connect transport, verified enrollment, media and profile state.
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

pub mod media;
pub mod network;
pub mod pairing;
pub mod profile;

pub use iroh::SecretKey as Credentials;
pub use iroh::dns;
pub use network::{ConnectNetwork, NetworkConfig, NetworkEvent};
