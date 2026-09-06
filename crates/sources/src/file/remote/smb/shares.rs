//! NetrShareEnum over NDR32, supported by Samba and Windows SRVSVC.
//! The SMB client's RPC convenience API currently binds NDR64 only.

use smb::{Client, Pipe};
use smb_msg::{IoctlBuffer, PipeTransceiveRequest};

use crate::{SourceError, SourceResult};

pub(super) fn list(client: &Client, server: &str) -> SourceResult<Vec<String>> {
    let pipe = client.open_pipe(server, "srvsvc").map_err(super::error)?;
    let result = enumerate(&pipe, server);
    let _ = pipe.close();
    result
}

fn enumerate(pipe: &Pipe, server: &str) -> SourceResult<Vec<String>> {
    let mut bind = vec![0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0];
    // SRVSVC 3.0 and the standard NDR32 transfer syntax 2.0.
    bind.extend_from_slice(&[
        0xc8, 0x4f, 0x32, 0x4b, 0x70, 0x16, 0xd3, 0x01, 0x12, 0x78, 0x5a, 0x47, 0xbf, 0x6e, 0xe1,
        0x88, 3, 0, 0, 0,
    ]);
    bind.extend_from_slice(&[
        4, 0x5d, 0x88, 0x8a, 0xeb, 0x1c, 0xc9, 0x11, 0x9f, 0xe8, 8, 0, 0x2b, 0x10, 0x48, 0x60, 2,
        0, 0, 0,
    ]);
    let ack = exchange(pipe, 11, 12, 1, &bind)?;
    let page_bytes = u16::from_le_bytes(
        ack.get(..2)
            .ok_or_else(|| invalid("Truncated RPC bind response"))?
            .try_into()
            .unwrap(),
    )
    .saturating_sub(512)
    .max(1024) as u32;
    let mut ack = Reader(&ack, 8);
    let address_length = u16::from_le_bytes(ack.take(2)?.try_into().unwrap()) as usize;
    ack.take(address_length)?;
    ack.align()?;
    if ack.word()? != 1 || ack.word()? != 0 {
        return Err(invalid("NDR32 share enumeration was rejected"));
    }

    let mut resume = 0;
    let mut call = 2;
    let mut names = Vec::new();
    loop {
        let mut request = Vec::new();
        word(&mut request, 1); // server name pointer
        string(&mut request, &format!(r"\\{server}"));
        for value in [1, 1, 1, 0, 0, page_bytes, 1, resume] {
            word(&mut request, value);
        }
        let mut body = Vec::new();
        word(&mut body, request.len() as u32);
        body.extend_from_slice(&[0, 0, 15, 0]); // context 0, NetrShareEnum opnum 15
        body.extend(request);
        let response = exchange(pipe, 0, 2, call, &body)?;
        let mut reader = Reader(&response, 8); // alloc_hint, context, cancel/reserved
        if reader.word()? != 1 || reader.word()? != 1 {
            return Err(invalid("Unexpected share information level"));
        }
        if reader.word()? != 0 {
            let count = reader.word()? as usize;
            let pointer = reader.word()?;
            if pointer != 0 {
                let capacity = reader.word()? as usize;
                if count > capacity || count > response.len() / 12 {
                    return Err(invalid("Invalid share array length"));
                }
                let entries = (0..count)
                    .map(|_| Ok((reader.word()?, reader.word()?, reader.word()?)))
                    .collect::<SourceResult<Vec<_>>>()?;
                for (name, kind, remark) in entries {
                    let name = if name != 0 {
                        Some(reader.string()?)
                    } else {
                        None
                    };
                    if remark != 0 {
                        reader.string()?;
                    }
                    if kind & 0xffff == 0
                        && let Some(name) = name
                    {
                        names.push(name);
                    }
                }
            } else if count != 0 {
                return Err(invalid("Missing share array"));
            }
        }
        let _total = reader.word()?;
        let next = if reader.word()? != 0 {
            reader.word()?
        } else {
            0
        };
        match reader.word()? {
            0 => break,
            234 if next != resume => resume = next, // ERROR_MORE_DATA
            status => {
                return Err(SourceError::Other(format!(
                    "SMB share enumeration returned Windows status {status}"
                )));
            }
        }
        call += 1;
    }
    Ok(names)
}

fn exchange(pipe: &Pipe, kind: u8, expected: u8, call: u32, body: &[u8]) -> SourceResult<Vec<u8>> {
    let length = u16::try_from(body.len() + 16).map_err(|_| invalid("RPC request is too large"))?;
    let mut packet = vec![5, 0, kind, 3, 0x10, 0, 0, 0];
    packet.extend(length.to_le_bytes());
    packet.extend([0, 0]);
    packet.extend(call.to_le_bytes());
    packet.extend(body);
    let response = pipe
        .fsctl_with_options(
            PipeTransceiveRequest::from(IoctlBuffer::from(packet)),
            65536,
        )
        .map_err(super::error)?;
    let bytes: &[u8] = response.as_ref();
    if bytes.len() < 16
        || bytes[0] != 5
        || bytes[2] != expected
        || bytes[3] & 3 != 3
        || bytes[4] != 0x10
        || bytes[10..12] != [0, 0]
        || bytes[12..16] != call.to_le_bytes()
    {
        return Err(invalid("Invalid share enumeration RPC response"));
    }
    let end = u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize;
    bytes
        .get(16..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| invalid("Truncated RPC response"))
}

fn word(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend(value.to_le_bytes());
}
fn string(bytes: &mut Vec<u8>, value: &str) {
    let words = value.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    for count in [words.len() as u32, 0, words.len() as u32] {
        word(bytes, count);
    }
    for word in words {
        bytes.extend(word.to_le_bytes());
    }
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

struct Reader<'a>(&'a [u8], usize);
impl Reader<'_> {
    fn take(&mut self, count: usize) -> SourceResult<&[u8]> {
        let end = self
            .1
            .checked_add(count)
            .ok_or_else(|| invalid("Invalid NDR length"))?;
        let bytes = self
            .0
            .get(self.1..end)
            .ok_or_else(|| invalid("Truncated NDR share response"))?;
        self.1 = end;
        Ok(bytes)
    }
    fn align(&mut self) -> SourceResult<()> {
        self.take((4 - self.1 % 4) % 4).map(|_| ())
    }
    fn word(&mut self) -> SourceResult<u32> {
        self.align()?;
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self) -> SourceResult<String> {
        let maximum = self.word()?;
        let offset = self.word()?;
        let count = self.word()?;
        if offset > maximum || count > maximum - offset {
            return Err(invalid("Invalid NDR string length"));
        }
        let length = (count as usize)
            .checked_mul(2)
            .ok_or_else(|| invalid("Invalid NDR string length"))?;
        let bytes = self.take(length)?;
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        let words = words.strip_suffix(&[0]).unwrap_or(&words);
        String::from_utf16(words).map_err(|_| invalid("Invalid share name encoding"))
    }
}
fn invalid(message: &str) -> SourceError {
    SourceError::Network(message.into())
}
