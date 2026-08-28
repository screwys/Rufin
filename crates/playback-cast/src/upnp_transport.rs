use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use roxmltree::{Document, Node};
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
struct UpnpService {
    service_type: String,
    control_url: Url,
}

#[derive(Clone, Debug)]
pub(crate) struct UpnpDevice {
    url: Url,
    friendly_name: String,
    services: Vec<UpnpService>,
    client: reqwest::blocking::Client,
    local_address: Option<IpAddr>,
    network_interface: Option<String>,
}

impl UpnpDevice {
    pub(crate) fn from_url(
        url: &str,
        local_address: Option<IpAddr>,
        network_interface: Option<&str>,
    ) -> Result<Self, String> {
        Self::from_url_with_timeout(url, local_address, network_interface, REQUEST_TIMEOUT)
    }

    pub(crate) fn from_url_with_timeout(
        url: &str,
        local_address: Option<IpAddr>,
        network_interface: Option<&str>,
        request_timeout: Duration,
    ) -> Result<Self, String> {
        let url = Url::parse(url).map_err(|error| error.to_string())?;
        let mut builder = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy();
        if let Some(local_address) = local_address {
            builder = builder.local_address(local_address);
        }
        #[cfg(any(
            target_os = "android",
            target_os = "fuchsia",
            target_os = "illumos",
            target_os = "ios",
            target_os = "linux",
            target_os = "macos",
            target_os = "solaris",
            target_os = "tvos",
            target_os = "visionos",
            target_os = "watchos",
        ))]
        if let Some(network_interface) = network_interface {
            builder = builder.interface(network_interface);
        }
        let client = builder.build().map_err(|error| error.to_string())?;
        let response = client
            .get(url.clone())
            .timeout(request_timeout)
            .send()
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "the renderer description responded with status code {}",
                response.status()
            ));
        }
        let description = response.text().map_err(|error| error.to_string())?;
        let document = Document::parse(&description).map_err(|error| error.to_string())?;
        let service_base = document
            .descendants()
            .find(|node| node.has_tag_name("URLBase"))
            .and_then(|node| node.text())
            .and_then(|base| Url::parse(base).ok())
            .unwrap_or_else(|| url.clone());
        let renderer = document
            .descendants()
            .filter(|node| node.has_tag_name("device"))
            .find(|node| {
                child_text(*node, "deviceType")
                    .is_some_and(|kind| device_type_matches(kind, "MediaRenderer"))
            })
            .ok_or_else(|| "UPnP description does not contain a MediaRenderer".to_string())?;
        let friendly_name = child_text(renderer, "friendlyName")
            .ok_or_else(|| "UPnP renderer does not have a friendly name".to_string())?
            .to_string();
        let services = renderer
            .descendants()
            .filter(|node| node.has_tag_name("service"))
            .filter_map(|service| {
                let service_type = child_text(service, "serviceType")?.to_string();
                let control_url = service_base.join(child_text(service, "controlURL")?).ok()?;
                Some(UpnpService {
                    service_type,
                    control_url,
                })
            })
            .collect();
        Ok(Self {
            url,
            friendly_name,
            services,
            client,
            local_address,
            network_interface: network_interface.map(str::to_string),
        })
    }

    pub(crate) fn friendly_name(&self) -> &str {
        &self.friendly_name
    }

    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn local_address(&self) -> Option<IpAddr> {
        self.local_address
    }

    pub(crate) fn network_interface(&self) -> Option<&str> {
        self.network_interface.as_deref()
    }

    pub(crate) fn has_service(&self, name: &str) -> bool {
        self.service(name).is_some()
    }

    pub(crate) fn action(
        &self,
        service_name: &str,
        action: &str,
        payload: &str,
    ) -> Result<HashMap<String, String>, String> {
        let service = self.service(service_name).ok_or_else(|| {
            format!(
                "{} does not provide UPnP {service_name}",
                self.friendly_name
            )
        })?;
        let envelope = format!(
            concat!(
                "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" ",
                "s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">",
                "<s:Body><u:{action} xmlns:u=\"{service_type}\">{payload}",
                "</u:{action}></s:Body></s:Envelope>"
            ),
            action = action,
            payload = payload,
            service_type = service.service_type,
        );
        let response = self
            .client
            .post(service.control_url.clone())
            .header("CONTENT-TYPE", "text/xml; charset=\"utf-8\"")
            .header(
                "SOAPAction",
                format!("\"{}#{action}\"", service.service_type),
            )
            .body(envelope)
            .send()
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "The control point responded with status code {}",
                response.status()
            ));
        }
        parse_action_response(&response.text().map_err(|error| error.to_string())?)
    }

    fn service(&self, name: &str) -> Option<&UpnpService> {
        self.services
            .iter()
            .find(|service| service_type_matches(&service.service_type, name))
    }
}

pub(crate) fn service_type_matches(service_type: &str, name: &str) -> bool {
    service_type.contains(&format!(":service:{name}:"))
}

pub(crate) fn device_type_matches(device_type: &str, name: &str) -> bool {
    device_type.contains(&format!(":device:{name}:"))
}

fn child_text<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)?
        .text()
}

fn parse_action_response(body: &str) -> Result<HashMap<String, String>, String> {
    let document = Document::parse(body).map_err(|error| error.to_string())?;
    let response = document
        .descendants()
        .find(|node| node.has_tag_name("Body"))
        .and_then(|body| body.children().find(Node::is_element))
        .ok_or_else(|| "UPnP response does not contain a SOAP body".to_string())?;
    if response.has_tag_name("Fault") {
        let description = response
            .descendants()
            .find(|node| node.has_tag_name("errorDescription"))
            .and_then(|node| node.text())
            .unwrap_or("UPnP action failed");
        return Err(description.to_string());
    }
    Ok(response
        .children()
        .filter(Node::is_element)
        .map(|node| {
            (
                node.tag_name().name().to_string(),
                node.text().unwrap_or_default().to_string(),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, SocketAddr};
    use std::sync::mpsc;
    use std::thread;

    use tiny_http::{Response, Server};

    use super::*;

    #[test]
    fn selected_interface_and_source_support_description_and_soap_requests() {
        let (selected_interface, selected) = if_addrs::get_if_addrs()
            .expect("local interfaces")
            .into_iter()
            .find_map(|interface| match interface.addr {
                if_addrs::IfAddr::V4(address) if address.ip.is_loopback() => {
                    Some((interface.name, IpAddr::V4(address.ip)))
                }
                _ => None,
            })
            .expect("IPv4 loopback interface");
        let server = Server::http(SocketAddr::new(selected, 0)).expect("fake renderer");
        let port = server
            .server_addr()
            .to_ip()
            .expect("renderer address")
            .port();
        let (sent, received) = mpsc::channel();
        let renderer = thread::spawn(move || {
            let description = server.recv().expect("description request");
            sent.send(*description.remote_addr().expect("description peer"))
                .expect("record description peer");
            description
                .respond(Response::from_string(
                    r#"<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>Test Renderer</friendlyName><serviceList><service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><serviceId>urn:upnp-org:serviceId:AVTransport</serviceId><SCPDURL>/transport.xml</SCPDURL><controlURL>/transport</controlURL><eventSubURL>/events</eventSubURL></service></serviceList></device></root>"#,
                ))
                .expect("description response");
            let action = server.recv().expect("SOAP request");
            sent.send(*action.remote_addr().expect("SOAP peer"))
                .expect("record SOAP peer");
            action
                .respond(Response::from_string(
                    r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:GetTransportInfoResponse xmlns:u="urn:schemas-upnp-org:service:AVTransport:1"><CurrentTransportState>STOPPED</CurrentTransportState></u:GetTransportInfoResponse></s:Body></s:Envelope>"#,
                ))
                .expect("SOAP response");
        });
        let device = UpnpDevice::from_url(
            &format!("http://{selected}:{port}/device.xml"),
            Some(selected),
            Some(&selected_interface),
        )
        .expect("bound renderer description");
        assert_eq!(device.friendly_name(), "Test Renderer");
        assert_eq!(device.url().port(), Some(port));
        assert_eq!(device.local_address(), Some(selected));
        assert_eq!(
            device.network_interface(),
            Some(selected_interface.as_str())
        );
        assert!(device.has_service("AVTransport"));
        let state = device
            .action(
                "AVTransport",
                "GetTransportInfo",
                "<InstanceID>0</InstanceID>",
            )
            .expect("bound SOAP action");

        assert_eq!(
            state.get("CurrentTransportState").map(String::as_str),
            Some("STOPPED")
        );
        assert!(received.iter().all(|peer| peer.ip() == selected));
        renderer.join().expect("renderer thread");
    }
}
