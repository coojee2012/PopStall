use std::path::Path;
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_tickets::Ticket;
use iroh_tickets::endpoint::EndpointTicket;

use super::plaza::Plaza;
use super::protocol::StallHandler;
use crate::storage::load_or_create_secret_key;

pub const ALPN: &[u8] = b"popstall/http/1";
const RELAY_WAIT: Duration = Duration::from_secs(12);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);

pub struct ShareMaterial {
    pub code: String,
    pub relay_ready: bool,
    pub endpoint_id: String,
}

pub struct Node {
    pub endpoint: Endpoint,
    /// Kept alive so the accept loop continues after `start` returns.
    #[allow(dead_code)]
    pub router: Router,
    handler: StallHandler,
    plaza: Option<Plaza>,
}

impl Node {
    pub async fn start(data_dir: &Path) -> anyhow::Result<Self> {
        let secret_key = load_or_create_secret_key(data_dir)?;
        let handler = StallHandler::new();
        let endpoint = bind_endpoint(secret_key).await?;
        let mut router = Router::builder(endpoint.clone()).accept(ALPN, handler.clone());
        let plaza = if cfg!(test) {
            None
        } else {
            let mdns = super::plaza::attach_mdns(&endpoint);
            let gossip = iroh_gossip::Gossip::builder().spawn(endpoint.clone());
            router = router.accept(iroh_gossip::ALPN, gossip.clone());
            Some(super::plaza::start(endpoint.clone(), gossip, mdns))
        };
        let router = router.spawn();
        tracing::info!(endpoint_id = %endpoint.id(), "iroh endpoint is bound");
        Ok(Self {
            endpoint,
            router,
            handler,
            plaza,
        })
    }

    pub async fn set_host(&self, target: super::HostTarget) {
        self.handler.set_host(target).await;
    }

    pub async fn clear_host(&self) {
        self.handler.clear_host().await;
    }

    pub fn announce_stall(&self, title: &str, label: &str) {
        if let Some(plaza) = &self.plaza {
            plaza.announce(title, label);
        }
    }

    pub fn retract_stall(&self) {
        if let Some(plaza) = &self.plaza {
            plaza.retract();
        }
    }

    pub async fn plaza_snapshot(&self) -> super::plaza::PlazaSnapshot {
        match &self.plaza {
            Some(plaza) => plaza.snapshot(self.endpoint.id()).await,
            None => super::plaza::PlazaSnapshot::default(),
        }
    }

    pub async fn share_material(&self) -> ShareMaterial {
        if !relay_ready(&self.endpoint) {
            let ready = wait_for_relay(&self.endpoint, RELAY_WAIT).await;
            if !ready {
                tracing::warn!("relay address was not ready; the share code will be refreshed once it is");
            }
        }
        self.share_snapshot()
    }

    pub fn share_snapshot(&self) -> ShareMaterial {
        let addr = self.endpoint.addr();
        let ready = addr.relay_urls().next().is_some();
        if let Some(relay) = addr.relay_urls().next() {
            tracing::info!(%relay, "share code includes a relay");
        }
        ShareMaterial {
            code: EndpointTicket::new(addr).encode_string(),
            relay_ready: ready,
            endpoint_id: self.endpoint.id().to_string(),
        }
    }

    pub async fn connect(&self, share_code: &str) -> anyhow::Result<iroh::endpoint::Connection> {
        let ticket = EndpointTicket::decode_string(share_code.trim())
            .map_err(|err| anyhow::anyhow!("分享码无法解析: {err:?}"))?;
        let addr = ticket.endpoint_addr().clone();
        self.ensure_not_self(addr.id)?;
        let dial = if addr.relay_urls().next().is_some() {
            addr
        } else {
            tracing::info!("share code has no relay; resolving the endpoint id");
            EndpointAddr::from(addr.id)
        };
        self.dial(dial).await
    }

    pub async fn connect_endpoint(&self, endpoint_id: &str) -> anyhow::Result<iroh::endpoint::Connection> {
        let id: EndpointId = endpoint_id
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("这个摊位的地址无效"))?;
        self.ensure_not_self(id)?;
        self.dial(EndpointAddr::from(id)).await
    }

    pub fn endpoint_id_of(share_code: &str) -> anyhow::Result<String> {
        let ticket = EndpointTicket::decode_string(share_code.trim())
            .map_err(|err| anyhow::anyhow!("分享码无法解析: {err:?}"))?;
        Ok(ticket.endpoint_addr().id.to_string())
    }

    fn ensure_not_self(&self, id: EndpointId) -> anyhow::Result<()> {
        if id == self.endpoint.id() {
            anyhow::bail!("这是你自己的摊位，用本机预览看");
        }
        Ok(())
    }

    async fn dial(&self, addr: EndpointAddr) -> anyhow::Result<iroh::endpoint::Connection> {
        match tokio::time::timeout(CONNECT_TIMEOUT, self.endpoint.connect(addr, ALPN)).await {
            Ok(Ok(connection)) => Ok(connection),
            Ok(Err(err)) => Err(anyhow::anyhow!("P2P 连接失败: {err:?}")),
            Err(_) => Err(anyhow::anyhow!(
                "P2P 连接超时。对方需要正在开张；跨网第一次握手可能要半分钟"
            )),
        }
    }
}

async fn bind_endpoint(secret_key: iroh::SecretKey) -> anyhow::Result<Endpoint> {
    if cfg!(test) {
        return Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .bind()
            .await
            .map_err(|err| anyhow::anyhow!("启动 P2P 节点失败: {err:?}"));
    }
    match Endpoint::builder(presets::N0)
        .secret_key(secret_key.clone())
        .address_lookup(iroh_mainline_address_lookup::DhtAddressLookup::builder())
        .bind()
        .await
    {
        Ok(endpoint) => Ok(endpoint),
        Err(err) => {
            tracing::warn!(?err, "DHT address lookup did not start");
            Endpoint::builder(presets::N0)
                .secret_key(secret_key)
                .bind()
                .await
                .map_err(|err| anyhow::anyhow!("启动 P2P 节点失败: {err:?}"))
        }
    }
}

fn relay_ready(endpoint: &Endpoint) -> bool {
    endpoint.addr().relay_urls().next().is_some()
}

async fn wait_for_relay(endpoint: &Endpoint, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if relay_ready(endpoint) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use tokio::io::AsyncReadExt;

    use super::super::HostTarget;
    use super::super::frame::{RequestFrame, encode_request};
    use super::super::io::read_response_head;
    use super::{ALPN, Node};
    use crate::server::StaticHost;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn loopback_fetches_index_over_iroh() {
        let dir = std::env::temp_dir().join(format!(
            "popstall-loop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("host-key")).unwrap();
        std::fs::create_dir_all(dir.join("guest-key")).unwrap();
        std::fs::create_dir_all(dir.join("site")).unwrap();
        std::fs::write(dir.join("site/index.html"), b"loopback-ok").unwrap();

        let host = Node::start(&dir.join("host-key")).await.unwrap();
        let site = StaticHost::bind(dir.join("site")).await.unwrap();
        host.set_host(HostTarget {
            router: site.router.clone(),
        })
        .await;

        let mut addr = iroh::EndpointAddr::new(host.endpoint.id());
        for socket in host.endpoint.bound_sockets() {
            let dialable = match socket.ip() {
                IpAddr::V4(ip) if ip.is_unspecified() => {
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), socket.port())
                }
                IpAddr::V6(ip) if ip.is_unspecified() => continue,
                _ => socket,
            };
            addr = addr.with_ip_addr(dialable);
        }

        let guest = Node::start(&dir.join("guest-key")).await.unwrap();
        let connection =
            tokio::time::timeout(Duration::from_secs(20), guest.endpoint.connect(addr, ALPN))
                .await
                .expect("connect timed out")
                .expect("connect failed");
        let (mut send, mut recv) = connection.open_bi().await.expect("open stream");
        let frame = encode_request(&RequestFrame::get("/")).unwrap();
        send.write_all(&frame).await.unwrap();
        send.finish().unwrap();

        let head = read_response_head(&mut recv).await.unwrap();
        assert_eq!(head.status, 200, "{}", head.content_type);
        let mut body = Vec::new();
        AsyncReadExt::read_to_end(&mut recv, &mut body)
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("loopback-ok"),
            "{}",
            String::from_utf8_lossy(&body)
        );

        site.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
