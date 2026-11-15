//! Windows folder listings. One call returns every entry's size, times, attributes and file
//! ID, so a walk opens no file it does not read, and a moved file keeps its identity.

#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io;
use std::mem::offset_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FULL_DIR_INFO, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FileFullDirectoryInfo,
    FileIdBothDirectoryInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    GetVolumeInformationByHandleW,
};

use crate::file::walk::Entry;

/// The reparse tags of links: symbolic links and junctions.
const NAME_SURROGATE: u32 = 0x2000_0000;
const FILE_SUPPORTS_OPEN_BY_FILE_ID: u32 = 0x0100_0000;
/// 1601 to 1970, in the 100 ns units of a file time.
const UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// A listing buffer, in 8-byte words: entries are aligned to them.
const BUFFER_WORDS: usize = 8192;

/// One entry as a folder lists it.
struct Listed<'a> {
    name: &'a [u16],
    attributes: u32,
    /// The reparse tag of a reparse point.
    tag: u32,
    size: i64,
    written: i64,
    created: i64,
    id: i64,
}

fn open(path: &Path, list: bool) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(if list { FILE_LIST_DIRECTORY.0 } else { 0 })
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn facts(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut facts = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is open and `facts` is a valid place for the result.
    unsafe { GetFileInformationByHandle(handle(file), &mut facts) }?;
    Ok(facts)
}

/// Whether the file's volume keeps file IDs. FAT and some shares do not.
fn identified(file: &File) -> bool {
    let mut flags = 0;
    // SAFETY: the handle is open and `flags` is a valid place for the result.
    unsafe { GetVolumeInformationByHandleW(handle(file), None, None, None, Some(&mut flags), None) }
        .is_ok()
        && flags & FILE_SUPPORTS_OPEN_BY_FILE_ID != 0
}

fn unix_ns(time: i64) -> Option<i64> {
    time.checked_sub(UNIX_EPOCH)
        .filter(|time| *time >= 0)?
        .checked_mul(100)
}

fn entry(
    name: String,
    directory: bool,
    size: i64,
    written: i64,
    created: i64,
    identity: Option<(u32, i64)>,
) -> Entry {
    // An ID of zero or all ones is none.
    let identity = identity.filter(|(_, id)| !matches!(id, 0 | -1));
    Entry {
        native: name.clone(),
        name,
        directory,
        size: (!directory).then_some(size),
        mtime_ns: unix_ns(written).unwrap_or_default(),
        device_id: identity.map(|(volume, _)| i64::from(volume)),
        inode: identity.map(|(_, id)| id),
        birth_ns: unix_ns(created),
        native_id: None,
        revision: None,
    }
}

fn file_time(time: windows::Win32::Foundation::FILETIME) -> i64 {
    (i64::from(time.dwHighDateTime) << 32) | i64::from(time.dwLowDateTime)
}

/// The facts of one path, a link's being those of what it leads to.
pub(crate) fn stat(path: &Path) -> io::Result<Entry> {
    let file = open(path, false)?;
    let facts = facts(&file)?;
    let id = (i64::from(facts.nFileIndexHigh) << 32) | i64::from(facts.nFileIndexLow);
    Ok(entry(
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        facts.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
        (i64::from(facts.nFileSizeHigh) << 32) | i64::from(facts.nFileSizeLow),
        file_time(facts.ftLastWriteTime),
        file_time(facts.ftCreationTime),
        identified(&file).then_some((facts.dwVolumeSerialNumber, id)),
    ))
}

/// The entries the system wrote into `buffer`, as one of its two listing layouts.
macro_rules! listed {
    ($buffer:expr, $layout:ty, |$fixed:ident| $id:expr) => {{
        let start = $buffer.as_ptr().cast::<u8>();
        let mut offset = Some(0_usize);
        std::iter::from_fn(move || {
            let at = offset?;
            // SAFETY: the system filled the buffer with a chain of entries, each at the offset
            // the one before it gives and followed by its name, all inside the buffer.
            let ($fixed, name) = unsafe {
                let entry = start.add(at);
                let fixed = entry.cast::<$layout>().read_unaligned();
                let name = std::slice::from_raw_parts(
                    entry.add(offset_of!($layout, FileName)).cast::<u16>(),
                    fixed.FileNameLength as usize / 2,
                );
                (fixed, name)
            };
            offset = ($fixed.NextEntryOffset != 0).then(|| at + $fixed.NextEntryOffset as usize);
            Some(Listed {
                name,
                attributes: $fixed.FileAttributes,
                tag: $fixed.EaSize,
                size: $fixed.EndOfFile,
                written: $fixed.LastWriteTime,
                created: $fixed.CreationTime,
                id: $id,
            })
        })
    }};
}

/// The entries of a folder, with the links `follow` accepts as what they lead to.
pub(super) fn list(folder: &Path, follow: &dyn Fn(&Path) -> bool) -> io::Result<Vec<Entry>> {
    let opened = open(folder, true)?;
    let volume = identified(&opened)
        .then(|| facts(&opened))
        .transpose()?
        .map(|facts| facts.dwVolumeSerialNumber);
    let class = if volume.is_some() {
        FileIdBothDirectoryInfo
    } else {
        FileFullDirectoryInfo
    };
    let mut buffer = vec![0_u64; BUFFER_WORDS];
    let mut entries = Vec::new();
    let mut links = Vec::new();
    loop {
        // SAFETY: the handle is open and the buffer is writable for the size given.
        let read = unsafe {
            GetFileInformationByHandleEx(
                handle(&opened),
                class,
                buffer.as_mut_ptr().cast(),
                (BUFFER_WORDS * 8) as u32,
            )
        };
        match read {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(error.into()),
        }
        let listed: Box<dyn Iterator<Item = Listed<'_>>> = if volume.is_some() {
            Box::new(listed!(buffer, FILE_ID_BOTH_DIR_INFO, |fixed| fixed.FileId))
        } else {
            Box::new(listed!(buffer, FILE_FULL_DIR_INFO, |fixed| 0))
        };
        for listed in listed {
            // A name that is not UTF-8 cannot be stored as a path the source opens.
            let Ok(name) = String::from_utf16(listed.name) else {
                continue;
            };
            // What Windows keeps for itself: recycle bins, volume data, and the junctions it
            // leaves for old programs, which lead into other folders of the user.
            let system = FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0;
            if name == "." || name == ".." || listed.attributes & system == system {
                continue;
            }
            if listed.attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
                && listed.tag & NAME_SURROGATE != 0
            {
                links.push(name);
                continue;
            }
            let directory = listed.attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
            // A folder's times come from the folder itself: its parent lists them late.
            let own = directory.then(|| stat(&folder.join(&name)).ok()).flatten();
            entries.push(match own {
                Some(own) => Entry {
                    native: name.clone(),
                    name,
                    ..own
                },
                None => entry(
                    name,
                    directory,
                    listed.size,
                    listed.written,
                    listed.created,
                    volume.map(|volume| (volume, listed.id)),
                ),
            });
        }
    }
    // In name order, so of two links to one place the same one is followed every time. A link
    // that leads nowhere is left out.
    links.sort();
    for name in links {
        let path = folder.join(&name);
        if follow(&path)
            && let Ok(own) = stat(&path)
        {
            entries.push(Entry {
                native: name.clone(),
                name,
                ..own
            });
        }
    }
    Ok(entries)
}
