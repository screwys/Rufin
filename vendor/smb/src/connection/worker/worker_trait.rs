use std::{sync::Arc, time::Duration};

use crate::{
    Error, connection::connection_info::ConnectionInfo, msg_handler::ReceiveOptions,
    session::SessionAndChannel, sync_helpers::*,
};
use smb_transport::SmbTransport;

use maybe_async::*;
use smb_msg::Status;

use crate::{
    connection::transformer::Transformer,
    msg_handler::{IncomingMessage, OutgoingMessage, SendMessageResult},
};

/// SMB2 connection worker.
///
/// Each Implementation of this trait is responsible for handling the connection to the server,
/// sending messages, and redirecting correct messages when received,
/// if using async, to the correct pending task.
#[maybe_async(AFIT)]
#[allow(async_fn_in_trait)]
pub trait Worker: Sized + std::fmt::Debug {
    /// Instantiates a new connection worker.
    async fn start(transport: Box<dyn SmbTransport>, timeout: Duration)
    -> crate::Result<Arc<Self>>;
    /// Stops the worker, shutting down the connection.
    async fn stop(&self) -> crate::Result<()>;

    async fn send(&self, msg: OutgoingMessage) -> crate::Result<SendMessageResult>;

    /// (Internal)
    ///
    /// This function is implemented to receive a single message from the server,
    /// with the specified filters.
    /// Use [`Worker::receive`] instead, if you're a user of this trait, and not an implementor.
    /// # Arguments
    /// * `msg_id` - The message ID to receive. This function will not return until the message id specified
    /// is received.
    /// # Returns
    /// * The message received from the server, matching the filters.
    async fn receive_next(&self, options: &ReceiveOptions<'_>) -> crate::Result<IncomingMessage>;

    #[cfg(feature = "async")]
    async fn receive_next_cancellable(
        &self,
        options: &ReceiveOptions<'_>,
    ) -> crate::Result<IncomingMessage> {
        if options.async_cancel.is_none() {
            return self.receive_next(options).await;
        }
        let recv_fut = self.receive_next(options);
        tokio::select! {
            biased;
            _ = options.async_cancel.as_ref().unwrap().cancelled() => {
                Err(Error::Cancelled("receive_next"))
            }
            res = recv_fut => {
                res
            }
        }
    }

    #[cfg(not(feature = "async"))]
    async fn receive_next_cancellable(
        &self,
        options: &ReceiveOptions<'_>,
    ) -> crate::Result<IncomingMessage> {
        // There's no actual async cancellation, so we do our best effort.
        // If the request is already running, cancellation must be performed by sending a
        // cancel message to the server.
        if options
            .async_cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(Error::Cancelled("receive_next"));
        }

        self.receive_next(options).await
    }

    /// Receive a message from the server.
    /// This is a user function that will wait for the message to be received.
    async fn receive(&self, options: &ReceiveOptions<'_>) -> crate::Result<IncomingMessage> {
        if options.msg_id == u64::MAX {
            return Err(Error::InvalidArgument(
                "Message ID -1 is not valid for receive()".to_string(),
            ));
        }

        let curr = self.receive_next(options).await?;

        // Not async -- return the result.
        if !curr.message.header.flags.async_command() {
            return Ok(curr);
        }

        // Handle async.
        if !options.allow_async {
            return Err(Error::InvalidArgument(
                "Async command is not allowed in this context.".to_string(),
            ));
        }

        // If not pending, that's the result, right away!
        if curr.message.header.status != Status::Pending as u32 {
            return Ok(curr);
        }

        log::debug!(
            "Received async pending message with ID {} and status {}.",
            curr.message.header.message_id,
            curr.message.header.status
        );

        let async_id = match curr.message.header.async_id {
            Some(async_id) => async_id,
            None => panic!("Async ID is None, but async command is set. This should not happen."),
        };

        if async_id == 0 {
            return Ok(curr);
        }

        if let Some(async_msg_ids) = &options.async_msg_ids {
            async_msg_ids.set(options.msg_id, async_id);
        }

        loop {
            let msg = self.receive_next_cancellable(options).await?;

            // Check if the message is async and has the same ID.
            if !msg.message.header.flags.async_command()
                || msg.message.header.async_id != Some(async_id)
            {
                return Err(Error::InvalidArgument(format!(
                    "Received message for msgid {} with async ID {} but expected async ID {}",
                    msg.message.header.message_id,
                    msg.message
                        .header
                        .async_id
                        .map(|x| x.to_string())
                        .unwrap_or("None".to_string()),
                    async_id
                )));
            }

            // We've got a result!
            if msg.message.header.status != Status::Pending as u32 {
                return Ok(msg);
            }

            log::debug!(
                "Received another async pending message with ID {} and status {}.",
                msg.message.header.message_id,
                msg.message.header.status
            );
        }
    }

    /// Get the transformer for this worker.
    fn transformer(&self) -> &Transformer;

    #[maybe_async]
    async fn negotaite_complete(&self, neg: &ConnectionInfo) {
        self.transformer().negotiated(neg).await.unwrap();
    }

    #[maybe_async]
    async fn session_started(&self, info: &Arc<RwLock<SessionAndChannel>>) -> crate::Result<()> {
        self.transformer().session_started(info).await
    }

    #[maybe_async]
    async fn session_ended(&self, info: &Arc<RwLock<SessionAndChannel>>) -> crate::Result<()> {
        self.transformer().session_ended(info).await
    }
}
