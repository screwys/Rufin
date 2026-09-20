use std::sync::Arc;

use crate::{connection::preauth_hash::PreauthHashState, dialects::DialectImpl};
use binrw::prelude::*;
use smb_dtyp::Guid;
use smb_msg::*;

use super::ConnectionConfig;

/// Contains important information from the negotiation process,
/// to be used during connection operations.
#[derive(Debug)]
pub struct NegotiatedProperties {
    /// From the server's negotiation response.
    pub server_guid: Guid,

    /// From the server's negotiation response.
    pub caps: GlobalCapabilities,

    /// From the server's negotiation response.
    pub max_transact_size: u32,
    /// From the server's negotiation response.
    pub max_read_size: u32,
    /// From the server's negotiation response.
    pub max_write_size: u32,

    /// From the server's negotiation response.
    pub auth_buffer: Vec<u8>,

    /// Signing algorithm used for the connection, and specified by the server
    /// using negotiation context. This is irrelevant for dialects below 3.1.1,
    /// and if not specified, this property is not set, but the connection may still be
    /// signed using the default algorithm, as specified in the spec.
    pub signing_algo: Option<SigningAlgorithmId>,
    /// Encryption cipher used for the connection, and specified by the server
    /// using negotiation context. This is irrelevant for dialects below 3.1.1,
    /// and if not specified, this property is not set, but the connection may still be
    /// encrypted using the default cipher, as specified in the spec.
    pub encryption_cipher: Option<EncryptionCipher>,
    /// Compression capabilities used for the connection, and specified by the server
    /// using negotiation context.
    pub compression: Option<CompressionCapabilities>,

    /// The selected dialect revision for the connection.
    /// Use [ConnectionInfo::dialect] to get the implementation of the selected dialect.
    pub dialect_rev: Dialect,
}

/// This struct is initalized once a connection is established and negotiated.
/// It contains all the information about the connection.
#[derive(Debug)]
pub struct ConnectionInfo {
    /// The server name used for the connection.
    pub server_name: String,
    /// The server address used for the connection.
    pub server_address: std::net::SocketAddr,

    /// Contains negotiated properties of the connection.
    pub negotiation: NegotiatedProperties,
    /// Contains the implementation of the selected dialect.
    pub dialect: Arc<DialectImpl>,
    /// Contains the configuration of the connection, as specified by the user when the connection was established.
    pub config: ConnectionConfig,
    /// Preauthentication hash state, if applicable.
    pub preauth_hash: PreauthHashState,
    /// The client GUID used for the connection.
    pub client_guid: Guid,
}
