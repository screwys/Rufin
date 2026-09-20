use super::*;

pub(crate) type ChannelUpstream = HandlerReference<ConnectionMessageHandler>;

pub struct Channel {
    channel_id: u32,

    pub(crate) handler: HandlerReference<ChannelMessageHandler>,
    pub(crate) conn_info: Arc<ConnectionInfo>,
}

impl Channel {
    #[maybe_async]
    pub(crate) async fn new(
        upstream: &ChannelUpstream,
        conn_info: &Arc<ConnectionInfo>,
        setup_result: &Arc<RwLock<SessionAndChannel>>,
    ) -> crate::Result<Self> {
        let (session_id, channel_id) = {
            let setup_result = setup_result.read().await?;
            let session = setup_result.session.read().await?;
            let channel = setup_result
                .channel
                .as_ref()
                .ok_or_else(|| Error::InvalidState("Channel not set in setup result".into()))?;
            (session.id(), channel.id())
        };
        let handler = ChannelMessageHandler::new(session_id, channel_id, upstream, setup_result);
        Ok(Self {
            channel_id,
            handler,
            conn_info: conn_info.clone(),
        })
    }

    /// Returns the Session ID of this session.
    ///
    /// This ID is the same as the SMB's session id,
    /// so it is unique-per-connection, and may be seen on the wire as well.
    #[inline]
    pub fn session_id(&self) -> u64 {
        self.handler.session_id()
    }

    #[inline]
    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }
}

/// Message handler a specific channel.
///
/// This only makes sense, since sessions are not actually able to send data
/// as "themselves", but rather, through a channel.
pub struct ChannelMessageHandler {
    session_id: u64,
    channel_id: u32,
    upstream: ChannelUpstream,

    session_state: Arc<RwLock<SessionAndChannel>>,
}

#[maybe_async(AFIT)]
impl ChannelMessageHandler {
    fn new(
        session_id: u64,
        channel_id: u32,
        upstream: &ChannelUpstream,
        setup_result: &Arc<RwLock<SessionAndChannel>>,
    ) -> HandlerReference<ChannelMessageHandler> {
        HandlerReference::new(ChannelMessageHandler {
            session_id,
            channel_id,
            upstream: upstream.clone(),
            session_state: setup_result.clone(),
        })
    }

    pub(crate) async fn make_for_setup(
        setup_result: &Arc<RwLock<SessionAndChannel>>,
        upstream: &ChannelUpstream,
    ) -> crate::Result<Self> {
        let session_id = setup_result.read().await?.session.read().await?.id();
        Ok(Self {
            session_id,
            channel_id: u32::MAX,
            upstream: upstream.clone(),
            session_state: setup_result.clone(),
        })
    }

    /// (Internal)
    ///
    /// Verifies an [`IncomingMessage`] for the current session.
    /// This is trustworthy only since we trust the [`Transformer`][crate::connection::transformer::Transformer] implementation
    /// to provide the correct IDs and verify signatures and encryption.
    ///
    /// # Arguments
    /// * `incoming` - The incoming message to verify.
    /// # Returns
    /// An empty [`crate::Result`] if the message is valid, or an error if the message is invalid.
    #[maybe_async]
    async fn _verify_incoming(&self, incoming: &IncomingMessage) -> crate::Result<()> {
        // allow unsigned messages only if the session is anonymous or guest.
        // this is enforced against configuration when setting up the session.
        let (unsigned_allowed, encryption_required) = {
            let session = self.session_state.read().await?;
            let session = session.session.read().await?;
            let encryption_required = session.is_ready() && session.should_encrypt()?;
            (session.allow_unsigned()?, encryption_required)
        };

        // Make sure that it's our session.
        if incoming.message.header.session_id == 0 {
            return Err(Error::InvalidMessage(
                "No session ID in message that got to session!".to_string(),
            ));
        }
        if incoming.message.header.session_id != self.session_id {
            return Err(Error::InvalidMessage(
                "Message not for this session!".to_string(),
            ));
        }
        // Make sure encryption is used when required.
        if !incoming.form.encrypted && encryption_required {
            return Err(Error::InvalidMessage(
                "Message not encrypted, but encryption is required for the session!".to_string(),
            ));
        }
        // and signed, unless allowed not to.
        if !incoming.form.signed_or_encrypted() && !unsigned_allowed {
            return Err(Error::InvalidMessage(
                "Message not signed or encrypted, but signing is required for the session!"
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// **Insecure! Insecure! Insecure!**
    ///
    /// Same as [`ChannelMessageHandler::recvo`], but possible skips security validation.
    /// # Arguments
    /// * `options` - The options for receiving the message.
    /// * `skip_security_validation` - Whether to skip security validation of the incoming message.
    ///   This shall only be used when authentication is still being set up.
    /// # Returns
    /// An [`IncomingMessage`] if the message is valid, or an error if the message is invalid.
    #[maybe_async]
    pub(crate) async fn recvo_internal(
        &self,
        options: ReceiveOptions<'_>,
        skip_security_validation: bool,
    ) -> crate::Result<IncomingMessage> {
        let incoming = self.upstream.recvo(options).await?;

        if !skip_security_validation {
            self._verify_incoming(&incoming).await?;
        } else {
            // Note: this is performed here for extra security,
            // while we could have just checked the session state, let's require
            // the caller to explicitly state that it is okay to skip security validation.
            let session = self.session_state.read().await?;
            let session = session.session.read().await?;
            assert!(
                session.is_initial(),
                "Incorrect internal state: security checks are never skipped, unless the session is still being set up!"
            );
        }

        Ok(incoming)
    }

    /// (Internal)
    ///
    /// Assures the sessions may not be used anymore.
    async fn _invalidate(&self) -> crate::Result<()> {
        self.upstream
            .worker()
            .ok_or_else(|| Error::InvalidState("Worker not available!".to_string()))?
            .session_ended(&self.session_state)
            .await
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn channel_id(&self) -> u32 {
        self.channel_id
    }

    pub fn session_state(&self) -> &Arc<RwLock<SessionAndChannel>> {
        &self.session_state
    }
}

#[maybe_async(AFIT)]
impl MessageHandler for ChannelMessageHandler {
    async fn sendo(&self, mut msg: OutgoingMessage) -> crate::Result<SendMessageResult> {
        {
            let session = self.session_state.read().await?;
            let session = session.session.read().await?;
            if session.is_invalid() {
                return Err(Error::InvalidState("Session is invalid".to_string()));
            }

            // It is possible for a lower level to request encryption.
            if msg.encrypt {
                // Session must be ready to encrypt messages.
                if !session.is_ready() {
                    return Err(Error::InvalidState(
                        "Session is not ready, cannot encrypt message".to_string(),
                    ));
                }
            }
            // Otherwise, we should check the session's configuration.
            else if session.is_ready() || session.is_setting_up() {
                // Encrypt if configured for the session,
                if session.is_ready() && session.should_encrypt()? {
                    msg.encrypt = true;
                }
                // Sign
                else if !session.allow_unsigned()? {
                    msg.message.header.flags.set_signed(true);
                }
            }
        }
        msg.message.header.session_id = self.session_id;
        self.upstream.sendo(msg).await
    }

    async fn recvo(&self, options: ReceiveOptions<'_>) -> crate::Result<IncomingMessage> {
        let incoming = self.upstream.recvo(options).await?;

        self._verify_incoming(&incoming).await?;

        Ok(incoming)
    }

    async fn notify(&self, msg: IncomingMessage) -> crate::Result<()> {
        self._verify_incoming(&msg).await?;

        match &msg.message.content {
            ResponseContent::ServerToClientNotification(s2c_notification) => {
                match s2c_notification.notification {
                    // TODO: Move this to primary session
                    Notification::NotifySessionClosed(_) => self._invalidate().await,
                }
            }
            _ => {
                log::warn!(
                    "Received unexpected message in session handler: {:?}",
                    msg.message.content
                );
                Ok(())
            }
        }
    }
}
