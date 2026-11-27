use std::net::{SocketAddr, SocketAddrV4};
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::Endpoint;
use portmapper::{Client, Config, Protocol};
use tokio::task::JoinHandle;

const RELEASE_WAIT: Duration = Duration::from_secs(3);

/// Best-effort UPnP/PCP/NAT-PMP mapping. `portmapper` already probes in
/// PCP -> NAT-PMP -> UPnP order and renews at half the lease lifetime.
pub(crate) struct PortMap {
    endpoint: Endpoint,
    client: Option<Client>,
    watcher: JoinHandle<()>,
    published: Arc<Mutex<Option<SocketAddrV4>>>,
}

impl PortMap {
    /// Returns `None` when mapping is disabled or no valid local port is known.
    /// A failed mapping is not fatal: iroh keeps using relays/direct paths.
    pub(crate) async fn start(
        endpoint: Endpoint,
        enabled: bool,
        local_port: Option<u16>,
    ) -> Option<Self> {
        if !enabled {
            return None;
        }
        let port = NonZeroU16::new(local_port?)?;
        let client = Client::new(Config {
            enable_upnp: true,
            enable_pcp: true,
            enable_nat_pmp: true,
            protocol: Protocol::Udp,
        });
        let mut external = client.watch_external_address();
        let published = Arc::new(Mutex::new(None));
        let watcher_published = published.clone();
        let watcher_endpoint = endpoint.clone();
        let watcher = tokio::spawn(async move {
            let mut published: Option<SocketAddrV4> = None;
            loop {
                let current = *external.borrow();
                if current != published {
                    match (published, current) {
                        (Some(old), None) => {
                            watcher_endpoint
                                .remove_external_addr(&SocketAddr::V4(old))
                                .await;
                            tracing::info!("端口映射已释放，回退到 relay/直连");
                        }
                        (_, Some(addr)) => {
                            watcher_endpoint
                                .add_external_addr(SocketAddr::V4(addr))
                                .await;
                            tracing::info!(%addr, "端口映射已建立");
                        }
                        (None, None) => {}
                    }
                    published = current;
                    if let Ok(mut guard) = watcher_published.lock() {
                        *guard = current;
                    }
                }
                if external.changed().await.is_err() {
                    break;
                }
            }
            if let Some(old) = published {
                watcher_endpoint
                    .remove_external_addr(&SocketAddr::V4(old))
                    .await;
            }
        });
        client.update_local_port(port);
        Some(Self {
            endpoint,
            client: Some(client),
            watcher,
            published,
        })
    }

    /// Ask the router to release the mapping before the service is dropped.
    pub(crate) async fn shutdown(mut self) {
        if let Some(client) = self.client.take() {
            client.deactivate();
            let mut external = client.watch_external_address();
            if external.borrow().is_some() {
                let _ = tokio::time::timeout(RELEASE_WAIT, async {
                    loop {
                        if external.borrow().is_none() {
                            break;
                        }
                        if external.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
            }
            drop(client);
        }
        self.watcher.abort();
        let published = self
            .published
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(addr) = published {
            self.endpoint
                .remove_external_addr(&SocketAddr::V4(addr))
                .await;
        }
    }
}

impl Drop for PortMap {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            client.deactivate();
        }
        self.watcher.abort();
        if let Some(addr) = self
            .published
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let endpoint = self.endpoint.clone();
                handle.spawn(async move {
                    endpoint.remove_external_addr(&SocketAddr::V4(addr)).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a router with UPnP/PCP/NAT-PMP enabled"]
    async fn port_mapping_probe_records_router_requirement() {
        let client = Client::new(Config::default());
        let result = client
            .probe()
            .await
            .expect("portmapper probe channel must answer");
        match result {
            Ok(available) => assert!(
                available.upnp || available.pcp || available.nat_pmp,
                "enable UPnP, PCP or NAT-PMP on the test router: {available}"
            ),
            Err(err) => panic!("port mapping probe requires a capable router: {err}"),
        }
    }
}
