//! Incremental Multi-Status parsing for listings and sync-collection responses.

use quick_xml::{NsReader, events::Event, name::ResolveResult};
use tokio::io::AsyncBufRead;

use crate::{SourceError, SourceResult};

const DAV: &str = "DAV:";
const OC: &str = "http://owncloud.org/ns";

#[derive(Debug, Default)]
pub(crate) struct Entry {
    pub href: String,
    pub status: Option<u16>,
    pub directory: bool,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub etag: Option<String>,
    pub native_id: Option<String>,
    pub permissions: Option<String>,
    pub sync_token: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DirectoryRevision {
    etag: Option<String>,
    sync_token: String,
}

impl Entry {
    pub fn revision(&self) -> Option<String> {
        if self.directory
            && let Some(token) = &self.sync_token
        {
            return Some(
                serde_json::to_string(&DirectoryRevision {
                    etag: self.etag.clone(),
                    sync_token: token.clone(),
                })
                .expect("directory revision"),
            );
        }
        self.etag.clone().or_else(|| {
            self.modified
                .as_ref()
                .map(|modified| format!("{modified}:{}", self.size.unwrap_or_default()))
        })
    }
}

pub(crate) fn sync_token(revision: &str) -> Option<String> {
    serde_json::from_str::<DirectoryRevision>(revision)
        .ok()
        .map(|revision| revision.sync_token)
}

#[derive(Clone, Copy, PartialEq)]
enum Element {
    Multistatus,
    Response,
    Propstat,
    Prop,
    Status,
    Href,
    Collection,
    Size,
    Modified,
    Etag,
    ResourceId,
    FileId,
    InstanceFileId,
    Permissions,
    SyncToken,
    Other,
}

fn element(namespace: ResolveResult<'_>, local: &str) -> Element {
    use Element::*;
    match namespace {
        ResolveResult::Bound(ns) if ns.as_ref() == DAV => match local {
            "multistatus" => Multistatus,
            "response" => Response,
            "propstat" => Propstat,
            "prop" => Prop,
            "status" => Status,
            "href" => Href,
            "collection" => Collection,
            "getcontentlength" => Size,
            "getlastmodified" => Modified,
            "getetag" => Etag,
            "resource-id" => ResourceId,
            "sync-token" => SyncToken,
            _ => Other,
        },
        ResolveResult::Bound(ns) if ns.as_ref() == OC => match local {
            "fileid" => FileId,
            "id" => InstanceFileId,
            "permissions" => Permissions,
            _ => Other,
        },
        _ => Other,
    }
}

pub(crate) async fn parse<F: std::future::Future<Output = SourceResult<()>>>(
    input: impl AsyncBufRead + Unpin,
    mut accept: impl FnMut(Entry) -> F,
) -> SourceResult<Option<String>> {
    use Element::*;
    let mut reader = NsReader::from_reader(input);
    reader.config_mut().expand_empty_elements = true;
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut response = Entry::default();
    let mut properties = Entry::default();
    let mut property_status = None;
    let mut text = String::new();
    let mut sync_token = None;
    let mut multistatus = false;
    let mut response_id_priority = 0;
    let mut property_id_priority = 0;
    loop {
        let (namespace, event) = reader
            .read_resolved_event_into_async(&mut buffer)
            .await
            .map_err(xml_error)?;
        match event {
            Event::Start(tag) => {
                let kind = element(namespace, tag.local_name().as_ref());
                if stack.is_empty() {
                    if kind != Multistatus || multistatus {
                        return Err(SourceError::Other(
                            "Expected a WebDAV Multi-Status response".into(),
                        ));
                    }
                    multistatus = true;
                }
                if kind == Response {
                    response = Entry::default();
                    response_id_priority = 0;
                }
                if kind == Propstat {
                    properties = Entry::default();
                    property_status = None;
                    property_id_priority = 0;
                }
                if kind == Collection && stack.contains(&Prop) {
                    properties.directory = true;
                }
                stack.push(kind);
                text.clear();
            }
            Event::Text(value) => {
                text.push_str(&value.xml_content(quick_xml::XmlVersion::Implicit1_0))
            }
            Event::CData(value) => text.push_str(&value),
            Event::GeneralRef(value) => {
                if let Some(character) = value.resolve_char_ref().map_err(xml_error)? {
                    text.push(character);
                } else {
                    let name = value;
                    text.push_str(match &*name {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => return Err(SourceError::Other("Unknown WebDAV XML entity".into())),
                    });
                }
            }
            Event::End(_) => {
                let kind = stack.pop().unwrap_or(Other);
                let value = text.trim();
                match kind {
                    Href if stack.contains(&ResourceId) => {
                        properties.native_id = (!value.is_empty()).then(|| format!("dav:{value}"));
                        property_id_priority = 3;
                    }
                    Href if stack.last() == Some(&Response) => response.href = value.to_string(),
                    Status => {
                        let status = value
                            .split_whitespace()
                            .nth(1)
                            .and_then(|v| v.parse::<u16>().ok());
                        if stack.contains(&Propstat) {
                            property_status = status;
                        } else {
                            response.status = status;
                        }
                    }
                    Size => properties.size = value.parse().ok(),
                    Modified => properties.modified = Some(value.to_string()),
                    Etag => properties.etag = Some(value.to_string()),
                    FileId | InstanceFileId => {
                        let priority = if kind == FileId { 2 } else { 1 };
                        if priority > property_id_priority && !value.is_empty() {
                            let namespace = if kind == FileId { "oc-fileid" } else { "oc-id" };
                            properties.native_id = Some(format!("{namespace}:{value}"));
                            property_id_priority = priority;
                        }
                    }
                    Permissions => properties.permissions = Some(value.to_string()),
                    SyncToken if stack.contains(&Propstat) => {
                        properties.sync_token = Some(value.to_string())
                    }
                    SyncToken => sync_token = Some(value.to_string()),
                    Propstat if property_status.is_some_and(|code| (200..300).contains(&code)) => {
                        response.directory |= properties.directory;
                        macro_rules! merge { ($($field:ident),*) => {$(
                            if properties.$field.is_some() { response.$field = properties.$field.take(); }
                        )*}; }
                        merge!(size, modified, etag, permissions, sync_token);
                        if properties.native_id.is_some()
                            && property_id_priority > response_id_priority
                        {
                            response.native_id = properties.native_id.take();
                            response_id_priority = property_id_priority;
                        }
                    }
                    Response => accept(std::mem::take(&mut response)).await?,
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => {
                if !multistatus || !stack.is_empty() {
                    return Err(SourceError::Other("Incomplete WebDAV response".into()));
                }
                break;
            }
            _ => {}
        }
        buffer.clear();
    }
    Ok(sync_token)
}

fn xml_error(error: impl std::fmt::Display) -> SourceError {
    SourceError::Other(format!("Invalid WebDAV response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn publishes_each_complete_row_before_the_listing_finishes() {
        let (mut writer, input) = tokio::io::duplex(1024);
        let (sent, mut received) = tokio::sync::mpsc::channel(1);
        let parsing = tokio::spawn(async move {
            parse(tokio::io::BufReader::new(input), move |row| {
                let sent = sent.clone();
                async move {
                    sent.send(row).await.unwrap();
                    Ok(())
                }
            })
            .await
        });
        writer.write_all(br#"<x:multistatus xmlns:x="DAV:" xmlns:o="http://owncloud.org/ns"><x:response><x:href>/music/a%20&amp;%20b/</x:href><x:propstat><x:prop><x:resourcetype><x:collection/></x:resourcetype><o:id>123instance</o:id><o:fileid>123</o:fileid><x:getetag>&quot;v1&quot;</x:getetag></x:prop><x:status>HTTP/1.1 200 OK</x:status></x:propstat><x:propstat><x:prop><x:getcontentlength>999</x:getcontentlength></x:prop><x:status>HTTP/1.1 404 Not Found</x:status></x:propstat></x:response>"#).await.unwrap();
        let row = tokio::time::timeout(std::time::Duration::from_secs(1), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.href, "/music/a%20&%20b/");
        assert!(row.directory);
        assert_eq!(row.size, None);
        assert_eq!(row.etag.as_deref(), Some("\"v1\""));
        assert_eq!(row.native_id.as_deref(), Some("oc-fileid:123"));
        assert!(!parsing.is_finished());
        writer
            .write_all(b"<x:sync-token>next</x:sync-token></x:multistatus>")
            .await
            .unwrap();
        drop(writer);
        assert_eq!(parsing.await.unwrap().unwrap().as_deref(), Some("next"));
    }

    #[tokio::test]
    async fn a_truncated_listing_cannot_complete_an_authoritative_scan() {
        let xml = br#"<multistatus xmlns="DAV:"><response><href>/a</href><status>HTTP/1.1 404 Not Found</status></response><response>"#;
        let mut deleted = None;
        assert!(
            parse(&xml[..], |row| {
                deleted = row.status;
                std::future::ready(Ok(()))
            })
            .await
            .is_err()
        );
        assert_eq!(deleted, Some(404));
    }
}
