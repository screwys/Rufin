//! Session logic module.
//!
//! This module contains the session setup logic, as well as the session message handling,
//! including encryption and signing of messages.

use crate::UncPath;
use crate::connection::connection_info::ConnectionInfo;
use crate::connection::preauth_hash::{PreauthHashState, PreauthHashValue};
use crate::connection::worker::Worker;
use crate::{
    Error,
    connection::ConnectionMessageHandler,
    crypto::KeyToDerive,
    msg_handler::{
        HandlerReference, IncomingMessage, MessageHandler, OutgoingMessage, ReceiveOptions,
        SendMessageResult,
    },
    sync_helpers::*,
    tree::Tree,
};
use smb_msg::{Notification, ResponseContent, Status, session_setup::*};
use smb_transport::IoVec;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU32};

mod authenticator;
mod channel;
mod encryptor_decryptor;
mod setup;
mod signer;
#[cfg(feature = "kerberos")]
mod sspi_network_client;
mod state;

pub use channel::*;
pub use encryptor_decryptor::{MessageDecryptor, MessageEncryptor};

pub use signer::MessageSigner;
pub use state::{ChannelInfo, SessionInfo};

use setup::*;

pub struct Session {
    primary_channel: Channel,
    alt_channels: RwLock<HashMap<u32, Channel>>,
    channel_counter: AtomicU32,

    // Message handler for this session.
    session_handler: HandlerReference<SessionMessageHandler>,
}

#[maybe_async]
impl Session {
    /// Sets up a new session on the specified connection.
    /// This method is crate-internal; Use [`Connection::authenticate`] to create a new session.
    ///
    /// [Session::bind] may be used instead, to bind an existing session to a new connection.
    pub(crate) async fn create(
        identity: sspi::AuthIdentity,
        upstream: &ChannelUpstream,
        conn_info: &Arc<ConnectionInfo>,
    ) -> crate::Result<Session> {
        const FIRST_CHANNEL_ID: u32 = 0;

        let setup_result = SessionSetup::<SmbSessionNew>::new(
            identity,
            upstream,
            conn_info,
            FIRST_CHANNEL_ID,
            None,
        )
        .await?;

        let primary_channel = Self::_common_setup(setup_result).await?;

        let handler =
            HandlerReference::new(SessionMessageHandler::new(primary_channel.handler.clone()));

        Ok(Session {
            session_handler: handler,
            primary_channel,
            alt_channels: Default::default(),
            channel_counter: AtomicU32::new(FIRST_CHANNEL_ID + 1),
        })
    }

    /// Binds an existing session to a new connection.
    ///
    /// Returns the channel ID (in the scope of the current session) of the newly created channel.
    pub(crate) async fn bind(
        &self,
        identity: sspi::AuthIdentity,
        handler: &HandlerReference<ConnectionMessageHandler>,
        conn_info: &Arc<ConnectionInfo>,
    ) -> crate::Result<u32> {
        if self.conn_info.negotiation.dialect_rev != conn_info.negotiation.dialect_rev {
            return Err(Error::InvalidState(
                "Cannot bind session to connection with different dialect.".to_string(),
            ));
        }
        if self.conn_info.client_guid != conn_info.client_guid {
            return Err(Error::InvalidState(
                "Cannot bind session to connection with different client GUID.".to_string(),
            ));
        }

        {
            let primary_session_state = self.handler.session_state().read().await?;
            let session = primary_session_state.session.read().await?;
            if !session.is_ready() {
                return Err(Error::InvalidState(
                    "Cannot bind session that is not ready.".to_string(),
                ));
            }
            if session.allow_unsigned()? {
                return Err(Error::InvalidState(
                    "Cannot bind session that allows unsigned messages.".to_string(),
                ));
            }
        }

        let new_channel_id = self
            .channel_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let setup_result = SessionSetup::<SmbSessionBind>::new(
            identity,
            handler,
            conn_info,
            new_channel_id,
            Some(self.handler.session_state()),
        )
        .await?;

        let channel = Self::_common_setup(setup_result).await?;
        let channel_handler = channel.handler.clone();

        self.alt_channels
            .write()
            .await?
            .insert(new_channel_id, channel);

        self.session_handler
            .channel_handlers
            .write()
            .await?
            .insert(new_channel_id, channel_handler);

        Ok(new_channel_id)
    }

    async fn _common_setup<T>(mut session_setup: SessionSetup<'_, T>) -> crate::Result<Channel>
    where
        T: SessionSetupProperties,
    {
        let setup_result = session_setup.setup().await?;

        {
            let session = setup_result.read().await?;
            let session = session.session.read().await?;
            log::debug!("Session setup complete.");
            if session.allow_unsigned()? {
                log::debug!("Session is guest/anonymous.");
            }
        };

        let channel = Channel::new(
            session_setup.upstream(),
            session_setup.conn_info(),
            &setup_result,
        )
        .await?;

        Ok(channel)
    }

    /// Connects to the specified tree on the current session.
    /// ## Arguments
    /// * `name` - The name of the tree to connect to.
    pub async fn tree_connect(&self, name: &UncPath) -> crate::Result<Tree> {
        let name = name.clone().with_no_path().to_string();
        let tree = Tree::connect(&name, &self.session_handler, &self.conn_info).await?;
        Ok(tree)
    }

    /// Logs off the session.
    ///
    /// Any resources held by the session will be released,
    /// and any [`Tree`] objects and their resources will be unusable.
    pub async fn logoff(&self) -> crate::Result<()> {
        self.session_handler.logoff().await
    }
}

impl Deref for Session {
    type Target = Channel;

    fn deref(&self) -> &Self::Target {
        &self.primary_channel
    }
}

#[derive(Clone)]
pub struct SessionAndChannel {
    pub session_id: u64,

    pub session: Arc<RwLock<SessionInfo>>,
    pub channel: Option<ChannelInfo>,
}

impl SessionAndChannel {
    pub fn new(session_id: u64, session: Arc<RwLock<SessionInfo>>) -> Self {
        Self {
            session_id,
            session,
            channel: None,
        }
    }

    pub fn set_channel(&mut self, channel: ChannelInfo) {
        self.channel = Some(channel);
    }
}

pub(crate) struct SessionMessageHandler {
    session_id: u64,
    // this is used to speed up access to the primary channel handler.
    primary_channel_id: u32,
    primary_channel: HandlerReference<ChannelMessageHandler>,

    channel_handlers: RwLock<HashMap<u32, HandlerReference<ChannelMessageHandler>>>,

    dropping: AtomicBool,
}

#[maybe_async(AFIT)]
impl SessionMessageHandler {
    pub fn new(primary_channel: HandlerReference<ChannelMessageHandler>) -> Self {
        let session_id = primary_channel.session_id();
        let primary_channel_id = primary_channel.channel_id();
        Self {
            session_id,
            primary_channel_id,
            primary_channel: primary_channel.clone(),
            channel_handlers: RwLock::new(HashMap::from([(primary_channel_id, primary_channel)])),
            dropping: AtomicBool::new(false),
        }
    }

    pub async fn logoff(&self) -> crate::Result<()> {
        if self
            .dropping
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }

        {
            let state = self.primary_channel.session_state().read().await?;
            let state = state.session.read().await?;
            if !state.is_ready() {
                log::trace!("Session not ready, or logged-off already, skipping logoff.");
                return Ok(());
            }
        }

        log::debug!("Logging off session.");

        let _response = self.send_recv(LogoffRequest {}.into()).await?;

        // This also invalidates the session object.
        log::info!("Session logged off.");
        self.primary_channel
            .session_state()
            .read()
            .await?
            .session
            .write()
            .await?
            .invalidate();

        Ok(())
    }

    /// Logs off the session and invalidates it.
    ///
    /// # Notes
    /// This method waits for the logoff response to be received from the server.
    /// It is used when dropping the session.
    #[cfg(feature = "async")]
    async fn logoff_async(&self) {
        self.logoff().await.unwrap_or_else(|e| {
            log::error!("Failed to logoff: {e}");
        });
    }

    #[inline]
    async fn _with_channel<T: WithChannel>(
        &self,
        channel_id: Option<u32>,
        t: T,
    ) -> crate::Result<T::Result> {
        if channel_id.is_none() || channel_id.unwrap() == self.primary_channel_id {
            return t.work(&self.primary_channel).await;
        }

        let channel_id = channel_id.unwrap();

        let handlers = self.channel_handlers.read().await?;
        if let Some(handler) = handlers.get(&channel_id) {
            t.work(handler).await
        } else {
            Err(Error::ChannelNotFound(self.session_id, channel_id))
        }
    }
}

#[maybe_async(AFIT)]
impl MessageHandler for SessionMessageHandler {
    async fn sendo(&self, msg: OutgoingMessage) -> crate::Result<SendMessageResult> {
        self._with_channel(msg.channel_id, SendoWithChannel(msg))
            .await
    }

    async fn recvo(&self, options: ReceiveOptions<'_>) -> crate::Result<IncomingMessage> {
        self._with_channel(options.channel_id, RecvoWithChannel(options))
            .await
    }
}

#[maybe_async(AFIT)]
trait WithChannel {
    type Result;
    async fn work(
        self,
        href: &HandlerReference<ChannelMessageHandler>,
    ) -> crate::Result<Self::Result>;
}

struct SendoWithChannel(OutgoingMessage);
#[maybe_async(AFIT)]
impl WithChannel for SendoWithChannel {
    type Result = SendMessageResult;
    async fn work(
        self,
        href: &HandlerReference<ChannelMessageHandler>,
    ) -> crate::Result<Self::Result> {
        href.sendo(self.0).await
    }
}

struct RecvoWithChannel<'a>(ReceiveOptions<'a>);
#[maybe_async(AFIT)]
impl WithChannel for RecvoWithChannel<'_> {
    type Result = IncomingMessage;
    async fn work(
        self,
        href: &HandlerReference<ChannelMessageHandler>,
    ) -> crate::Result<Self::Result> {
        href.recvo(self.0).await
    }
}

#[cfg(not(feature = "async"))]
impl Drop for SessionMessageHandler {
    fn drop(&mut self) {
        self.logoff().unwrap_or_else(|e| {
            log::error!("Failed to logoff: {e}",);
        });
    }
}

#[cfg(feature = "async")]
impl Drop for SessionMessageHandler {
    fn drop(&mut self) {
        if self
            .dropping
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }

        let session_id = self.session_id;
        let primary_channel_id = self.primary_channel_id;
        let primary_channel = self.primary_channel.clone();

        tokio::task::spawn(async move {
            let temp_handler = SessionMessageHandler {
                session_id,
                dropping: AtomicBool::new(false),
                primary_channel_id,
                primary_channel,
                channel_handlers: Default::default(),
            };
            temp_handler.logoff_async().await;
        });
    }
}
