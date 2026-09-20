use crate::{error::*, sync_helpers::*};
use smb_transport::{IoVec, SmbTransport, SmbTransportRead, SmbTransportWrite, TransportError};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::{Error, msg_handler::IncomingMessage};

use super::{backend_trait::MultiWorkerBackend, base::ParallelWorker};

#[derive(Debug)]
pub struct ThreadingBackend {
    worker: Arc<ParallelWorker<Self>>,

    /// The loops' handles for the worker.
    loop_handles: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
    stopped: AtomicBool,
}

impl ThreadingBackend {
    fn is_cancelled(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl ThreadingBackend {
    const READ_POLL_TIMEOUT: Duration = Duration::from_millis(100);

    fn loop_receive(&self, mut rtransport: Box<dyn SmbTransportRead>) {
        while !self.is_cancelled() {
            let next = rtransport.receive();
            // Handle polling fail
            if let Err(TransportError::IoError(ref e)) = next
                && e.kind() == std::io::ErrorKind::WouldBlock
            {
                continue;
            }
            match self.worker.incoming_data_callback(next) {
                Ok(_) => {}
                Err(Error::TransportError(TransportError::NotConnected)) => {
                    log::error!("Connection closed.");
                    self.stopped
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                Err(Error::ConnectionStopped) => {
                    break;
                }
                Err(e) => {
                    log::error!("Error in worker recv loop: {e}");
                }
            }
        }
        log::debug!("Receive loop finished. Cleaning up.");
        if let Ok(mut state) = self.worker.state.lock() {
            for (_, tx) in state.awaiting.drain() {
                let _notify_result = tx.send(Err(Error::ConnectionStopped));
            }
        }
    }

    fn loop_send(
        &self,
        mut wtransport: Box<dyn SmbTransportWrite>,
        send_channel: mpsc::Receiver<Option<IoVec>>,
    ) {
        loop {
            match self.loop_send_next(send_channel.recv(), wtransport.as_mut()) {
                Ok(_) => {}
                Err(Error::TransportError(TransportError::NotConnected)) => {
                    log::error!("Connection closed.");
                    self.stopped
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                Err(Error::ConnectionStopped) => {
                    break;
                }
                Err(e) => {
                    log::error!("Error in worker send loop: {e}");
                }
            }
        }
        log::debug!("Send loop finished.");
    }

    #[inline]
    fn loop_send_next(
        &self,
        message: Result<Option<IoVec>, mpsc::RecvError>,
        wtransport: &mut dyn SmbTransportWrite,
    ) -> crate::Result<()> {
        self.worker.outgoing_data_callback(message?, wtransport)
    }
}

impl MultiWorkerBackend for ThreadingBackend {
    type SendMessage = Option<IoVec>;

    type AwaitingNotifier = std::sync::mpsc::Sender<crate::Result<IncomingMessage>>;
    type AwaitingWaiter = std::sync::mpsc::Receiver<crate::Result<IncomingMessage>>;

    fn start(
        transport: Box<dyn SmbTransport>,
        worker: Arc<ParallelWorker<Self>>,
        send_channel_recv: mpsc::Receiver<Self::SendMessage>,
    ) -> crate::Result<Arc<Self>>
    where
        Self: std::fmt::Debug,
        Self::AwaitingNotifier: std::fmt::Debug,
    {
        let backend = Arc::new(Self {
            worker,
            loop_handles: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });

        // Start the worker loops - send and receive.
        let backend_receive = backend.clone();
        let (rtransport, wtransport) = transport.split()?;
        let backend_send = backend.clone();

        rtransport.set_read_timeout(Self::READ_POLL_TIMEOUT)?;

        let handle1 = std::thread::spawn(move || backend_receive.loop_receive(rtransport));
        let handle2 =
            std::thread::spawn(move || backend_send.loop_send(wtransport, send_channel_recv));

        backend
            .loop_handles
            .lock()
            .unwrap()
            .replace((handle1, handle2));

        Ok(backend)
    }

    fn stop(&self) -> crate::Result<()> {
        log::debug!("Stopping worker.");

        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let handles = self
            .loop_handles
            .lock()
            .unwrap()
            .take()
            .ok_or(Error::ConnectionStopped)?;

        // wake up the sender to stop the loop.
        self.worker.sender.send(None).unwrap();

        // Join the threads.
        handles
            .0
            .join()
            .map_err(|_| Error::JoinError("Error stopping receivedr".to_string()))?;

        handles
            .1
            .join()
            .map_err(|_| Error::JoinError("Error stopping sender".to_string()))?;

        Ok(())
    }

    fn wrap_msg_to_send(msg: IoVec) -> Self::SendMessage {
        Some(msg)
    }

    fn make_notifier_awaiter_pair() -> (Self::AwaitingNotifier, Self::AwaitingWaiter) {
        std::sync::mpsc::channel()
    }

    fn wait_on_waiter(
        waiter: Self::AwaitingWaiter,
        timeout: Duration,
    ) -> crate::Result<IncomingMessage> {
        if timeout == Duration::ZERO {
            return waiter.recv().map_err(|_| {
                Error::MessageProcessingError("Failed to receive message.".to_string())
            })?;
        }

        waiter.recv_timeout(timeout).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => {
                Error::OperationTimeout(TimedOutTask::ReceiveNextMessage, timeout)
            }
            _ => Error::MessageProcessingError("Failed to receive message.".to_string()),
        })?
    }

    fn send_notify(
        tx: Self::AwaitingNotifier,
        msg: crate::Result<IncomingMessage>,
    ) -> crate::Result<()> {
        tx.send(msg).map_err(|_| {
            Error::MessageProcessingError("Failed to send message to awaiting task.".to_string())
        })
    }

    fn make_send_channel_pair() -> (
        mpsc::Sender<Self::SendMessage>,
        mpsc::Receiver<Self::SendMessage>,
    ) {
        mpsc::channel()
    }
}
