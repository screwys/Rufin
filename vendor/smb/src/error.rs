use std::{num::TryFromIntError, sync::PoisonError};

use smb_transport::TransportError;
use thiserror::Error;

use crate::{UncPath, connection::TransformError, sync_helpers::AcquireError};
use smb_msg::{Command, ErrorResponse, Status};

#[derive(Debug)]
pub enum TimedOutTask {
    ReceiveNextMessage,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Unexpected Message, {0}")]
    InvalidMessage(String),
    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Binrw Error: {0}")]
    BinRWError(#[from] binrw::Error),
    #[error("Int parsing Error: {0}")]
    ParsingError(#[from] TryFromIntError),

    /// Indicates connection stopped - due to error or closed by user.
    /// See [`TransportError::NotConnected`] for transport-level disconnection.
    /// Usually, this is the actual error returned when trying to use a stopped connection, anyway.
    #[error("Client connection is stopped")]
    ConnectionStopped,

    #[error("Operation cancelled: {0}")]
    Cancelled(&'static str),

    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("Unable to transform message: {0}")]
    TranformFailed(TransformError),
    #[error("Crypto error: {0}")]
    CryptoError(#[from] crate::crypto::CryptoError),

    /// Indicates that the negotiation phase of the SMB protocol failed.
    ///
    /// This might be due to incompatible protocol versions, unsupported features,
    /// or configuration issues between the client and server.
    #[error("Negotiation error: {0}")]
    NegotiationError(String),

    #[error("Signature verification failed!")]
    SignatureVerificationFailed,
    #[error("Unexpected message status: {}.", Status::try_display_as_status(*.0))]
    UnexpectedMessageStatus(u32),
    // TODO: This vs UnexpectedMessageStatus?!
    #[error("Server returned an error message with status: {}.", Status::try_display_as_status(*.0))]
    ReceivedErrorMessage(u32, ErrorResponse),
    #[error("Unexpected command: {0}")]
    UnexpectedMessageCommand(Command),
    #[error("Missing permissions to perform {0}")]
    MissingPermissions(String),

    /// Indicates an error sourced from the underlying authentication SSPI
    /// (Security Support Provider Interface) library.
    ///
    /// SSPI is used for handling authentication mechanisms like NTLM and Kerberos.
    ///
    /// For more references, see the [`sspi` crate documentation][sspi]
    #[error("Sspi error: {0}")]
    SspiError(#[from] sspi::Error),

    #[error("Provided buffer size too small to contain {data_type}")]
    BufferTooSmall {
        data_type: &'static str,
        required: Option<usize>,
        provided: usize,
    },

    #[error("Url parse error: {0}")]
    UrlParseError(#[from] url::ParseError),
    #[error("Unsupported authentication mechanism: {0}")]
    UnsupportedAuthenticationMechanism(String),
    #[error("Compression error: {0}")]
    CompressionError(#[from] crate::compression::CompressionError),
    #[error("Message processing failed. {0}")]
    MessageProcessingError(String),
    #[error("Operation timed out: {0:?}, took >{1:?}")]
    OperationTimeout(TimedOutTask, std::time::Duration),
    #[error("Lock error.")]
    LockError,
    #[cfg(feature = "async")]
    #[error("Task join error.")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("Acquire Error: {0}")]
    AcquireError(#[from] AcquireError),
    #[cfg(not(feature = "async"))]
    #[error("Thread join error: {0}")]
    JoinError(String),
    #[cfg(not(feature = "async"))]
    #[error("Channel recv error.")]
    ChannelRecvError(#[from] std::sync::mpsc::RecvError),
    #[error("Unexpected message with ID {0} (exp {1}).")]
    UnexpectedMessageId(u64, u64),
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
    #[error("Unsupported operation: {0}")]
    UnsupportedOperation(String),
    #[error("Unable to perform DFS resolution: {0}")]
    DfsError(UncPath),
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Channel {1} for session {0} not found.")]
    ChannelNotFound(u64, u32),

    #[error("RPC error: {0}")]
    RpcError(#[from] smb_rpc::SmbRpcError),
    #[error("SMB message error: {0}")]
    SmbMessageError(#[from] smb_msg::SmbMsgError),
    #[error("SMB FSCC error: {0}")]
    FsccError(#[from] smb_fscc::SmbFsccError),

    #[error("Transport error: {0}")]
    TransportError(#[from] TransportError),

    #[error("Other error: {0}")]
    Other(&'static str),
}

impl<T> From<PoisonError<T>> for Error {
    fn from(_: PoisonError<T>) -> Self {
        Error::LockError
    }
}
