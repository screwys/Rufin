use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

const COALESCE: Duration = Duration::from_secs(1);
const RECLAIM_INTERVAL: Duration = Duration::from_secs(10);
const FILE_RECLAIM_INTERVAL: Duration = Duration::from_secs(60);

pub fn configure_allocator() -> bool {
    let arenas = std::thread::available_parallelism()
        .map(|count| count.get().saturating_mul(2))
        .unwrap_or(2)
        .min(libc::c_int::MAX as usize) as libc::c_int;
    let mut configured = true;
    // Fixed thresholds prevent large temporary buffers from raising them process-wide.
    for (option, value) in [
        (libc::M_ARENA_MAX, arenas),
        (libc::M_MMAP_THRESHOLD, 2 * 1024 * 1024),
        (libc::M_TRIM_THRESHOLD, 4 * 1024 * 1024),
        (libc::M_TOP_PAD, 128 * 1024),
    ] {
        // SAFETY: Both executables call this before starting worker threads.
        #[allow(unsafe_code)]
        let result = unsafe { libc::mallopt(option, value) };
        configured &= result != 0;
    }
    let _ = requests();
    configured
}

/// Requests reclamation after a batch has released its temporary allocations.
pub fn request_reclaim() {
    if let Some(sender) = requests() {
        // One pending request covers all releases until the next trim.
        let _ = sender.try_send(());
    }
}

fn requests() -> Option<&'static mpsc::SyncSender<()>> {
    static REQUESTS: OnceLock<Option<mpsc::SyncSender<()>>> = OnceLock::new();
    REQUESTS.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("memory-reclaim".into())
            .spawn(move || reclaim(receiver))
        {
            Ok(_) => Some(sender),
            Err(error) => {
                tracing::warn!(target: "rufin::memory", %error, "could not start memory reclamation");
                None
            }
        }
    }).as_ref()
}

fn reclaim(requests: mpsc::Receiver<()>) {
    let mut last_trim: Option<Instant> = None;
    let mut file_deadline = Instant::now() + FILE_RECLAIM_INTERVAL;
    let mut startup_pending = true;
    loop {
        let request = if startup_pending {
            requests.recv_timeout(file_deadline.saturating_duration_since(Instant::now()))
        } else {
            requests
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        if matches!(request, Err(mpsc::RecvTimeoutError::Disconnected)) {
            break;
        }
        if request.is_ok() {
            let now = Instant::now();
            let deadline = last_trim
                .map(|last| (last + RECLAIM_INTERVAL).max(now + COALESCE))
                .unwrap_or(now + COALESCE);
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            let _ = requests.try_recv();
            let before = if tracing::enabled!(target: "rufin::memory", tracing::Level::DEBUG) {
                // SAFETY: mallinfo2 reads glibc's allocator counters under its arena locks.
                #[allow(unsafe_code)]
                Some(unsafe { libc::mallinfo2() })
            } else {
                None
            };
            let started = Instant::now();
            // SAFETY: glibc serializes malloc_trim with allocations in each arena.
            #[allow(unsafe_code)]
            let released = unsafe { libc::malloc_trim(0) } != 0;
            last_trim = Some(Instant::now());
            if let Some(before) = before {
                let elapsed_us = started.elapsed().as_micros() as u64;
                // SAFETY: as above; the snapshot describes allocator capacity, not residency.
                #[allow(unsafe_code)]
                let after = unsafe { libc::mallinfo2() };
                tracing::debug!(
                    target: "rufin::memory",
                    pid = std::process::id(),
                    released,
                    elapsed_us,
                    arena_bytes = after.arena,
                    arena_allocated_bytes = after.uordblks,
                    arena_free_bytes = after.fordblks,
                    mapped_bytes = after.hblkhd,
                    malloc_provider = malloc_provider(),
                    previous_arena_bytes = before.arena,
                    previous_arena_free_bytes = before.fordblks,
                    "allocator reclamation finished"
                );
            }
        }
        if Instant::now() >= file_deadline {
            startup_pending = false;
            file_deadline = Instant::now() + FILE_RECLAIM_INTERVAL;
            let started = Instant::now();
            match crate::file_pages::reclaim() {
                Ok(advised_bytes) => tracing::debug!(
                    target: "rufin::memory",
                    pid = std::process::id(),
                    advised_bytes,
                    elapsed_us = started.elapsed().as_micros() as u64,
                    "file page reclamation finished"
                ),
                Err(error) => {
                    tracing::debug!(target: "rufin::memory", %error, "could not reclaim file pages")
                }
            }
        }
    }
}

fn malloc_provider() -> &'static str {
    static LIBRARY: OnceLock<String> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        // SAFETY: dladdr fills this record for the linked malloc function. Its allocator
        // library remains loaded while the process uses that function.
        #[allow(unsafe_code)]
        unsafe {
            let mut info: libc::Dl_info = std::mem::zeroed();
            if libc::dladdr(libc::malloc as *const libc::c_void, &mut info) == 0
                || info.dli_fname.is_null()
            {
                return "unknown".into();
            }
            std::ffi::CStr::from_ptr(info.dli_fname)
                .to_string_lossy()
                .rsplit('/')
                .next()
                .unwrap_or("unknown")
                .to_owned()
        }
    })
}
