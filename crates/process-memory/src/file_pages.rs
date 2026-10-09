use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::time::{Duration, Instant};
struct FilePages {
    pagemap: std::fs::File,
    page_size: usize,
    states: [u8; 65536],
    advised_bytes: usize,
    error: Option<std::io::Error>,
    scan_time: Duration,
    discard_time: Duration,
}
/// Returns clean pages from loaded executables and libraries. Used pages reload from their files.
pub(crate) fn reclaim() -> std::io::Result<usize> {
    #[allow(unsafe_code)]
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mut pages = FilePages {
        pagemap: std::fs::File::open("/proc/self/pagemap")?,
        page_size,
        states: [0; 65536],
        advised_bytes: 0,
        error: None,
        scan_time: Duration::ZERO,
        discard_time: Duration::ZERO,
    };
    // SAFETY: the callback borrows pages during this synchronous iteration under the loader lock.
    #[allow(unsafe_code)]
    unsafe {
        libc::dl_iterate_phdr(Some(reclaim_object), std::ptr::from_mut(&mut pages).cast());
    }
    tracing::debug!(target:"rufin::memory",pid=std::process::id(),scan_us=pages.scan_time.as_micros() as u64,
        discard_us=pages.discard_time.as_micros() as u64,"file page reclamation costs");
    match pages.error {
        Some(error) => Err(error),
        None => Ok(pages.advised_bytes),
    }
}
impl FilePages {
    fn clean_ranges(&mut self, start: usize, end: usize) -> std::io::Result<Vec<Range<usize>>> {
        // Pagemap has one native-endian u64 per virtual page. Only the state flags are needed.
        self.pagemap
            .seek(SeekFrom::Start((start / self.page_size) as u64 * 8))?;
        let mut remaining = (end - start) / self.page_size;
        let mut address = start;
        let mut clean_start = Some(start);
        let mut ranges = Vec::new();
        while remaining != 0 {
            let count = remaining.min(self.states.len() / 8);
            self.pagemap.read_exact(&mut self.states[..count * 8])?;
            for bytes in self.states[..count * 8].chunks_exact(8) {
                let state = u64::from_ne_bytes(bytes.try_into().expect("one pagemap entry"));
                let present = state & (1u64 << 63) != 0;
                let swapped = state & (1u64 << 62) != 0;
                let file_backed = state & (1u64 << 61) != 0;
                if swapped || present && !file_backed {
                    if let Some(start) = clean_start.take()
                        && start < address
                    {
                        ranges.push(start..address);
                    }
                } else if clean_start.is_none() {
                    clean_start = Some(address);
                }
                address += self.page_size;
            }
            remaining -= count;
        }
        if let Some(start) = clean_start
            && start < end
        {
            ranges.push(start..end);
        }
        Ok(ranges)
    }
}
#[allow(unsafe_code)]
unsafe extern "C" fn reclaim_object(
    info: *mut libc::dl_phdr_info,
    _size: usize,
    data: *mut libc::c_void,
) -> libc::c_int {
    // SAFETY: glibc provides the live object and headers. pages belongs to the synchronous caller.
    let (info, pages, headers) = unsafe {
        let info = &*info;
        (
            info,
            &mut *data.cast::<FilePages>(),
            std::slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)),
        )
    };
    for header in headers {
        if header.p_type != libc::PT_LOAD
            || header.p_flags & libc::PF_W != 0
            || header.p_filesz == 0
        {
            continue;
        }
        let start = info.dlpi_addr as usize + header.p_vaddr as usize;
        let end = (start + header.p_filesz as usize).div_ceil(pages.page_size) * pages.page_size;
        let start = start / pages.page_size * pages.page_size;
        let started = Instant::now();
        let clean = pages.clean_ranges(start, end);
        pages.scan_time += started.elapsed();
        let ranges = match clean {
            Err(error) => {
                pages.error = Some(error);
                return 1;
            }
            Ok(ranges) => ranges,
        };
        for range in ranges {
            let started = Instant::now();
            // SAFETY: the loader lock keeps this originally read-only file segment mapped.
            // Pagemap excludes swapped and private anonymous pages, including modified code.
            // Writable and relocated segments were excluded using their original ELF flags.
            let result = unsafe {
                libc::madvise(
                    range.start as *mut libc::c_void,
                    range.len(),
                    libc::MADV_DONTNEED,
                )
            };
            pages.discard_time += started.elapsed();
            if result == 0 {
                pages.advised_bytes += range.len();
            }
        }
    }
    0
}
