//! A hash of a file's audio payload that survives tag edits where the container allows it.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const ASF_HEADER: [u8; 16] = [
    0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];
const ASF_DATA: [u8; 16] = [
    0x36, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];

/// A hash of the audio payload of a Local file, tags excluded where the container allows:
/// MPEG, FLAC, APE and WavPack (ID3v2 head, APEv2 and ID3v1 tail and FLAC metadata blocks
/// skipped), Ogg (header packets skipped), MP4 (mdat), RIFF/AIFF (data/SSND) and ASF (Data
/// Object); otherwise the whole file.
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
    } else if head == ASF_HEADER[..12] {
        asf(&mut file, length)?
    } else {
        tagged(&mut file, length)?
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

/// The bodies of the top-level Data Objects of an ASF file.
fn asf(file: &mut File, length: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut ranges = Vec::new();
    let mut header = [0; 24];
    let mut offset = 0;
    while offset + 24 <= length {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header)?;
        let size = u64::from_le_bytes(header[16..].try_into().expect("8 bytes"));
        // A broadcast Data Object leaves its size at zero and runs to the end.
        let end = match size {
            0 => length,
            size if size < 24 => break,
            size => offset.saturating_add(size).min(length),
        };
        if header[..16] == ASF_DATA {
            ranges.push((offset + 24, end));
        }
        offset = end;
    }
    Ok(ranges)
}

/// The audio of a file whose tags sit around it: after leading ID3v2 tags, MPEG frames, APE
/// or WavPack blocks up to trailing APEv2 and ID3v1 tags, FLAC frames after its metadata
/// blocks, or an Ogg stream's packets after its headers. None of them when the payload is
/// not one of these.
fn tagged(file: &mut File, length: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut start = 0;
    let mut header = [0; 10];
    loop {
        if start + 10 > length {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut header)?;
        if &header[..3] != b"ID3" {
            break;
        }
        let size = header[6..10]
            .iter()
            .fold(0_u64, |size, byte| size << 7 | u64::from(byte & 0x7f));
        start += 10 + size + if header[5] & 0x10 != 0 { 10 } else { 0 };
    }
    match &header[..4] {
        b"OggS" => ogg(file, start, length),
        b"fLaC" => {
            let end = tail(file, start, length)?;
            Ok(flac(file, start + 4, end)?.into_iter().collect())
        }
        b"wvpk" | b"MAC " => Ok(vec![(start, tail(file, start, length)?)]),
        _ if header[0] == 0xff && header[1] & 0xe0 == 0xe0 => {
            Ok(vec![(start, tail(file, start, length)?)])
        }
        _ => Ok(Vec::new()),
    }
}

/// Where the audio ends before trailing ID3v1 and APEv2 tags.
fn tail(file: &mut File, start: u64, length: u64) -> io::Result<u64> {
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
    Ok(end)
}

/// The FLAC frames after the metadata blocks that start at `offset`.
fn flac(file: &mut File, mut offset: u64, end: u64) -> io::Result<Option<(u64, u64)>> {
    let mut header = [0; 4];
    loop {
        if offset + 4 > end {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header)?;
        offset += 4 + u64::from(u32::from_be_bytes([0, header[1], header[2], header[3]]));
        if header[0] & 0x80 != 0 {
            return Ok((offset < end).then_some((offset, end)));
        }
    }
}

/// The packet data of an Ogg stream after its header packets, which hold its tags. Pages are
/// left out, since a tag edit renumbers them. None of it for a stream of several logical
/// streams or of a codec it does not know.
fn ogg(file: &mut File, start: u64, length: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut ranges = Vec::<(u64, u64)>::new();
    let mut header = [0; 27];
    let mut lacing = [0; 255];
    let mut first = Vec::new();
    let (mut headers, mut packets, mut serial) = (None, 0, None);
    let mut offset = start;
    while offset + 27 <= length {
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut header)?;
        let page = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
        if &header[..4] != b"OggS" || *serial.get_or_insert(page) != page {
            return Ok(Vec::new());
        }
        let segments = &mut lacing[..usize::from(header[26])];
        file.read_exact(segments)?;
        let mut body = offset + 27 + segments.len() as u64;
        for &lace in segments.iter() {
            let size = u64::from(lace);
            if headers.is_none() {
                (&mut *file).take(size).read_to_end(&mut first)?;
            } else if headers.is_some_and(|count| packets >= count) {
                match ranges.last_mut() {
                    Some((_, end)) if *end == body => *end = body + size,
                    _ => ranges.push((body, body + size)),
                }
            }
            body += size;
            if lace < 255 {
                packets += 1;
                if headers.is_none() {
                    let Some(count) = ogg_headers(&first) else {
                        return Ok(Vec::new());
                    };
                    headers = Some(count);
                }
            }
        }
        offset = body;
    }
    Ok(ranges)
}

/// How many header packets a logical stream starts with, from its first packet.
fn ogg_headers(first: &[u8]) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| first.get(range);
    if first.starts_with(b"\x01vorbis") {
        Some(3)
    } else if first.starts_with(b"OpusHead") {
        Some(2)
    } else if first.starts_with(b"Speex   ") {
        let extra = number(68..72)?;
        Some(2 + u64::from(u32::from_le_bytes(extra.try_into().ok()?)))
    } else if first.starts_with(b"\x7fFLAC") {
        let count = u16::from_be_bytes(number(7..9)?.try_into().ok()?);
        (count > 0).then(|| 1 + u64::from(count))
    } else {
        None
    }
}
