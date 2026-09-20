use crate::{
    connection::transformer::Transformer,
    error::*,
    msg_handler::{IncomingMessage, OutgoingMessage, ReceiveOptions, SendMessageResult},
};
use smb_transport::{SmbTransport, TransportError};
use std::sync::OnceLock;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use super::Worker;

/// Single-threaded worker.
pub struct SingleWorker {
    // for trait compatibility, we need to use RefCell here,
    // since we can't have mutable references to the same object in multiple threads,
    // which is useful in the async worker.
    transport: Mutex<OnceLock<Box<dyn SmbTransport>>>,
    transformer: Transformer,
    timeout: Mutex<Option<Duration>>,
}

impl Worker for SingleWorker {
    fn start(transport: Box<dyn SmbTransport>, timeout: Duration) -> crate::Result<Arc<Self>> {
        transport.set_read_timeout(timeout)?;
        Ok(Arc::new(Self {
            transport: Mutex::new(OnceLock::from(transport)),
            transformer: Transformer::default(),
            timeout: Mutex::new(Some(timeout)),
        }))
    }

    fn stop(&self) -> crate::Result<()> {
        self.transport
            .lock()?
            .take()
            .ok_or(crate::Error::ConnectionStopped)?;
        Ok(())
    }

    fn send(&self, msg: OutgoingMessage) -> crate::Result<SendMessageResult> {
        let msg_id = msg.message.header.message_id;
        let return_raw_data = msg.return_raw_data;

        let msg_to_send = self.transformer.transform_outgoing(msg)?;

        let mut t = self.transport.lock()?;
        t.get_mut()
            .ok_or(crate::Error::ConnectionStopped)?
            .send(&msg_to_send)?;

        let raw_msg = if return_raw_data {
            Some(msg_to_send)
        } else {
            None
        };

        Ok(SendMessageResult::new(msg_id, raw_msg))
    }

    fn receive_next(&self, options: &ReceiveOptions<'_>) -> crate::Result<IncomingMessage> {
        // Receive next message
        let mut self_mut = self.transport.lock()?;
        let transport = self_mut.get_mut().ok_or(crate::Error::ConnectionStopped)?;

        if options.timeout.is_some() {
            // TODO: implement receive with timeout in options.
            unimplemented!("Receive with timeout is not supported in SingleWorker");
        }

        let msg = transport.receive().map_err(|e| match e {
            TransportError::IoError(ioe) => {
                if ioe.kind() == std::io::ErrorKind::WouldBlock {
                    Error::OperationTimeout(
                        TimedOutTask::ReceiveNextMessage,
                        self.timeout
                            .lock()
                            .map(|v| v.unwrap_or(Duration::ZERO))
                            .unwrap_or(Duration::MAX),
                    )
                } else {
                    crate::Error::IoError(ioe)
                }
            }
            _ => e.into(),
        })?;
        // Transform the message
        let im = self.transformer.transform_incoming(msg)?;
        // Make sure this is our message.
        // In async clients, this is no issue, but here, we can't deal with unordered/unexpected message IDs.
        if im.message.header.message_id != options.msg_id {
            return Err(crate::Error::UnexpectedMessageId(
                im.message.header.message_id,
                options.msg_id,
            ));
        }
        Ok(im)
    }

    fn transformer(&self) -> &Transformer {
        &self.transformer
    }
}

impl std::fmt::Debug for SingleWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SingleWorker")
            .field("timeout", &self.timeout)
            .finish()
    }
}
