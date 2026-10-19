//! Shared Connect transport, verified enrollment, media and profile state.
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

pub mod media;
mod nearby;
pub mod network;
pub mod pairing;

pub use iroh::SecretKey as Credentials;
pub use iroh::dns;
pub use network::{ConnectNetwork, NetworkConfig, NetworkEvent};
