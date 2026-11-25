use std::path::PathBuf;

use iroh::endpoint::Connection;
use tokio::sync::{Mutex, OnceCell};

use crate::p2p::Node;
use crate::proxy::GuestProxy;
use crate::server::StaticHost;

pub struct AppState {
    data_dir: PathBuf,
    node: OnceCell<Node>,
    pub host_ops: Mutex<()>,
    pub host: Mutex<Option<HostSession>>,
    pub stage: Mutex<Option<StageSession>>,
}

pub struct HostSession {
    pub site: StaticHost,
    pub share_code: String,
    pub endpoint_id: String,
    pub relay_ready: bool,
    pub locked: bool,
}

pub struct StageSession {
    pub label: String,
    pub guest: Option<GuestSession>,
}

pub struct GuestSession {
    pub proxy: GuestProxy,
    pub connection: Connection,
    pub endpoint_id: String,
    pub title: String,
    pub password: String,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            node: OnceCell::new(),
            host_ops: Mutex::new(()),
            host: Mutex::new(None),
            stage: Mutex::new(None),
        }
    }

    pub fn started_node(&self) -> Option<&Node> {
        self.node.get()
    }

    pub async fn node(&self) -> anyhow::Result<&Node> {
        let data_dir = self.data_dir.clone();
        self.node
            .get_or_try_init(|| async move { Node::start(&data_dir).await })
            .await
            .map_err(|err| anyhow::anyhow!(err))
    }
}

impl GuestSession {
    pub fn shutdown(self) {
        let _ = self.proxy.shutdown_handle().send(true);
        self.connection.close(0u8.into(), b"closed");
    }
}
