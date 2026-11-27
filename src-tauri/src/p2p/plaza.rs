use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use iroh::{Endpoint, EndpointId, SecretKey};
use iroh_gossip::{Event, Gossip, GossipSender, JoinOptions, TopicId};
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use serde::Serialize;
use tokio::sync::{Mutex, watch};

use super::board::{self, BOOTSTRAP_LIMIT};
use super::card::{self, Card};
use crate::storage::helper_prefs;

const TOPIC: TopicId = TopicId::from_bytes([
    0xd9, 0x00, 0x3d, 0x5f, 0xc0, 0x88, 0xb0, 0x33, 0x24, 0xaf, 0xd9, 0xa9, 0x67, 0x27, 0xe6, 0xd1,
    0x6d, 0x41, 0xe9, 0x0f, 0x35, 0xeb, 0x45, 0x56, 0x58, 0x3b, 0x3c, 0xf2, 0x72, 0xd3, 0x73, 0xb7,
]);
const TICK: Duration = Duration::from_secs(20);
const MAX_STALLS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Announce {
    Idle,
    Open {
        title: String,
        label: String,
        blurb: String,
        locked: bool,
    },
}

#[derive(Clone, Debug)]
struct LiveStall {
    title: String,
    label: String,
    blurb: String,
    locked: bool,
    timestamp: u64,
    expires: Instant,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlazaStall {
    pub endpoint_id: String,
    pub title: String,
    pub label: String,
    pub blurb: String,
    pub locked: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlazaSnapshot {
    pub stalls: Vec<PlazaStall>,
    pub neighbors: usize,
    pub degraded: bool,
    pub relay_used: Option<String>,
}

impl Default for PlazaSnapshot {
    fn default() -> Self {
        Self {
            stalls: Vec::new(),
            neighbors: 0,
            degraded: false,
            relay_used: None,
        }
    }
}

#[derive(Clone)]
pub struct Plaza {
    announce: watch::Sender<Announce>,
    stalls: Arc<Mutex<HashMap<EndpointId, LiveStall>>>,
    neighbors: Arc<AtomicUsize>,
    degraded: Arc<AtomicBool>,
    relay_used: Arc<Mutex<Option<String>>>,
}

impl Plaza {
    pub fn announce(&self, title: &str, label: &str, blurb: &str, locked: bool) {
        let _ = self.announce.send(Announce::Open {
            title: card::clip(title, 48),
            label: card::clip(label, 32),
            blurb: card::clip_chars(blurb, card::MAX_BLURB_CHARS),
            locked,
        });
    }

    pub fn retract(&self) {
        let _ = self.announce.send(Announce::Idle);
    }

    pub async fn peer_ids(&self, limit: usize) -> Vec<EndpointId> {
        let mut ids: Vec<EndpointId> = self.stalls.lock().await.keys().copied().collect();
        ids.truncate(limit);
        ids
    }

    pub async fn snapshot(&self, self_id: EndpointId) -> PlazaSnapshot {
        let mut stalls = self.stalls.lock().await;
        let now = Instant::now();
        stalls.retain(|_, stall| stall.expires > now);
        let mut listed: Vec<PlazaStall> = stalls
            .iter()
            .filter(|(id, _)| **id != self_id)
            .map(|(id, stall)| PlazaStall {
                endpoint_id: id.to_string(),
                title: stall.title.clone(),
                label: stall.label.clone(),
                blurb: stall.blurb.clone(),
                locked: stall.locked,
            })
            .collect();
        listed.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then(left.endpoint_id.cmp(&right.endpoint_id))
        });
        PlazaSnapshot {
            stalls: listed,
            neighbors: self.neighbors.load(Ordering::Relaxed),
            degraded: self.degraded.load(Ordering::Relaxed),
            relay_used: self.relay_used.lock().await.clone(),
        }
    }
}

pub fn attach_mdns(endpoint: &Endpoint) -> Option<MdnsAddressLookup> {
    let service = match MdnsAddressLookup::builder().build(endpoint.id()) {
        Ok(service) => service,
        Err(err) => {
            tracing::warn!(?err, "local discovery did not start");
            return None;
        }
    };
    match endpoint.address_lookup() {
        Ok(lookup) => lookup.add(service.clone()),
        Err(err) => {
            tracing::warn!(?err, "local discovery was not registered");
            return None;
        }
    }
    Some(service)
}

pub fn start(endpoint: Endpoint, gossip: Gossip, mdns: Option<MdnsAddressLookup>) -> Plaza {
    let (announce, announce_rx) = watch::channel(Announce::Idle);
    let stalls = Arc::new(Mutex::new(HashMap::new()));
    let neighbors = Arc::new(AtomicUsize::new(0));
    let degraded = Arc::new(AtomicBool::new(false));
    let relay_used = Arc::new(Mutex::new(None));
    let local_peers = Arc::new(Mutex::new(Vec::new()));
    let heard = Arc::new(Mutex::new(Vec::new()));
    if let Some(mdns) = mdns {
        let peers = local_peers.clone();
        let self_id = endpoint.id();
        tokio::spawn(async move {
            let mut events = mdns.subscribe().await;
            while let Some(event) = events.next().await {
                match event {
                    DiscoveryEvent::Discovered { endpoint_info, .. } => {
                        remember(&peers, self_id, endpoint_info.endpoint_id).await;
                    }
                    DiscoveryEvent::Expired { endpoint_id } => {
                        peers.lock().await.retain(|id| *id != endpoint_id);
                    }
                    _ => {}
                }
            }
        });
    }
    let plaza = Plaza {
        announce,
        stalls: stalls.clone(),
        neighbors: neighbors.clone(),
        degraded: degraded.clone(),
        relay_used: relay_used.clone(),
    };
    let secret = endpoint.secret_key().clone();
    tokio::spawn(async move {
        plaza_loop(
            endpoint,
            gossip,
            secret,
            announce_rx,
            stalls,
            neighbors,
            degraded,
            relay_used,
            local_peers,
            heard,
        )
        .await;
    });
    plaza
}

async fn plaza_loop(
    endpoint: Endpoint,
    gossip: Gossip,
    secret: SecretKey,
    mut announce: watch::Receiver<Announce>,
    stalls: Arc<Mutex<HashMap<EndpointId, LiveStall>>>,
    neighbors: Arc<AtomicUsize>,
    degraded: Arc<AtomicBool>,
    relay_used: Arc<Mutex<Option<String>>>,
    local_peers: Arc<Mutex<Vec<EndpointId>>>,
    heard: Arc<Mutex<Vec<EndpointId>>>,
) {
    let _ = wait_for_relay(&endpoint, Duration::from_secs(15)).await;
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(?err, "plaza client failed");
            return;
        }
    };
    loop {
        if let Err(err) = run_session(
            &gossip,
            &secret,
            &client,
            &mut announce,
            &stalls,
            &neighbors,
            &degraded,
            &relay_used,
            &local_peers,
            &heard,
        )
        .await
        {
            tracing::warn!(?err, "plaza session stopped");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_session(
    gossip: &Gossip,
    secret: &SecretKey,
    client: &reqwest::Client,
    announce: &mut watch::Receiver<Announce>,
    stalls: &Mutex<HashMap<EndpointId, LiveStall>>,
    neighbors: &AtomicUsize,
    degraded: &AtomicBool,
    relay_used: &Mutex<Option<String>>,
    local_peers: &Mutex<Vec<EndpointId>>,
    heard: &Mutex<Vec<EndpointId>>,
) -> anyhow::Result<()> {
    let options = JoinOptions::with_bootstrap(std::iter::empty()).with_subscription_capacity(256);
    let topic = gossip
        .subscribe_with_opts(TOPIC, options)
        .await
        .map_err(|err| anyhow::anyhow!("加入广场失败: {err:?}"))?;
    let (mut sender, mut receiver) = topic.split();
    neighbors.store(0, Ordering::Relaxed);
    let self_id = secret.public();
    let mut published = false;
    publish_current(
        &mut sender,
        secret,
        current_announce(announce),
        &mut published,
    )
    .await;

    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    refresh_links(
        &mut sender,
        client,
        secret,
        self_id,
        degraded,
        relay_used,
        local_peers,
        heard,
    )
    .await;

    loop {
        tokio::select! {
                event = receiver.next() => {
                    let Some(event) = event else {
                        return Ok(());
                    };
                    match event {
                        Ok(Event::Received(message)) => {
                            if let Some(id) = ingest(stalls, self_id, &message.content).await {
                                remember(heard, self_id, id).await;
                            }
                        }
                        Ok(Event::NeighborUp(_)) => {
                            neighbors.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(Event::NeighborDown(_)) => {
                            let _ = neighbors.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                                Some(value.saturating_sub(1))
                            });
                        }
                        Ok(Event::Lagged) => tracing::debug!("plaza missed a burst of messages"),
                        Ok(_) => {}
                        Err(err) => tracing::debug!(?err, "plaza event failed"),
                    }
                }
                changed = announce.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    publish_current(&mut sender, secret, current_announce(announce), &mut published).await;
                }
                _ = ticker.tick() => {
                    expire(stalls).await;
                    refresh_links(
            &mut sender,
            client,
            secret,
            self_id,
            degraded,
            relay_used,
            local_peers,
            heard,
        )
        .await;
                    publish_current(&mut sender, secret, current_announce(announce), &mut published).await;
                }
            }
    }
}

async fn refresh_links(
    sender: &mut GossipSender,
    client: &reqwest::Client,
    secret: &SecretKey,
    self_id: EndpointId,
    degraded: &AtomicBool,
    relay_used: &Mutex<Option<String>>,
    local_peers: &Mutex<Vec<EndpointId>>,
    heard: &Mutex<Vec<EndpointId>>,
) {
    let fetched = board::fetch_bootstrap(client, secret).await;
    *relay_used.lock().await = fetched.relay_used.clone();
    degraded.store(fetched.degraded, Ordering::Relaxed);
    let merged = board::merge_bootstrap(self_id, &fetched.ids, BOOTSTRAP_LIMIT);
    helper_prefs::remember_bootstrap_peers(&merged);
    if !board::same_members(&fetched.ids, &merged)
        && !board::publish_bootstrap(client, secret, &merged, fetched.observed).await
    {
        degraded.store(true, Ordering::Relaxed);
    }
    let mut dial = Vec::new();
    push_unique(&mut dial, &merged, self_id, 8);
    push_unique(&mut dial, &local_peers.lock().await.clone(), self_id, 8);
    push_unique(&mut dial, &heard.lock().await.clone(), self_id, 8);
    if dial.is_empty() {
        return;
    }
    if let Err(err) = sender.join_peers(dial).await {
        tracing::debug!(?err, "plaza join failed");
    }
}

fn push_unique(into: &mut Vec<EndpointId>, ids: &[EndpointId], self_id: EndpointId, limit: usize) {
    let mut added = 0;
    for id in ids {
        if *id == self_id || into.contains(id) {
            continue;
        }
        into.push(*id);
        added += 1;
        if added == limit {
            break;
        }
    }
}

fn current_announce(announce: &watch::Receiver<Announce>) -> Announce {
    announce.borrow().clone()
}

async fn publish_current(
    sender: &mut GossipSender,
    secret: &SecretKey,
    announce: Announce,
    published: &mut bool,
) {
    let bytes = match &announce {
        Announce::Open {
            title,
            label,
            blurb,
            locked,
        } => {
            *published = true;
            card::encode_open(secret, title, label, blurb, *locked)
        }
        Announce::Idle if *published => {
            *published = false;
            card::encode_bye(secret)
        }
        Announce::Idle => return,
    };
    if let Err(err) = sender.broadcast(Bytes::from(bytes)).await {
        tracing::debug!(?err, "plaza broadcast failed");
    }
}

async fn ingest(
    stalls: &Mutex<HashMap<EndpointId, LiveStall>>,
    self_id: EndpointId,
    bytes: &[u8],
) -> Option<EndpointId> {
    let card = card::decode(bytes, card::unix_now())?;
    let endpoint_id = match &card {
        Card::Open { endpoint_id, .. } | Card::Bye { endpoint_id } => *endpoint_id,
    };
    if endpoint_id == self_id {
        return None;
    }
    let mut stalls = stalls.lock().await;
    match card {
        Card::Bye { .. } => {
            stalls.remove(&endpoint_id);
            Some(endpoint_id)
        }
        Card::Open {
            title,
            label,
            blurb,
            locked,
            timestamp,
            ttl,
            ..
        } => {
            if let Some(existing) = stalls.get(&endpoint_id)
                && existing.timestamp > timestamp
            {
                return Some(endpoint_id);
            }
            let is_new = !stalls.contains_key(&endpoint_id);
            stalls.insert(
                endpoint_id,
                LiveStall {
                    title: title.clone(),
                    label,
                    blurb,
                    locked,
                    timestamp,
                    expires: Instant::now() + Duration::from_secs(u64::from(ttl.clamp(30, 180))),
                },
            );
            if stalls.len() > MAX_STALLS {
                if let Some(oldest) = stalls
                    .iter()
                    .min_by_key(|(_, stall)| stall.expires)
                    .map(|(id, _)| *id)
                {
                    stalls.remove(&oldest);
                }
            }
            if is_new {
                tracing::info!(%endpoint_id, %title, locked, "plaza saw a stall");
            }
            Some(endpoint_id)
        }
    }
}

async fn expire(stalls: &Mutex<HashMap<EndpointId, LiveStall>>) {
    let now = Instant::now();
    stalls.lock().await.retain(|_, stall| stall.expires > now);
}

async fn remember(peers: &Mutex<Vec<EndpointId>>, self_id: EndpointId, id: EndpointId) {
    if id == self_id {
        return;
    }
    let mut peers = peers.lock().await;
    if !peers.contains(&id) {
        peers.push(id);
    }
    if peers.len() > 32 {
        peers.remove(0);
    }
}

async fn wait_for_relay(endpoint: &Endpoint, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if endpoint.addr().relay_urls().next().is_some() {
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
    use super::*;

    fn test_plaza(degraded: bool, relay: Option<&str>) -> Plaza {
        Plaza {
            announce: watch::channel(Announce::Idle).0,
            stalls: Arc::new(Mutex::new(HashMap::new())),
            neighbors: Arc::new(AtomicUsize::new(0)),
            degraded: Arc::new(AtomicBool::new(degraded)),
            relay_used: Arc::new(Mutex::new(relay.map(str::to_string))),
        }
    }

    #[tokio::test]
    async fn snapshot_default_has_no_degradation() {
        let snapshot = PlazaSnapshot::default();
        assert!(!snapshot.degraded);
        assert_eq!(snapshot.relay_used, None);
        assert!(snapshot.stalls.is_empty());
        assert_eq!(snapshot.neighbors, 0);
    }

    #[tokio::test]
    async fn snapshot_exposes_relay_and_degraded_state() {
        let plaza = test_plaza(true, Some("https://dns.iroh.link/pkarr"));
        let snapshot = plaza.snapshot(iroh::SecretKey::generate().public()).await;
        assert!(snapshot.degraded);
        assert_eq!(
            snapshot.relay_used.as_deref(),
            Some("https://dns.iroh.link/pkarr")
        );
    }

    #[test]
    fn push_unique_skips_self_and_duplicates() {
        let self_id = iroh::SecretKey::generate().public();
        let other = iroh::SecretKey::generate().public();
        let third = iroh::SecretKey::generate().public();
        let mut dial = Vec::new();
        push_unique(&mut dial, &[self_id, other, other, third], self_id, 8);
        assert_eq!(dial, vec![other, third]);
    }
}
