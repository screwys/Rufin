use std::io::{self, Write};
use std::sync::mpsc;
use std::time::Duration;

use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone)]
pub(super) struct Writer {
    sender: mpsc::SyncSender<Option<Vec<u8>>>,
}

/// Drains queued stderr output on drop, with a bounded wait for stalled destinations.
pub struct Guard {
    sender: mpsc::SyncSender<Option<Vec<u8>>>,
    finished: mpsc::Receiver<()>,
}

impl Writer {
    pub(super) fn new(mut output: impl Write + Send + 'static) -> (Self, Guard) {
        let (sender, receiver) = mpsc::sync_channel::<Option<Vec<u8>>>(64);
        let (finished_sender, finished) = mpsc::channel();
        std::thread::Builder::new()
            .name("diagnostic-stderr".into())
            .spawn(move || {
                while let Ok(Some(record)) = receiver.recv() {
                    if output.write_all(&record).is_err() {
                        break;
                    }
                }
                let _ = output.flush();
                let _ = finished_sender.send(());
            })
            .expect("start diagnostic stderr writer");
        (
            Self {
                sender: sender.clone(),
            },
            Guard { sender, finished },
        )
    }

    pub(super) fn enqueue(&self, record: Vec<u8>) {
        // A stalled terminal or redirected pipe must not stall the caller. Only
        // the stderr copy is lossy; the separate local log still records events.
        let _ = self.sender.try_send(Some(record));
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Drain normal output, but never wait indefinitely or join a worker that
        // may be stuck in an OS write. None follows all previously queued records.
        if self.sender.try_send(None).is_ok() {
            let _ = self.finished.recv_timeout(Duration::from_millis(100));
        }
    }
}

pub(super) struct Record<'a> {
    writer: &'a Writer,
    bytes: Vec<u8>,
}

impl<'a> MakeWriter<'a> for Writer {
    type Writer = Record<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        Record {
            writer: self,
            bytes: Vec::new(),
        }
    }
}

impl Write for Record<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Record<'_> {
    fn drop(&mut self) {
        if !self.bytes.is_empty() {
            self.writer.enqueue(std::mem::take(&mut self.bytes));
        }
    }
}
