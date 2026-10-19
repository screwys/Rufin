use std::{
    borrow::Cow,
    io::{Read, Write},
};

use age::x25519::Identity;
use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};

pub(super) fn sensitive(kind: &str) -> bool {
    matches!(
        kind.split(':').next(),
        Some("source" | "integration" | "scrobbling" | "connect_key")
    )
}

pub(super) fn protect<'a>(
    kind: &str,
    bytes: &'a [u8],
    identity: &Identity,
) -> Result<Cow<'a, [u8]>> {
    if !sensitive(kind) {
        return Ok(Cow::Borrowed(bytes));
    }
    let recipient = identity.to_public();
    let mut output =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))?
            .wrap_output(Vec::new())?;
    output.write_all(bytes)?;
    Ok(Cow::Owned(output.finish()?))
}

pub(super) fn expose<'a>(
    kind: &str,
    bytes: &'a [u8],
    identity: &Identity,
) -> Result<Cow<'a, [u8]>> {
    if !sensitive(kind) {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut input =
        age::Decryptor::new(bytes)?.decrypt(std::iter::once(identity as &dyn age::Identity))?;
    let mut plaintext = Vec::new();
    input.read_to_end(&mut plaintext)?;
    Ok(Cow::Owned(plaintext))
}

pub(super) fn protect_payload(
    kind: &str,
    payload: Option<String>,
    identity: &Identity,
) -> Result<Option<String>> {
    payload
        .map(|payload| {
            if sensitive(kind) {
                protect(kind, payload.as_bytes(), identity).map(|bytes| STANDARD.encode(bytes))
            } else {
                Ok(payload)
            }
        })
        .transpose()
}

pub(super) fn expose_payload(
    kind: &str,
    payload: Option<String>,
    identity: &Identity,
) -> Result<Option<String>> {
    payload
        .map(|payload| {
            if sensitive(kind) {
                Ok(String::from_utf8(
                    expose(kind, &STANDARD.decode(payload)?, identity)?.into_owned(),
                )?)
            } else {
                Ok(payload)
            }
        })
        .transpose()
}
