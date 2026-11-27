//! Opt-in network harness for external DHT, relay, and UPnP/PCP checks.
//!
//! Every test here is ignored by default because it needs live network
//! infrastructure. Run them explicitly with `--ignored` on a machine that has
//! the relevant network access; no test performs network I/O by default.

use std::time::Duration;

use futures_util::StreamExt;
use iroh::address_lookup::AddressLookup;
use iroh::{Endpoint, EndpointAddr, TransportAddr, endpoint::presets};
use iroh_mainline_address_lookup::DhtAddressLookup;

const ALPN: &[u8] = b"popstall/network-test/1";

#[tokio::test]
#[ignore = "requires a live DHT bootstrap node"]
async fn dht_bootstrap_reaches_live_nodes() {
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .address_lookup(DhtAddressLookup::builder())
        .bind()
        .await
        .expect("N0 endpoint must bind");
    endpoint.online().await;

    let lookup = DhtAddressLookup::builder()
        .no_publish()
        .build()
        .expect("read-only DHT lookup must build");
    let mut results = lookup
        .resolve(endpoint.id())
        .expect("DHT lookup must return a stream");
    let item = tokio::time::timeout(Duration::from_secs(90), results.next())
        .await
        .expect("DHT lookup timed out")
        .expect("DHT lookup stream ended")
        .expect("DHT lookup failed");
    assert_eq!(item.endpoint_id(), endpoint.id());
    assert!(
        item.to_endpoint_addr().relay_urls().next().is_some(),
        "published DHT record must contain a relay address: {item:?}"
    );
    endpoint.close().await;
}

#[tokio::test]
#[ignore = "requires a live N0 relay"]
async fn relay_fallback_uses_live_relay() {
    let server = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("server endpoint must bind");
    server.online().await;
    let server_id = server.id();
    let relay_url = server
        .addr()
        .relay_urls()
        .next()
        .cloned()
        .expect("online endpoint must have a relay URL");

    let client = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .expect("client endpoint must bind");
    client.online().await;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("server endpoint closed");
        let conn = incoming.await.expect("relay-only connection failed");
        let (mut send, mut recv) = conn.accept_bi().await.expect("accept bi failed");
        let data = recv.read_to_end(64).await.expect("read failed");
        assert_eq!(data, b"relay");
        send.write_all(&data).await.expect("write failed");
        send.finish().expect("finish failed");
        conn.closed().await;
    });

    let relay_only = EndpointAddr::from_parts(server_id, [TransportAddr::Relay(relay_url)]);
    let conn = tokio::time::timeout(Duration::from_secs(60), client.connect(relay_only, ALPN))
        .await
        .expect("relay-only connection timed out")
        .expect("relay-only connection failed");
    let (mut send, mut recv) = conn.open_bi().await.expect("open bi failed");
    send.write_all(b"relay").await.expect("write failed");
    send.finish().expect("finish failed");
    let echoed = recv.read_to_end(64).await.expect("read failed");
    assert_eq!(echoed, b"relay");

    client.close().await;
    server_task.await.expect("server task panicked");
}

#[tokio::test]
#[ignore = "requires a router with UPnP/PCP enabled"]
async fn port_mapping_uses_live_router() {
    let client = portmapper::Client::new(portmapper::Config::default());
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
