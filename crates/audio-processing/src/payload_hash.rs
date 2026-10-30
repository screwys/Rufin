//! A hash of a file's audio payload that survives tag edits where the container allows it.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// A hash of the audio payload of a Local file, tags excluded where the container allows:
/// MPEG (ID3v2 head, ID3v1/APEv2 tail skipped), MP4 (mdat), RIFF/AIFF (data/SSND); otherwise
/// the whole file.
pub fn payload_hash(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut head = Vec::with_capacity(12);
    (&mut file).take(12).read_to_end(&mut head)?;
    let mut ranges = if head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WAVE") {
        chunks(&mut file, length, b"data", u32::from_le_bytes)?
    } else if head.starts_with(b"FORM") && matches!(head.get(8..12), Some(b"AIFF" | b"AIFC")) {
        chunks(&mut file, length, b"SSND", u32::from_be_bytes)?
    } else if head.get(4..8) == Some(b"ftyp") {
        boxes(&mut file, length)?
    } else {
        mpeg(&mut file, length)?.into_iter().collect()
    };
    if ranges.is_empty() {
        ranges.push((0, length));
    }
    let mut hasher = blake3::Hasher::new();
    for (start, end) in ranges {
        file.seek(SeekFrom::Start(start))?;
        io::copy(&mut (&mut file).take(end - start), &mut hasher)?;
    }
    Ok(*hasher.finalize().as_bytes())
}

/// The bodies of the top-level chunks named `id` in a RIFF or IFF file.
fn chunks(
    file: &mut File,
    length: u64,
    id: &[u8; 4],
    size: fn([u8; 4]) -> u32,
) -> io::Result<Vec<(u64, u64)>> {
    let mut ranges = Vec::new();
    let mut header = [0; 8];
    let mut offset = 12;
    while offset + 8 <= length {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header)?;
        let declared = u64::from(size([header[4], header[5], header[6], header[7]]));
        let end = (offset + 8).saturating_add(declared).min(length);
        if &header[..4] == id {
            ranges.push((offset + 8, end));
        }
        offset = end + (declared & 1);
    }
    Ok(ranges)
}

/// The bodies of the top-level `mdat` boxes of an MP4 file.
fn boxes(file: &mut File, length: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut ranges = Vec::new();
    let mut header = [0; 16];
    let mut offset = 0;
    while offset + 8 <= length {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header[..8])?;
        let (size, body) = match u32::from_be_bytes([header[0], header[1], header[2], header[3]]) {
            0 => (length - offset, 8),
            1 if offset + 16 <= length => {
                file.read_exact(&mut header[8..])?;
                (
                    u64::from_be_bytes(header[8..].try_into().expect("8 bytes")),
                    16,
                )
            }
            size => (u64::from(size), 8),
        };
        if size < body {
            break;
        }
        let end = offset.saturating_add(size).min(length);
        if &header[4..8] == b"mdat" {
            ranges.push(((offset + body).min(end), end));
        }
        offset = end;
    }
    Ok(ranges)
}

/// The MPEG frames between leading ID3v2 tags and trailing APEv2 and ID3v1 tags; None when no
/// frame sync follows the leading tags.
fn mpeg(file: &mut File, length: u64) -> io::Result<Option<(u64, u64)>> {
    let mut start = 0;
    let mut header = [0; 10];
    loop {
        file.seek(SeekFrom::Start(start))?;
        if start + 10 > length {
            return Ok(None);
        }
        file.read_exact(&mut header)?;
        if &header[..3] != b"ID3" {
            break;
        }
        let size = header[6..10]
            .iter()
            .fold(0_u64, |size, byte| size << 7 | u64::from(byte & 0x7f));
        start += 10 + size + if header[5] & 0x10 != 0 { 10 } else { 0 };
    }
    if header[0] != 0xff || header[1] & 0xe0 != 0xe0 {
        return Ok(None);
    }
    let mut end = length;
    let mut tail = [0; 32];
    if end >= start + 128 {
        file.seek(SeekFrom::Start(end - 128))?;
        file.read_exact(&mut tail[..3])?;
        if &tail[..3] == b"TAG" {
            end -= 128;
        }
    }
    if end >= start + 32 {
        file.seek(SeekFrom::Start(end - 32))?;
        file.read_exact(&mut tail)?;
        if &tail[..8] == b"APETAGEX" {
            let size = u64::from(u32::from_le_bytes([tail[12], tail[13], tail[14], tail[15]]));
            let header = if tail[23] & 0x80 != 0 { 32 } else { 0 };
            end = end.saturating_sub(size + header).max(start);
        }
    }
    Ok(Some((start, end)))
}
