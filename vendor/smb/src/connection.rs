pub mod config;
pub mod connection_info;
pub mod preauth_hash;
pub mod transformer;
pub mod worker;

use crate::compression;
use crate::connection::preauth_hash::PreauthHashState;
use crate::dialects::DialectImpl;
use crate::session::ChannelMessageHandler;
use crate::sync_helpers::*;
use crate::{Error, crypto, msg_handler::*, session::Session};
use binrw::prelude::*;
pub use config::*;
use connection_info::{ConnectionInfo, NegotiatedProperties};
use maybe_async::*;
use rand::RngCore;
use rand::rngs::OsRng;
use smb_dtyp::*;
use smb_msg::{Command, Response, negotiate::*, plain::*, smb1::SMB1NegotiateMessage};
use smb_transport::*;
use std::cmp::max;
use std::collections::HashMap;
use std::net::SocketAddr;
#[cfg(feature = "multi_threaded")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
pub use transformer::TransformError;
use worker::{Worker, WorkerImpl};

/// Represents an SMB connection.
///
/// Each SMB connection has a single matching transport (e.g. TCP connection).
/// Usually, most use cases require a single connection per server-client communication.
pub struct Connection {
    handler: HandlerReference<ConnectionMessageHandler>,
    config: ConnectionConfig,

    server_name: String,
    server_address: SocketAddr,
}

#[maybe_async(AFIT)]
impl Connection {
    /// Creates a new SMB connection, specifying a server configuration, without connecting to a server.
    /// Use the [`connect`](Connection::connect) method to establish a connection.
    pub fn build(
        server_name: &str,
        server_address: SocketAddr,
        client_guid: Guid,
        config: ConnectionConfig,
    ) -> crate::Result<Self> {
        config.validate()?;
        Ok(Connection {
            handler: HandlerReference::new(ConnectionMessageHandler::new(
                client_guid,
                config.credits_backlog,
            )),
            config,
            server_name: server_name.to_string(),
            server_address,
        })
    }

    /// Creates a SMB connection for an alternate channel,
    /// for the specified existing, primary connection.
    ///
    /// Returns the ID of the channel in the existing session.
    pub async fn bind_session(
        &self,
        primary_session: &Session,
        identity: sspi::AuthIdentity,
    ) -> crate::Result<u32> {
        log::debug!("Binding alternate session to new connection");

        if self.conn_info().is_none() {
            return Err(Error::InvalidState(
                "Connection must be negotiated before binding a session.".to_string(),
            ));
        }

        if !self
            .conn_info()
            .as_ref()
            .unwrap()
            .negotiation
            .caps
            .multi_channel()
        {
            return Err(Error::InvalidState(
                "Server does not support multichannel.".to_string(),
            ));
        }

        primary_session
            .bind(
                identity,
                &self.handler,
                self.handler.conn_info.get().unwrap(),
            )
            .await
    }

    /// Connects to the specified server, if it is not already connected, and negotiates the connection.
    pub async fn connect(&self) -> crate::Result<()> {
        if self.handler.worker().is_some() {
            return Err(Error::InvalidState("Already connected".into()));
        }

        let mut transport = make_transport(&self.config.transport, self.config.timeout())?;

        let mut actual_connect_address = self.server_address;
        if actual_connect_address.port() == 0 {
            actual_connect_address
                .set_port(self.config.port.unwrap_or_else(|| transport.default_port()));
        }

        log::info!(
            "Connecting to {} (at {actual_connect_address})...",
            &self.server_name,
        );
        transport
            .connect(&self.server_name, actual_connect_address)
            .await?;

        log::info!("Connected to {}. Negotiating.", &self.server_name);
        self._negotiate(transport, self.config.smb2_only_negotiate)
            .await?;

        Ok(())
    }

    /// Starts a new connection from an existing, connected transport.
    ///
    /// This is especially useful when you want to use a custom transport - otherwise,
    /// You should create a connection using the [`Client`][`crate::Client`] API.
    ///
    /// # Arguments
    /// * `transport` - The transport to use for the connection.
    /// * `server` - The name or address of the server to connect to.
    /// * `config` - The connection configuration. Note that the [`ConnectionConfig::transport`] field is NOT used when
    ///   creating the connection.
    /// # Returns
    /// A new [`Connection`] object with the specified transport and configuration.
    ///
    ///
    /// ```no_run
    /// # use smb::*;
    /// # use std::time::Duration;
    /// use smb_transport::TcpTransport;
    /// # #[cfg(not(feature = "async"))] fn main() {}
    /// #[cfg(feature = "async")]
    /// # #[tokio::main]
    /// # async fn main() -> Result<()> {
    /// let custom_tcp_transport = Box::new(TcpTransport::new(Duration::from_millis(10))); // you may also implement you own transport!
    /// let my_connection_config = ConnectionConfig { ..Default::default() };
    /// let connection = Connection::from_transport(custom_tcp_transport, "server", Guid::generate(), my_connection_config).await?;
    /// # Ok(())}
    /// ```
    pub async fn from_transport(
        transport: Box<dyn SmbTransport>,
        server: &str,
        client_guid: Guid,
        config: ConnectionConfig,
    ) -> crate::Result<Self> {
        let conn = Self::build(server, transport.remote_address()?, client_guid, config)?;
        conn._negotiate(transport, conn.config.smb2_only_negotiate)
            .await?;
        Ok(conn)
    }

    /// Closes the connection, and all of it's managed resources.
    ///
    /// Any session, tree, or file handles associated with the connection will be unusable after
    /// calling this method.
    ///
    /// See also [`Client::close`][`crate::Client::close`].
    pub async fn close(&self) -> crate::Result<()> {
        match self.handler.worker() {
            Some(c) => c.stop().await,
            None => Ok(()),
        }
    }

    /// Switches the protocol to SMB2 against the server if required,
    /// and wraps the transport in a SMB2 worker.
    #[maybe_async]
    async fn _negotiate_switch_to_smb2(
        &self,
        mut transport: Box<dyn SmbTransport>,
        smb2_only_neg: bool,
    ) -> crate::Result<Arc<WorkerImpl>> {
        // Multi-protocol negotiation: Begin with SMB1, expect SMB2.
        if !smb2_only_neg {
            log::debug!("Negotiating multi-protocol: Sending SMB1");
            // 1. Send SMB1 negotiate request
            let msg_bytes: Vec<u8> = SMB1NegotiateMessage::default().try_into()?;
            transport.send(&IoVec::from(msg_bytes)).await?;

            log::debug!("Sent SMB1 negotiate request, Receieving SMB2 response");
            // 2. Expect SMB2 negotiate response
            let recieved_bytes = transport.receive().await?;
            let response = Response::try_from(recieved_bytes.as_ref())?;
            let message = match response {
                Response::Plain(m) => m,
                _ => {
                    return Err(Error::InvalidMessage(
                        "Expected SMB2 negotiate response, got SMB1".to_string(),
                    ));
                }
            };

            let smb2_negotiate_response = message.content.to_negotiate()?;

            // 3. Make sure dialect is smb2*, message ID is 0.
            if smb2_negotiate_response.dialect_revision != NegotiateDialect::Smb02Wildcard {
                return Err(Error::InvalidMessage(
                    "Expected SMB2 wildcard dialect".to_string(),
                ));
            }
            if message.header.message_id != 0 {
                return Err(Error::InvalidMessage("Expected message ID 0".to_string()));
            }
            if message.header.credit_charge != 0 || message.header.credit_request != 1 {
                return Err(Error::InvalidMessage(
                    "Expected credit charge 0 and request 1 for initial message.".to_string(),
                ));
            }
            // Increase sequence number.
            self.handler.curr_msg_id.fetch_add(1, Ordering::SeqCst);
        }

        WorkerImpl::start(transport, self.config.timeout()).await
    }

    /// This method perofrms the SMB2 negotiation.
    #[maybe_async]
    async fn _negotiate_smb2(
        &self,
        server_address: std::net::SocketAddr,
    ) -> crate::Result<ConnectionInfo> {
        // Confirm that we're not already negotiated.
        if self.handler.conn_info.get().is_some() {
            return Err(Error::InvalidState("Already negotiated".into()));
        }

        log::debug!("Negotiating SMB2");

        // List possible versions to run with.
        let min_dialect = self.config.min_dialect.unwrap_or(Dialect::MIN);
        let max_dialect = self.config.max_dialect.unwrap_or(Dialect::MAX);
        let dialects: Vec<Dialect> = Dialect::ALL
            .iter()
            .filter(|dialect| **dialect >= min_dialect && **dialect <= max_dialect)
            .copied()
            .collect();

        if dialects.is_empty() {
            return Err(Error::InvalidConfiguration(
                "No dialects to negotiate".to_string(),
            ));
        }

        let encryption_algos = if !self.config.encryption_mode.is_disabled() {
            crypto::ENCRYPTING_ALGOS.into()
        } else {
            vec![]
        };

        // Send SMB2 negotiate request
        let (request_status, response) = self
            .handler
            .sendor_recv(
                OutgoingMessage::new(
                    self._make_smb2_neg_request(
                        dialects,
                        crypto::SIGNING_ALGOS.to_vec(),
                        encryption_algos,
                        compression::SUPPORTED_ALGORITHMS.to_vec(),
                    )
                    .into(),
                )
                .with_return_raw_data(true),
            )
            .await?;

        let smb2_negotiate_response = response.message.content.to_negotiate()?;

        // well, only 3.1 is supported for starters.
        let dialect_rev = smb2_negotiate_response.dialect_revision.try_into()?;
        if dialect_rev > max_dialect || dialect_rev < min_dialect {
            return Err(Error::NegotiationError(
                "Server selected an unsupported dialect.".into(),
            ));
        }

        let dialect_impl = DialectImpl::new(dialect_rev);
        let mut negotiation = NegotiatedProperties {
            server_guid: smb2_negotiate_response.server_guid,
            caps: smb2_negotiate_response.capabilities,
            max_transact_size: smb2_negotiate_response.max_transact_size,
            max_read_size: smb2_negotiate_response.max_read_size,
            max_write_size: smb2_negotiate_response.max_write_size,
            auth_buffer: smb2_negotiate_response.buffer.clone(),
            signing_algo: None,
            encryption_cipher: None,
            compression: None,
            dialect_rev,
        };

        dialect_impl.process_negotiate_request(
            &smb2_negotiate_response,
            &mut negotiation,
            &self.config,
        )?;
        if ((!u32::from_le_bytes(dialect_impl.get_negotiate_caps_mask().into_bytes()))
            & u32::from_le_bytes(negotiation.caps.into_bytes()))
            != 0
        {
            return Err(Error::NegotiationError(
                "Server capabilities are invalid for the selected dialect.".into(),
            ));
        }

        log::trace!(
            "Negotiated SMB results: dialect={:?}, state={:?}",
            dialect_rev,
            &negotiation
        );

        let preauth_hash = if dialect_impl.preauth_hash_supported() {
            PreauthHashState::begin()
                .next(
                    &request_status
                        .raw
                        .expect("Preauth hash must be calculated for supported dialect!"),
                )
                .next(&response.raw)
        } else {
            PreauthHashState::unsupported()
        };

        Ok(ConnectionInfo {
            negotiation,
            dialect: dialect_impl,
            config: self.config.clone(),
            server_name: self.server_name.clone(),
            preauth_hash,
            client_guid: self.handler.client_guid,
            server_address,
        })
    }

    /// Creates an SMB2 negotiate request.
    fn _make_smb2_neg_request(
        &self,
        supported_dialects: Vec<Dialect>,
        signing_algorithms: Vec<SigningAlgorithmId>,
        encrypting_algorithms: Vec<EncryptionCipher>,
        compression_algorithms: Vec<CompressionAlgorithm>,
    ) -> NegotiateRequest {
        let client_guid = self.handler.client_guid;
        let client_netname = self
            .config
            .client_name
            .clone()
            .unwrap_or_else(|| "smb-client".to_string());
        let has_signing = !signing_algorithms.is_empty();
        let has_encryption = !encrypting_algorithms.is_empty();

        // Context list supported on SMB3.1.1+
        let ctx_list = if supported_dialects.contains(&Dialect::Smb0311) {
            let mut preauth_integrity_hash = [0u8; 32];
            OsRng.fill_bytes(&mut preauth_integrity_hash);
            let mut ctx_list = vec![
                PreauthIntegrityCapabilities {
                    hash_algorithms: vec![HashAlgorithm::Sha512],
                    salt: preauth_integrity_hash.to_vec(),
                }
                .into(),
                NetnameNegotiateContextId {
                    netname: client_netname.into(),
                }
                .into(),
                EncryptionCapabilities {
                    ciphers: encrypting_algorithms,
                }
                .into(),
                CompressionCapabilities {
                    flags: CompressionCapsFlags::new()
                        .with_chained(!compression_algorithms.is_empty()),
                    compression_algorithms,
                }
                .into(),
                SigningCapabilities { signing_algorithms }.into(),
            ];
            // QUIC
            #[cfg(feature = "quic")]
            if matches!(self.config.transport, TransportConfig::Quic(_)) {
                ctx_list.push(NegotiateContext {
                    context_type: NegotiateContextType::TransportCapabilities,
                    data: NegotiateContextValue::TransportCapabilities(
                        TransportCapabilities::new().with_accept_transport_layer_security(true),
                    ),
                });
            }
            // TODO: Add to config
            if cfg!(feature = "rdma") {
                ctx_list.push(NegotiateContext {
                    context_type: NegotiateContextType::RdmaTransformCapabilities,
                    data: NegotiateContextValue::RdmaTransformCapabilities(
                        RdmaTransformCapabilities {
                            transforms: vec![RdmaTransformId::None],
                        },
                    ),
                });
            }
            Some(ctx_list)
        } else {
            None
        };

        // Set capabilities to 0 if no SMB3 dialects are supported.
        let capabilities = if supported_dialects.iter().max() < Some(&Dialect::Smb030) {
            GlobalCapabilities::new()
        } else {
            let mut capabilities = GlobalCapabilities::new()
                .with_dfs(true)
                .with_leasing(true)
                .with_large_mtu(true)
                .with_multi_channel(self.config.multichannel.is_enabled())
                .with_persistent_handles(false)
                .with_directory_leasing(true);

            if has_encryption {
                capabilities.set_encryption(true);
            }

            // Enable notifications by client config + build config.
            if !self.config.disable_notifications
                && cfg!(not(feature = "single_threaded"))
                && supported_dialects.contains(&Dialect::Smb0311)
            {
                capabilities.set_notifications(true);
            }
            capabilities
        };

        let security_mode = NegotiateSecurityMode::new().with_signing_enabled(has_signing);

        NegotiateRequest {
            security_mode,
            capabilities,
            client_guid,
            dialects: supported_dialects,
            negotiate_context_list: ctx_list,
        }
    }

    /// Performs SMB negotiation post-connect.
    #[maybe_async]
    async fn _negotiate(
        &self,
        transport: Box<dyn SmbTransport>,
        smb2_only_neg: bool,
    ) -> crate::Result<()> {
        if self.handler.conn_info.get().is_some() {
            return Err(Error::InvalidState("Already negotiated".into()));
        }

        let server_address = transport.remote_address()?;
        // Negotiate SMB1, Switch to SMB2
        let worker = self
            ._negotiate_switch_to_smb2(transport, smb2_only_neg)
            .await?;

        self.handler.worker.set(worker).unwrap();

        // Negotiate SMB2
        let info = self._negotiate_smb2(server_address).await?;

        self.handler
            .worker
            .get()
            .ok_or("Worker is uninitialized")
            .unwrap()
            .negotaite_complete(&info)
            .await;

        #[cfg(not(feature = "single_threaded"))]
        if !self.config.disable_notifications && info.negotiation.caps.notifications() {
            log::debug!("Starting Notification job.");
            self.handler.handler.start_notify().await?;
            log::debug!("Notification job started.");
        }

        self.handler.conn_info.set(Arc::new(info)).unwrap();

        log::debug!("Negotiation successful");
        Ok(())
    }

    /// Starts a new session for the current connection, and authenticates it
    /// using the provided user name and password.
    ///
    /// ## Arguments
    /// * `user_name` - The user to authenticate with.
    /// * `password` - The password for the user.
    ///
    /// ## Returns
    /// A [`Session`] object representing the authenticated session.
    ///
    /// ## Notes:
    /// * Use the [`ConnectionConfig`] to configure authentication options.
    pub async fn authenticate(&self, identity: sspi::AuthIdentity) -> crate::Result<Session> {
        let session = Session::create(
            identity,
            &self.handler,
            self.handler.conn_info.get().unwrap(),
        )
        .await?;
        let session_handler = session.handler.weak();
        self.handler
            .sessions
            .lock()
            .await?
            .insert(session.session_id(), session_handler);
        Ok(session)
    }

    /// Returns the connection information, if the connection has been negotiated.
    /// Otherwise, returns `None`.
    pub fn conn_info(&self) -> Option<&Arc<ConnectionInfo>> {
        self.handler.conn_info.get()
    }
}

/// This struct is the internal message handler for the SMB client.
pub(crate) struct ConnectionMessageHandler {
    client_guid: Guid,

    /// The number of extra credits to be requested by the client
    /// to enable larger requests/multiple outstanding requests.
    credits_backlog: u16,

    worker: OnceCell<Arc<WorkerImpl>>,

    #[cfg(feature = "async")]
    /// Cancellation token for stopping notifications.
    stop_notifications: CancellationToken,
    #[cfg(feature = "multi_threaded")]
    /// Flag to stop notifications.
    stop_notifications: Arc<AtomicBool>,

    /// Holds the sessions created by this connection.
    sessions: Mutex<HashMap<u64, Weak<ChannelMessageHandler>>>,

    // Negotiation-related state.
    conn_info: OnceCell<Arc<ConnectionInfo>>,

    /// Number of credits available to the client at the moment, for the next requests.
    curr_credits: Semaphore,
    /// The current message ID to be used in the next message.
    curr_msg_id: AtomicU64,
    /// The number of credits granted to the client by the server, including the being-used ones.
    /// This field is used ONLY when large MTU is enabled.
    credit_pool: AtomicU16,
}

impl ConnectionMessageHandler {
    fn new(client_guid: Guid, credits_backlog: Option<u16>) -> ConnectionMessageHandler {
        ConnectionMessageHandler {
            client_guid,
            worker: OnceCell::new(),
            conn_info: OnceCell::new(),
            credits_backlog: credits_backlog.unwrap_or(128),
            curr_credits: Semaphore::new(1),
            curr_msg_id: AtomicU64::new(0),
            credit_pool: AtomicU16::new(1),
            #[cfg(not(feature = "single_threaded"))]
            stop_notifications: Default::default(),
            sessions: Mutex::new(HashMap::with_capacity(1)),
        }
    }

    pub fn worker(&self) -> Option<&Arc<WorkerImpl>> {
        self.worker.get()
    }

    const SET_CREDIT_CHARGE_CMDS: &'static [Command] = &[
        Command::Read,
        Command::Write,
        Command::Ioctl,
        Command::QueryDirectory,
    ];

    const CREDIT_CALC_RATIO: u32 = 65536;
    const CREDITS_PER_MSG_NO_LARGE_MTU: u32 = 1;

    #[maybe_async]
    async fn process_sequence_outgoing(&self, msg: &mut OutgoingMessage) -> crate::Result<()> {
        if let Some(neg) = self.conn_info.get() {
            if neg.negotiation.caps.large_mtu() {
                // Calculate the cost of the message (charge).
                let cost = if Self::SET_CREDIT_CHARGE_CMDS.contains(&msg.message.header.command) {
                    let send_payload_size = msg.message.content.req_payload_size();
                    let expected_response_payload_size = msg.message.content.expected_resp_size();
                    (1 + (max(send_payload_size, expected_response_payload_size) - 1)
                        / Self::CREDIT_CALC_RATIO)
                        .try_into()
                        .unwrap()
                } else {
                    1
                };

                // First, acquire credits from the semaphore, and forget them.
                // They may be returned via the response message, at `process_sequence_incoming` below.
                self.curr_credits.acquire_many(cost as u32).await?.forget();

                let mut request = cost;
                // Request additional credits if required: if balance < extra, add to request the diff:
                let current_pool_size = self.credit_pool.load(Ordering::SeqCst);
                if current_pool_size < self.credits_backlog {
                    request += self.credits_backlog - current_pool_size;
                }

                msg.message.header.credit_charge = cost;
                msg.message.header.credit_request = request;
                msg.message.header.message_id =
                    self.curr_msg_id.fetch_add(cost as u64, Ordering::SeqCst);

                return Ok(());
            } else {
                debug_assert_eq!(msg.message.header.credit_request, 0);
                debug_assert_eq!(msg.message.header.credit_charge, 0);
            }
        }

        // Default case: logically waiting for single credit per message,
        // which will make the client wait for next response before allowing next request.
        self.curr_credits
            .acquire_many(Self::CREDITS_PER_MSG_NO_LARGE_MTU)
            .await?
            .forget();
        debug_assert!(
            self.curr_credits.available_permits() == 0,
            "Expected 0 credits available with no large mtu, got {}",
            self.curr_credits.available_permits()
        );

        msg.message.header.message_id = self
            .curr_msg_id
            .fetch_add(Self::CREDITS_PER_MSG_NO_LARGE_MTU as u64, Ordering::SeqCst);

        Ok(())
    }

    #[maybe_async]
    async fn process_sequence_incoming(&self, msg: &IncomingMessage) -> crate::Result<()> {
        if let Some(neg) = self.conn_info.get()
            && neg.negotiation.caps.large_mtu()
        {
            let granted_credits = msg.message.header.credit_request;
            let charged_credits = msg.message.header.credit_charge;
            // Update the pool size - return how many EXTRA credits were granted.
            // also, handle the case where the server granted less credits than charged.
            if charged_credits > granted_credits {
                self.credit_pool
                    .fetch_sub(charged_credits - granted_credits, Ordering::SeqCst);
            } else {
                self.credit_pool
                    .fetch_add(granted_credits - charged_credits, Ordering::SeqCst);
            }

            // Return the credits to the pool.
            self.curr_credits.add_permits(granted_credits as usize);
            return Ok(());
        }

        // Default case: return a single credit to the pool.
        self.curr_credits
            .add_permits(Self::CREDITS_PER_MSG_NO_LARGE_MTU as usize);
        debug_assert!(
            self.curr_credits.available_permits() <= Self::CREDITS_PER_MSG_NO_LARGE_MTU as usize,
            "Expected at most {} credits available with no large mtu, got {}",
            Self::CREDITS_PER_MSG_NO_LARGE_MTU,
            self.curr_credits.available_permits()
        );
        Ok(())
    }

    #[cfg(feature = "async")]
    async fn start_notify(self: &Arc<Self>) -> crate::Result<()> {
        let worker = self.worker.get().unwrap();
        let worker = worker.clone();
        const CHANNEL_BUFFER_SIZE: usize = 10;
        let (tx, mut rx) = tokio::sync::mpsc::channel(CHANNEL_BUFFER_SIZE);
        worker.start_notify_channel(tx)?;
        let stop_notification = self.stop_notifications.clone();
        let self_clone = self.clone();
        tokio::spawn(async move {
            loop {
                select! {
                    _ = stop_notification.cancelled() => {
                        log::info!("Notification handler cancelled.");
                        break;
                    }
                    else => {
                        while let Some(msg) = rx.recv().await {
                            self_clone.notify(msg).await.unwrap_or_else(|e| {
                                log::error!("Error handling notification: {e:?}");
                            });
                        }
                    }
                }
            }
            log::info!("Notification handler thread stopped.");
        });
        Ok(())
    }

    #[cfg(feature = "multi_threaded")]
    fn start_notify(self: &Arc<Self>) -> crate::Result<()> {
        let (tx, rx) = mpsc::channel();
        let worker = self.worker.get().unwrap();
        worker.start_notify_channel(tx)?;

        const POLLING_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
        let stopped_ref = self.stop_notifications.clone();
        let self_clone = self.clone();
        std::thread::spawn(move || {
            while !stopped_ref.load(Ordering::SeqCst) {
                match rx.recv_timeout(POLLING_INTERVAL) {
                    Ok(notification) => {
                        self_clone.notify(notification).unwrap_or_else(|e| {
                            log::error!("Error handling notification: {e:?}");
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            log::info!("Notification handler thread stopped.");
        });
        Ok(())
    }

    #[cfg(not(feature = "single_threaded"))]
    pub fn stop_notify(&self) {
        #[cfg(feature = "async")]
        self.stop_notifications.cancel();
        #[cfg(not(feature = "async"))]
        self.stop_notifications.store(true, Ordering::SeqCst);
        log::info!("Notification handler stopped.");
    }
}

impl MessageHandler for ConnectionMessageHandler {
    #[maybe_async]
    async fn sendo(&self, mut msg: OutgoingMessage) -> crate::Result<SendMessageResult> {
        let priority_value = match self.conn_info.get() {
            Some(neg_info) => match neg_info.negotiation.dialect_rev {
                Dialect::Smb0311 => 1,
                _ => 0,
            },
            None => 0,
        };
        msg.message.header.flags = msg.message.header.flags.with_priority_mask(priority_value);

        let is_cancel = msg.message.content.as_cancel().is_ok();
        if !is_cancel {
            self.process_sequence_outgoing(&mut msg).await?;
        } else if msg.message.header.message_id == 0 {
            return Err(Error::InvalidState(
                "Cancel message must have a valid message ID".into(),
            ));
        }

        self.worker
            .get()
            .ok_or(Error::InvalidState("Worker is uninitialized".into()))?
            .send(msg)
            .await
    }

    #[maybe_async]
    async fn recvo(&self, options: ReceiveOptions<'_>) -> crate::Result<IncomingMessage> {
        let msg = self.worker.get().unwrap().receive(&options).await?;

        // Command matching (if needed).
        if let Some(cmd) = options.cmd
            && msg.message.header.command != cmd
        {
            return Err(Error::UnexpectedMessageCommand(msg.message.header.command));
        }

        // Direction matching.
        if !msg.message.header.flags.server_to_redir() {
            return Err(Error::InvalidMessage(
                "Expected server-to-redir message".into(),
            ));
        }

        self.process_sequence_incoming(&msg).await?;

        // Expected status matching. Error if no match.
        if !options
            .status
            .iter()
            .any(|s| msg.message.header.status == *s as u32)
        {
            if let ResponseContent::Error(error_res) = msg.message.content {
                return Err(Error::ReceivedErrorMessage(
                    msg.message.header.status,
                    error_res,
                ));
            }
            return Err(Error::UnexpectedMessageStatus(msg.message.header.status));
        }

        Ok(msg)
    }

    #[maybe_async]
    async fn notify(&self, msg: IncomingMessage) -> crate::Result<()> {
        if msg.message.header.session_id == 0 {
            log::warn!("Received notification without session ID: {msg:?}");
            return Ok(());
        }

        // Avoid holding the lock while notifying the session further.
        let session = {
            let sessions = self.sessions.lock().await?;
            let session = sessions.get(&msg.message.header.session_id);

            if session.is_none() {
                log::warn!(
                    "Received notification for unknown session ID {}: {msg:?}",
                    msg.message.header.session_id
                );
                return Ok(());
            }

            session.unwrap().upgrade().ok_or_else(|| {
                Error::InvalidState(format!(
                    "Session {} is no longer available",
                    msg.message.header.session_id
                ))
            })?
        };

        session.notify(msg).await?;
        Ok(())
    }
}

#[cfg(not(feature = "async"))]
impl Drop for ConnectionMessageHandler {
    fn drop(&mut self) {
        #[cfg(not(feature = "single_threaded"))]
        self.stop_notify();

        if let Some(worker) = self.worker.take() {
            worker.stop().ok();
        }
    }
}

#[cfg(feature = "async")]
impl Drop for ConnectionMessageHandler {
    fn drop(&mut self) {
        #[cfg(not(feature = "single_threaded"))]
        self.stop_notify();

        let worker = match self.worker.take() {
            Some(worker) => worker,
            None => return,
        };

        tokio::task::spawn(async move {
            worker.stop().await.ok();
        });
    }
}
