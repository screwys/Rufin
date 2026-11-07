use crate::host::{AndroidError, error};
use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
    sync::OnceLock,
    time::Duration,
};

use jni::{
    Env, JavaVM, jni_sig, jni_str,
    objects::{Global, JByteArray, JObject, JObjectArray, JValue},
};
use rufin_connect::dns::{
    BoxIter, DnsError, DnsResolver, NameserverConfig, Resolver, TxtRecordData,
};

static NETWORK: OnceLock<Global<JObject<'static>>> = OnceLock::new();

pub(crate) fn initialize(env: &mut Env<'_>, context: &JObject<'_>) -> jni::errors::Result<()> {
    let network = env.new_object(
        jni_str!("io/github/screwys/rufin/platform/AndroidNetwork"),
        jni_sig!("(Landroid/content/Context;)V"),
        &[JValue::Object(context)],
    )?;
    let _ = NETWORK.set(env.new_global_ref(network)?);
    rufin_connect::network::install_dns_resolver(|| {
        DnsResolver::custom(AndroidDns {
            network: NETWORK.get().expect("Android networking is initialized"),
            resolver: OnceLock::new(),
        })
    });
    Ok(())
}

#[derive(Debug)]
struct AndroidDns {
    network: &'static Global<JObject<'static>>,
    resolver: OnceLock<DnsResolver>,
}

impl AndroidDns {
    fn nameservers(&self) -> io::Result<Vec<IpAddr>> {
        let bytes = JavaVM::singleton()
            .and_then(|vm| {
                vm.attach_current_thread(|env| {
                    let servers = env
                        .call_method(
                            &self.network,
                            jni_str!("dnsServers"),
                            jni_sig!("()[[B"),
                            &[],
                        )?
                        .l()?;
                    let servers = env.cast_local::<JObjectArray<JByteArray>>(servers)?;
                    (0..servers.len(env)?)
                        .map(|index| {
                            let server = servers.get_element(env, index)?;
                            env.convert_byte_array(server)
                        })
                        .collect::<jni::errors::Result<Vec<_>>>()
                })
            })
            .map_err(io::Error::other)?;
        bytes
            .into_iter()
            .map(|bytes| match bytes.len() {
                4 => Ok(IpAddr::from(<[u8; 4]>::try_from(bytes).unwrap())),
                16 => Ok(IpAddr::from(<[u8; 16]>::try_from(bytes).unwrap())),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid Android DNS address",
                )),
            })
            .collect()
    }

    fn resolver(&self) -> &DnsResolver {
        self.resolver.get_or_init(|| {
            let nameservers = self.nameservers().unwrap_or_else(|error| {
                tracing::warn!(%error, "Could not read Android DNS configuration");
                Vec::new()
            });
            DnsResolver::builder()
                .add_nameserver_configs(nameservers.into_iter().map(NameserverConfig::udp))
                .build()
        })
    }
}

type Lookup<T> = Pin<Box<dyn Future<Output = Result<BoxIter<T>, DnsError>> + Send>>;

impl Resolver for AndroidDns {
    fn lookup_ipv4(&self, host: String) -> Lookup<Ipv4Addr> {
        let resolver = self.resolver().clone();
        Box::pin(async move {
            let addresses = resolver.lookup_ipv4(host, Duration::MAX).await?;
            Ok(Box::new(addresses.filter_map(|address| match address {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            })) as BoxIter<_>)
        })
    }

    fn lookup_ipv6(&self, host: String) -> Lookup<Ipv6Addr> {
        let resolver = self.resolver().clone();
        Box::pin(async move {
            let addresses = resolver.lookup_ipv6(host, Duration::MAX).await?;
            Ok(Box::new(addresses.filter_map(|address| match address {
                IpAddr::V6(ip) => Some(ip),
                IpAddr::V4(_) => None,
            })) as BoxIter<_>)
        })
    }

    fn lookup_txt(&self, host: String) -> Lookup<TxtRecordData> {
        let resolver = self.resolver().clone();
        Box::pin(async move {
            let records = resolver
                .lookup_txt(host, Duration::MAX)
                .await?
                .collect::<Vec<_>>();
            Ok(Box::new(records.into_iter()) as BoxIter<_>)
        })
    }

    fn clear_cache(&self) {
        if let Some(resolver) = self.resolver.get() {
            resolver.clear_cache();
        }
    }

    fn reset(&self) -> Box<dyn Resolver> {
        Box::new(Self {
            network: self.network,
            resolver: OnceLock::new(),
        })
    }
}

#[derive(uniffi::Object)]
pub struct AndroidNetworkSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<rufin_core::connect::Status>>,
}

impl AndroidNetworkSubscription {
    pub(crate) fn new(
        mut changes: tokio::sync::watch::Receiver<rufin_core::connect::Status>,
    ) -> Self {
        changes.mark_changed();
        Self {
            changes: tokio::sync::Mutex::new(changes),
        }
    }
}

#[uniffi::export]
impl AndroidNetworkSubscription {
    pub async fn nearby_discovery(&self) -> Result<bool, AndroidError> {
        let mut changes = self.changes.lock().await;
        changes.changed().await.map_err(error)?;
        let status = changes.borrow_and_update();
        Ok(status.settings.enabled && status.settings.nearby)
    }
}
