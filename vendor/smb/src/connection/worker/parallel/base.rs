use crate::connection::transformer::Transformer;
use crate::connection::worker::Worker;
use crate::msg_handler::ReceiveOptions;
use crate::sync_helpers::*;
use maybe_async::*;
use smb_msg::ResponseContent;
use smb_transport::{IoVec, SmbTransport, SmbTransportWrite, TransportError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};

use crate::{
    Error,
    msg_handler::{IncomingMessage, OutgoingMessage, SendMessageResult},
};

use super::backend_trait::MultiWorkerBackend;

/// SMB2 base parallel transport worker (multi-threaded or async).
///
/// This struct is responsible for handling the transport to the server,
/// sending messages from SMB2 messages, and redirecting correct messages when received,
/// if using async, to the correct pending task.
/// One-per transport connection, hence takes ownership of the [SmbTransport] on [ParallelWorker::start].
pub struct ParallelWorker<BackendImplT>
where
    BackendImplT: MultiWorkerBackend + std::fmt::Debug,
    BackendImplT::AwaitingNotifier: std::fmt::Debug,
{
    /// The state of the worker, regarding messages to be received.
    /// See [`WorkerAwaitState`] for more details.
    pub(crate) state: Mutex<WorkerAwaitState<BackendImplT>>,

    /// The backend implementation of the worker -
    /// multi-threaded or async, depending on the crate configuration.
    backend_impl: Mutex<Option<Arc<BackendImplT>>>,

    transformer: Transformer,

    /// A channel that is being used to pass on messages that are being received from the server with
    /// no associated message ID (message id -1 - oplock break/server to client notification).
    notify_messages_channel: OnceCell<mpsc::Sender<IncomingMessage>>,

    /// The channel that is being used to pass on messages that are being sent to the server,
    /// from user threads to the worker send thread.
    pub(crate) sender: mpsc::Sender<BackendImplT::SendMessage>,

    /// A flag that indicates whether the worker is stopped.
    stopped: AtomicBool,
    /// The current timeout configured for the worker.
    timeout: AtomicU64,
}

/// Holds state for the worker, regarding messages to be received:
/// - awaiting: tasks that are waiting for a specific message ID.
/// - pending: messages that are waiting to be receive()-d.
#[derive(Debug)]
pub struct WorkerAwaitState<T>
where
    T: MultiWorkerBackend,
    T::AwaitingNotifier: std::fmt::Debug,
{
    /// Stores the awaiting tasks that are waiting for a specific message ID.
    pub awaiting: HashMap<u64, T::AwaitingNotifier>,
    /// Stores the pending messages, waiting to be receive()-d.
    pub pending: HashMap<u64, crate::Result<IncomingMessage>>,
}

impl<T> WorkerAwaitState<T>
where
    T: MultiWorkerBackend,
    T::AwaitingNotifier: std::fmt::Debug,
{
    fn new() -> Self {
        Self {
            awaiting: HashMap::new(),
            pending: HashMap::new(),
        }
    }
}

#[maybe_async(AFIT)]
impl<T> ParallelWorker<T>
where
    T: MultiWorkerBackend + std::fmt::Debug,
    T::AwaitingNotifier: std::fmt::Debug,
{
    /// Returns whether the worker is stopped.
    pub fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// This is a function that should be used by multi worker implementations (async/mtd),
    /// after gettting a messages from the server, this function processes it and
    /// notifies the awaiting tasks.
    pub(crate) async fn incoming_data_callback(
        self: &Arc<Self>,
        message: Result<Vec<u8>, TransportError>,
    ) -> crate::Result<()> {
        log::trace!("Received message from server.");
        let message = message?;

        // Tranform the message and verify it.
        let msg = self.transformer.transform_incoming(message).await;
        let (msg, msg_id) = match msg {
            // Good flow, message is OK.
            Ok(msg) => {
                let msg_id = msg.message.header.message_id;
                (Ok(msg), msg_id)
            }
            // If we have a message ID to notify the error, use it.
            Err(crate::Error::TranformFailed(e)) => match e.msg_id {
                Some(msg_id) => (Err(crate::Error::TranformFailed(e)), msg_id),
                None => return Err(Error::TranformFailed(e)),
            },
            Err(e) => {
                log::error!("Failed to transform message: {e:?}");
                return Err(e);
            }
        };

        // Message ID (-1) is used and valid for notifications -
        // OPLOCK_BREAK or SERVER_TO_CLIENT_NOTIFICATION only.
        if msg_id == u64::MAX {
            // Nothing's waiting, so if there was an error, return it --
            // no need to notify anyone else.
            let msg = msg?;

            // Server-to-client commands check.
            // allow only oplock break and server to client notification.
            if !matches!(
                msg.message.content,
                ResponseContent::OplockBreakNotify(_)
                    | ResponseContent::ServerToClientNotification(_)
            ) {
                return Err(Error::MessageProcessingError(
                    "Received notification message, but not an OPLOCK_BREAK or SERVER_TO_CLIENT_NOTIFICATION.".to_string(),
                ));
            }

            if let Some(s2c_channel) = self.notify_messages_channel.get() {
                log::trace!("Sending notification message to notify channel.");
                s2c_channel.send(msg).await.map_err(|_| {
                    Error::MessageProcessingError(
                        "Failed to send notification message to notify channel.".to_string(),
                    )
                })?;
            } else {
                log::warn!("Received notification message, but no notify channel is set.");
            }
            return Ok(());
        }

        // Update the state: If awaited, wake up the task. Else, store it.
        let mut state = self.state.lock().await?;
        let message_waiter = state.awaiting.remove(&msg_id);
        match message_waiter {
            Some(tx) => {
                log::trace!("Waking up awaiting task for key {msg_id}.");
                T::send_notify(tx, msg)?;
            }
            None => {
                log::trace!("Storing message until awaited: {msg_id}.",);
                state.pending.insert(msg_id, msg);
            }
        }
        Ok(())
    }

    /// This function is used to set the notify channel for the worker.
    pub fn start_notify_channel(
        self: &Arc<Self>,
        notify_channel: mpsc::Sender<IncomingMessage>,
    ) -> crate::Result<()> {
        self.notify_messages_channel
            .set(notify_channel)
            .map_err(|_| Error::InvalidState("Notify channel is already set.".to_string()))?;
        Ok(())
    }

    /// This is a function that should be used by multi worker implementations (async/mtd),
    /// to send a message to the server.
    pub async fn outgoing_data_callback(
        self: &Arc<Self>,
        message: Option<IoVec>,
        wtransport: &mut dyn SmbTransportWrite,
    ) -> crate::Result<()> {
        let message = match message {
            Some(m) => m,
            None => {
                if self.stopped() {
                    return Err(Error::ConnectionStopped);
                } else {
                    return Err(Error::MessageProcessingError(
                        "Empty message cannot be sent to the server.".to_string(),
                    ));
                }
            }
        };
        wtransport.send(&message).await?;

        Ok(())
    }
}

#[maybe_async(AFIT)]
impl<T> Worker for ParallelWorker<T>
where
    T: MultiWorkerBackend + std::fmt::Debug,
    T::AwaitingNotifier: std::fmt::Debug,
{
    async fn start(
        transport: Box<dyn SmbTransport>,
        timeout: Duration,
    ) -> crate::Result<Arc<Self>> {
        // Build the worker
        let (tx, rx) = T::make_send_channel_pair();
        let worker = Arc::new(ParallelWorker::<T> {
            state: Mutex::new(WorkerAwaitState::new()),
            backend_impl: Default::default(),
            transformer: Transformer::default(),
            notify_messages_channel: Default::default(),
            sender: tx,
            stopped: AtomicBool::new(false),
            timeout: AtomicU64::new(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)),
        });

        worker
            .backend_impl
            .lock()
            .await?
            .replace(T::start(transport, worker.clone(), rx).await?);

        Ok(worker)
    }

    async fn stop(&self) -> crate::Result<()> {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        {
            self.backend_impl
                .lock()
                .await?
                .take()
                .ok_or(Error::InvalidState(
                    "No backend present for worker.".to_string(),
                ))?
        }
        .stop()
        .await
    }

    async fn send(&self, msg: OutgoingMessage) -> crate::Result<SendMessageResult> {
        log::trace!("ParallelWorker::send({msg:?}) called");
        let return_raw_data = msg.return_raw_data;

        let id = msg.message.header.message_id;
        let message = { self.transformer.transform_outgoing(msg).await? };

        log::trace!("Message with ID {id} is passed to the worker for sending",);

        let raw_message_copy = if return_raw_data {
            Some(message.clone())
        } else {
            None
        };

        let message = T::wrap_msg_to_send(message);

        self.sender.send(message).await.map_err(|_| {
            Error::MessageProcessingError("Failed to send message to worker!".to_string())
        })?;

        Ok(SendMessageResult::new(id, raw_message_copy))
    }

    async fn receive_next(&self, options: &ReceiveOptions<'_>) -> crate::Result<IncomingMessage> {
        let wait_for_receive = {
            let mut state = self.state.lock().await?;
            if self.stopped() {
                log::trace!("Connection is closed, avoid receiving.");
                return Err(Error::ConnectionStopped);
            }
            if state.pending.contains_key(&options.msg_id) {
                log::trace!(
                    "Message with ID {} is already received, remove from pending.",
                    &options.msg_id
                );
                let data = state.pending.remove(&options.msg_id).ok_or_else(|| {
                    Error::InvalidState("Message ID not found in pending messages.".to_string())
                })?;
                return data;
            }

            log::trace!(
                "Message with ID {} is not received yet, insert channel and await.",
                options.msg_id
            );

            let (tx, rx) = T::make_notifier_awaiter_pair();
            state.awaiting.insert(options.msg_id, tx);
            rx
        };

        let timeout = options
            .timeout
            .unwrap_or_else(|| Duration::from_millis(self.timeout.load(Ordering::Relaxed)));
        let result = T::wait_on_waiter(wait_for_receive, timeout).await?;

        log::trace!("Received message {result:?}");

        Ok(result)
    }

    fn transformer(&self) -> &Transformer {
        &self.transformer
    }
}

impl<T> std::fmt::Debug for ParallelWorker<T>
where
    T: MultiWorkerBackend + std::fmt::Debug,
    T::AwaitingNotifier: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelWorker")
            .field("state", &self.state)
            .field("backend", &self.backend_impl)
            .field("sender", &self.sender)
            .field("stopped", &self.stopped)
            .finish()
    }
}
