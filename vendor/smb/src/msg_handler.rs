use maybe_async::*;
use smb_msg::{Command, PlainRequest, PlainResponse, RequestContent, Status};
use smb_transport::IoVec;
#[cfg(not(feature = "async"))]
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, atomic::AtomicU64};
#[cfg(feature = "async")]
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct OutgoingMessage {
    pub message: PlainRequest,

    pub return_raw_data: bool,

    /// Ask the sender to compress the message before sending, if possible.
    pub compress: bool,
    /// Ask the sender to encrypt the message before sending, if possible.
    pub encrypt: bool,
    // Signing is set through message/header/flags/signed.
    /// Whether this request also expects a response.
    /// This value defaults to true.
    pub has_response: bool,

    /// Zero copy support
    pub additional_data: Option<Arc<[u8]>>,

    /// Channel ID to use for this message, if any.
    pub channel_id: Option<u32>,
}

impl OutgoingMessage {
    pub fn new(content: RequestContent) -> OutgoingMessage {
        OutgoingMessage {
            message: PlainRequest::new(content),
            return_raw_data: false,
            compress: true,
            encrypt: false,
            has_response: true,
            additional_data: None,
            channel_id: None,
        }
    }

    pub fn with_additional_data(mut self, data: Arc<[u8]>) -> Self {
        self.additional_data = Some(data);
        self
    }

    pub fn with_return_raw_data(mut self, return_raw_data: bool) -> Self {
        self.return_raw_data = return_raw_data;
        self
    }

    pub fn with_encrypt(mut self, encrypt: bool) -> Self {
        self.encrypt = encrypt;
        self
    }

    pub fn with_channel_id(mut self, channel_id: Option<u32>) -> Self {
        self.channel_id = channel_id;
        self
    }
}

#[derive(Debug)]
pub struct SendMessageResult {
    // The message ID for the sent message.
    pub msg_id: u64,
    // If finalized, this is set.
    pub raw: Option<IoVec>,
}

impl SendMessageResult {
    pub fn new(msg_id: u64, raw: Option<IoVec>) -> SendMessageResult {
        SendMessageResult { msg_id, raw }
    }
}

#[derive(Debug)]
pub struct IncomingMessage {
    pub message: PlainResponse,
    /// The raw message received from the server, after applying transformations (e.g. decompression).
    pub raw: IoVec,

    // How did the message arrive?
    pub form: MessageForm,

    pub source_channel_id: Option<u32>,
}

impl IncomingMessage {
    pub fn new(message: PlainResponse, raw: IoVec, form: MessageForm) -> IncomingMessage {
        IncomingMessage {
            message,
            raw,
            form,
            source_channel_id: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct MessageForm {
    pub compressed: bool,
    pub encrypted: bool,
    pub signed: bool,
}

impl MessageForm {
    pub fn signed_or_encrypted(&self) -> bool {
        self.signed || self.encrypted
    }
}

#[derive(Debug)]
pub struct AsyncMessageIds {
    pub msg_id: AtomicU64,
    pub async_id: AtomicU64,
}

impl AsyncMessageIds {
    pub fn reset(&self) {
        self.set(u64::MAX, u64::MAX);
    }
    pub fn set(&self, msg_id: u64, async_id: u64) {
        self.msg_id
            .store(msg_id, std::sync::atomic::Ordering::SeqCst);
        self.async_id
            .store(async_id, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for AsyncMessageIds {
    fn default() -> Self {
        Self {
            msg_id: AtomicU64::new(u64::MAX),
            async_id: AtomicU64::new(u64::MAX),
        }
    }
}

/// Options for receiving a message.
///
/// Use a builder pattern to set the options:
/// ```
/// use smb_msg::*;
/// use smb::msg_handler::ReceiveOptions;
///
/// let options = ReceiveOptions::new()
///    .with_status(&[Status::Success])
///    .with_cmd(Some(Command::Negotiate));
/// ```
#[derive(Debug, Clone)]
pub struct ReceiveOptions<'a> {
    /// The expected status(es) of the received message.
    /// If the received message has a different status, an error will be returned.
    pub status: &'a [Status],

    /// If set, this command will be checked against the received command.
    /// If not set, no check will be performed.
    pub cmd: Option<Command>,

    /// When receiving a message, only messages with this msg_id will be returned.
    /// This is mostly used for async message handling, where the client is waiting for a specific message.
    pub msg_id: u64,

    /// The channel ID to receive messages from, if any.
    pub channel_id: Option<u32>,

    /// Whether to allow (and wait for) async responses.
    /// If set to false, an async response from the server will trigger an error.
    /// If set to true, the handler will allow async messages to be received,
    /// and will make the caller wait until the final async response is received --
    /// the async response with status other than [`Status::Pending`].
    ///
    /// When using crate feature `async`, see [`async_cancel`][Self::async_cancel].
    pub allow_async: bool,

    #[cfg(feature = "async")]
    /// An optional cancellation token to cancel the receive operation,
    /// if it's an async operation.
    pub async_cancel: Option<CancellationToken>,

    #[cfg(not(feature = "async"))]
    /// An atomic boolean flag to cancel the receive operation,
    /// if it's an async operation.
    pub async_cancel: Option<Arc<AtomicBool>>,

    /// An optional atomic u64 to update with a message ID + async ID that is being
    /// waited for. This is useful for tracking the async message ID
    /// across multiple threads.
    pub async_msg_ids: Option<Arc<AsyncMessageIds>>,

    /// A timeout for the receive operation.
    /// If not set, the default timeout of the connection is used.
    pub timeout: Option<std::time::Duration>,
}

impl<'a> ReceiveOptions<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status(mut self, status: &'a [Status]) -> Self {
        self.status = status;
        self
    }

    pub fn with_cmd(mut self, cmd: Option<Command>) -> Self {
        self.cmd = cmd;
        self
    }

    pub fn with_msg_id_filter(mut self, msg_id: u64) -> Self {
        self.msg_id = msg_id;
        self
    }

    pub fn with_allow_async(mut self, allow_async: bool) -> Self {
        self.allow_async = allow_async;
        self
    }

    #[cfg(feature = "async")]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.async_cancel = Some(token);
        self
    }

    #[cfg(not(feature = "async"))]
    pub fn with_cancellation_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.async_cancel = Some(flag);
        self
    }

    pub fn with_async_msg_ids(mut self, async_msg_ids: Arc<AsyncMessageIds>) -> Self {
        self.async_msg_ids = Some(async_msg_ids);
        self
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

impl<'a> Default for ReceiveOptions<'a> {
    fn default() -> Self {
        ReceiveOptions {
            status: &[Status::Success],
            cmd: None,
            msg_id: 0,
            allow_async: false,
            channel_id: None,
            async_cancel: None,
            async_msg_ids: None,
            timeout: None,
        }
    }
}

/// Chain-of-responsibility pattern trait for handling SMB messages
/// outgoing from the client or incoming from the server.
#[maybe_async(AFIT)]
#[allow(async_fn_in_trait)] // We need `async`-ed trait functions for the #[maybe_async] macro.
pub trait MessageHandler {
    /// Send a message to the server, returning the result.
    /// This must be implemented. Each handler in the chain must call the next handler,
    /// after possibly modifying the message.
    async fn sendo(&self, msg: OutgoingMessage) -> crate::Result<SendMessageResult>;

    /// Receive a message from the server, returning the result.
    /// This must be implemented, and must call the next handler in the chain,
    /// if there is one, using the provided `ReceiveOptions`.
    async fn recvo(&self, options: ReceiveOptions) -> crate::Result<IncomingMessage>;

    /// Called when a server-to-client message is received.
    ///
    /// # Arguments
    /// * `msg` - The message received from the server.
    ///
    /// # Returns
    /// A result indicating whether the message was handled successfully.
    ///
    /// # Notes
    /// * This method should generally be implemented by handlers, if there's a chance they are related
    ///   to any supported notification messages.
    /// * Unless the handler finishes handling the message fully, it should call the next handler in the chain.
    /// * Default implementation does nothing.
    async fn notify(&self, msg: IncomingMessage) -> crate::Result<()> {
        log::debug!("Received notification message: {msg:?}");
        Ok(())
    }

    // -- Utility functions, accessible from references via Deref.
    #[maybe_async]
    #[inline]
    async fn send(&self, msg: RequestContent) -> crate::Result<SendMessageResult> {
        self.sendo(OutgoingMessage::new(msg)).await
    }

    #[maybe_async]
    #[inline]
    async fn recv(&self, cmd: Command) -> crate::Result<IncomingMessage> {
        self.recvo(ReceiveOptions::new().with_cmd(Some(cmd))).await
    }

    #[maybe_async]
    #[inline]
    async fn sendor_recvo(
        &self,
        msg: OutgoingMessage,
        mut options: ReceiveOptions<'_>,
    ) -> crate::Result<(SendMessageResult, IncomingMessage)> {
        let channel_id = msg.channel_id;
        // Send the message and wait for the matching response.
        let send_result = self.sendo(msg).await?;

        options.msg_id = send_result.msg_id;
        options.channel_id = channel_id;

        let in_result = self.recvo(options).await?;
        Ok((send_result, in_result))
    }

    #[maybe_async]
    #[inline]
    async fn sendo_recvo(
        &self,
        msg: OutgoingMessage,
        options: ReceiveOptions<'_>,
    ) -> crate::Result<IncomingMessage> {
        self.sendor_recvo(msg, options).await.map(|(_, r)| r)
    }

    #[maybe_async]
    #[inline]
    async fn send_recvo(
        &self,
        msg: RequestContent,
        options: ReceiveOptions<'_>,
    ) -> crate::Result<IncomingMessage> {
        self.sendo_recvo(OutgoingMessage::new(msg), options).await
    }

    #[maybe_async]
    #[inline]
    async fn sendo_recv(&self, msg: OutgoingMessage) -> crate::Result<IncomingMessage> {
        let cmd = msg.message.content.associated_cmd();
        let options = ReceiveOptions::new().with_cmd(Some(cmd));
        self.sendo_recvo(msg, options).await
    }

    #[maybe_async]
    #[inline]
    async fn send_recv(&self, msg: RequestContent) -> crate::Result<IncomingMessage> {
        self.sendo_recv(OutgoingMessage::new(msg)).await
    }

    #[maybe_async]
    #[inline]
    async fn sendor_recv(
        &self,
        msg: OutgoingMessage,
    ) -> crate::Result<(SendMessageResult, IncomingMessage)> {
        self.sendor_recvo(msg, ReceiveOptions::new()).await
    }
}

/// A templated shared reference to an SMB message handler.
///
/// Provides a more ergonomic way to interact with the handler.
/// Provided methods are:
/// - `send*`: Send a message content to the server.
/// - `receive*`: Receive a message from the server.
/// - `send*_receive*`: Send a message and receive a response.
/// - `*o`: Send a message and receive a response with custom options:
///     - `sendo`: Send a message with custom, low-level handler options.
///     - `recvo`: Receive a message with custom, low-level handler options.
pub(crate) struct HandlerReference<T: MessageHandler + ?Sized> {
    pub handler: Arc<T>,
}

impl<T: MessageHandler> HandlerReference<T> {
    pub(crate) fn new(handler: T) -> HandlerReference<T> {
        HandlerReference {
            handler: Arc::new(handler),
        }
    }

    /// Returns a weak reference to the handler.
    pub(crate) fn weak(&self) -> std::sync::Weak<T> {
        Arc::downgrade(&self.handler)
    }
}

// Implement deref that returns the content of Arc<T> above (T)
impl<T: MessageHandler> std::ops::Deref for HandlerReference<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.handler
    }
}

// Clone:
impl<T: MessageHandler> Clone for HandlerReference<T> {
    fn clone(&self) -> Self {
        HandlerReference {
            handler: self.handler.clone(),
        }
    }
}
