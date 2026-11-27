use std::time::Duration;

use iroh::{EndpointId, SecretKey};
use iroh_dns::pkarr::{SignedPacket, Timestamp};
use zeroize::Zeroizing;

use super::board_kdf::hkdf_sha256;
use crate::storage::helper_prefs;

pub const BOARD_NAME: &str = "_popstall";
pub const BOOTSTRAP_LIMIT: usize = 12;

/// Public half of the retired bulletin key. It is kept only as a read-only
/// compatibility anchor; the corresponding secret is no longer in the source.
pub const BOARD_PUBLIC_Z32: &str = "wzkb9pgmqi4xhrjnb1o9kmajspw5jutthroiiarohrw8oywkxnmo";

const DEFAULT_RELAYS: &[&str] = &[
    "https://dns.iroh.link/pkarr",
    "https://staging-dns.iroh.link/pkarr",
];
const DERIVE_INFO: &[u8] = b"popstall/board/publish/v1";
const DERIVE_SALT: &[u8] = b"popstall/board/salt/v1";
const ENV_SECRET: &str = "POPSTALL_BOARD_SECRET";
const ENV_RELAYS: &str = "POPSTALL_PKARR_RELAYS";
const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone, Debug, Default)]
pub struct BootstrapResult {
    pub ids: Vec<EndpointId>,
    pub relay_used: Option<String>,
    pub degraded: bool,
    pub observed: Option<Timestamp>,
}

/// Returns the publish secret, or `None` when publishing must stay read-only.
///
/// An explicit hex override wins. A malformed override disables publishing
/// instead of silently falling back to a different identity.
pub fn board_secret(endpoint_secret: &SecretKey) -> Option<SecretKey> {
    if let Ok(raw) = std::env::var(ENV_SECRET) {
        let bytes = decode_hex32(&raw)?;
        return Some(SecretKey::from_bytes(&*bytes));
    }
    let ikm = Zeroizing::new(endpoint_secret.to_bytes());
    let mut okm = Zeroizing::new([0u8; 32]);
    hkdf_sha256(&*ikm, DERIVE_SALT, DERIVE_INFO, &mut okm);
    Some(SecretKey::from_bytes(&*okm))
}

fn board_publics(endpoint_secret: &SecretKey) -> Vec<EndpointId> {
    let mut publics = Vec::new();
    if let Some(secret) = board_secret(endpoint_secret) {
        publics.push(secret.public());
    }
    if let Some(public) = legacy_public()
        && !publics.contains(&public)
    {
        publics.push(public);
    }
    publics
}

fn legacy_public() -> Option<EndpointId> {
    EndpointId::from_z32(BOARD_PUBLIC_Z32).ok()
}

pub fn relay_urls() -> Vec<String> {
    if let Ok(raw) = std::env::var(ENV_RELAYS) {
        let urls = split_relays(&raw);
        if !urls.is_empty() {
            return urls;
        }
    }
    let cached = helper_prefs::load_board_cache().relay_urls;
    if !cached.is_empty() {
        let urls = split_relays(&cached.join(","));
        if !urls.is_empty() {
            return urls;
        }
    }
    DEFAULT_RELAYS
        .iter()
        .map(|url| (*url).to_string())
        .collect()
}

fn split_relays(raw: &str) -> Vec<String> {
    raw.split([',', '\n', ' '])
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
        .collect()
}

fn join_relay(relay: &str, public: &EndpointId) -> String {
    format!("{}/{}", relay.trim_end_matches('/'), public.to_z32())
}

pub fn merge_bootstrap(
    self_id: EndpointId,
    existing: &[EndpointId],
    limit: usize,
) -> Vec<EndpointId> {
    let mut merged = Vec::with_capacity(limit.min(existing.len() + 1));
    if limit == 0 {
        return merged;
    }
    merged.push(self_id);
    for id in existing {
        if *id == self_id || merged.contains(id) {
            continue;
        }
        merged.push(*id);
        if merged.len() == limit {
            break;
        }
    }
    merged
}

pub fn same_members(left: &[EndpointId], right: &[EndpointId]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort();
    right.sort();
    left == right
}

pub async fn fetch_bootstrap(
    client: &reqwest::Client,
    endpoint_secret: &SecretKey,
) -> BootstrapResult {
    let mut result = BootstrapResult::default();
    let relays = relay_urls();
    for public in board_publics(endpoint_secret) {
        match no_dht(client, &public, &relays).await {
            Some((packet, relay)) => {
                result.ids.extend(parse_ids(&packet_values(&packet)));
                result.relay_used = Some(relay);
                result.observed = Some(packet.timestamp());
                helper_prefs::cache_board_packet(
                    &public.to_z32(),
                    &packet.to_relay_payload(),
                    packet.timestamp().as_micros(),
                );
            }
            None => {
                if let Some(packet) = cache_only(&public) {
                    result.ids.extend(parse_ids(&packet_values(&packet)));
                    result.observed = Some(packet.timestamp());
                    result.degraded = true;
                }
            }
        }
    }
    let mut seen = Vec::new();
    result.ids.retain(|id| {
        if seen.contains(id) {
            false
        } else {
            seen.push(*id);
            true
        }
    });
    if result.ids.is_empty() {
        result.degraded = true;
    }
    result
}

/// Relay-only lookup: no Mainline DHT client is created or queried.
async fn no_dht(
    client: &reqwest::Client,
    public: &EndpointId,
    relays: &[String],
) -> Option<(SignedPacket, String)> {
    for relay in relays {
        let url = join_relay(relay, public);
        let response = match client.get(&url).send().await {
            Ok(response) => response,
            Err(err) => {
                tracing::debug!(%url, ?err, "plaza bulletin lookup failed");
                continue;
            }
        };
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            continue;
        }
        if !response.status().is_success() {
            tracing::debug!(%url, status = %response.status(), "plaza bulletin lookup was refused");
            continue;
        }
        let payload = match response.bytes().await {
            Ok(payload) => payload,
            Err(err) => {
                tracing::debug!(%url, ?err, "plaza bulletin body failed");
                continue;
            }
        };
        if let Some(packet) = verify_packet(public, &payload) {
            return Some((packet, relay.clone()));
        }
        tracing::debug!(%url, "plaza bulletin failed verification");
    }
    None
}

fn cache_only(public: &EndpointId) -> Option<SignedPacket> {
    let cache = helper_prefs::load_board_cache();
    if cache.cached_public.as_deref() != Some(public.to_z32().as_str()) {
        return None;
    }
    let payload = decode_hex(cache.cached_payload.as_deref()?)?;
    let packet = verify_packet(public, &payload)?;
    let now = Timestamp::now().as_micros();
    let max_age = CACHE_MAX_AGE.as_secs().saturating_mul(1_000_000);
    if !accept_cached_packet(&packet, now, max_age) {
        return None;
    }
    Some(packet)
}

fn verify_packet(public: &EndpointId, payload: &[u8]) -> Option<SignedPacket> {
    SignedPacket::from_relay_payload(public, payload).ok()
}

fn accept_cached_packet(packet: &SignedPacket, now_micros: u64, max_age_micros: u64) -> bool {
    let timestamp = packet.timestamp().as_micros();
    timestamp <= now_micros && now_micros.saturating_sub(timestamp) <= max_age_micros
}

pub async fn publish_bootstrap(
    client: &reqwest::Client,
    endpoint_secret: &SecretKey,
    ids: &[EndpointId],
    observed: Option<Timestamp>,
) -> bool {
    let Some(secret) = board_secret(endpoint_secret) else {
        tracing::warn!("plaza publish key is unavailable; staying read-only");
        return false;
    };
    let Some(packet) = fit_packet_after(&secret, ids, observed) else {
        tracing::warn!("plaza bulletin did not fit into a pkarr packet");
        return false;
    };
    let public = packet.public_key();
    let mut published = false;
    for relay in relay_urls() {
        let url = join_relay(&relay, &public);
        match client
            .put(&url)
            .body(packet.to_relay_payload())
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                tracing::info!(%url, peers = ids.len(), "published plaza bulletin");
                published = true;
                break;
            }
            Ok(response) => {
                tracing::warn!(%url, status = %response.status(), "plaza bulletin was refused");
            }
            Err(err) => tracing::warn!(%url, ?err, "plaza bulletin publish failed"),
        }
    }
    published
}

pub fn fit_packet(secret: &SecretKey, ids: &[EndpointId]) -> Option<SignedPacket> {
    fit_packet_after(secret, ids, None)
}

fn fit_packet_after(
    secret: &SecretKey,
    ids: &[EndpointId],
    observed: Option<Timestamp>,
) -> Option<SignedPacket> {
    let mut count = ids.len();
    while count > 0 {
        let values: Vec<String> = ids.iter().take(count).map(EndpointId::to_string).collect();
        if let Ok(base) = SignedPacket::from_txt_strings(secret, BOARD_NAME, &values, 30)
            && parse_ids(&packet_values(&base)).len() == count
        {
            let minimum = observed
                .map(|timestamp| timestamp.as_micros().saturating_add(1))
                .unwrap_or(0);
            let timestamp = Timestamp::now().as_micros().max(minimum);
            return rebuild_with_timestamp(secret, &base, timestamp);
        }
        count -= 1;
    }
    None
}

fn signed_packet_at(
    secret: &SecretKey,
    ids: &[EndpointId],
    timestamp_micros: u64,
) -> Option<SignedPacket> {
    let values: Vec<String> = ids.iter().map(EndpointId::to_string).collect();
    let base = SignedPacket::from_txt_strings(secret, BOARD_NAME, &values, 30).ok()?;
    rebuild_with_timestamp(secret, &base, timestamp_micros)
}

fn rebuild_with_timestamp(
    secret: &SecretKey,
    base: &SignedPacket,
    timestamp_micros: u64,
) -> Option<SignedPacket> {
    if timestamp_micros == base.timestamp().as_micros() {
        return Some(base.clone());
    }
    let encoded = base.encoded_packet();
    let mut signable = format!("3:seqi{timestamp_micros}e1:v{}:", encoded.len()).into_bytes();
    signable.extend_from_slice(encoded);
    let signature = secret.sign(&signable);
    SignedPacket::from_parts_unchecked(
        secret.public().as_bytes(),
        &signature.to_bytes(),
        Timestamp::from_micros(timestamp_micros),
        encoded,
    )
    .ok()
}

fn packet_values(packet: &SignedPacket) -> Vec<String> {
    packet
        .all_txt_records()
        .into_iter()
        .map(|(_, value)| value)
        .collect()
}

fn parse_ids(values: &[String]) -> Vec<EndpointId> {
    let mut ids = Vec::new();
    for value in values {
        for part in value.split([',', ' ', '\n']) {
            let Ok(id) = part.parse::<EndpointId>() else {
                continue;
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

fn decode_hex32(raw: &str) -> Option<Zeroizing<[u8; 32]>> {
    let raw = raw.trim();
    if raw.len() != 64 {
        return None;
    }
    let mut out = Zeroizing::new([0u8; 32]);
    for (index, pair) in raw.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        out[index] = (high << 4) | low;
    }
    Some(out)
}

fn decode_hex(raw: &str) -> Option<Vec<u8>> {
    if raw.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() / 2);
    for pair in raw.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BOARD_PUBLIC_Z32, BOOTSTRAP_LIMIT, CACHE_MAX_AGE, DEFAULT_RELAYS, accept_cached_packet,
        board_secret, fit_packet, fit_packet_after, legacy_public, merge_bootstrap, packet_values,
        parse_ids, rebuild_with_timestamp, same_members, signed_packet_at, verify_packet,
    };

    #[test]
    fn merge_puts_self_first_and_keeps_strangers() {
        let self_id = iroh::SecretKey::generate().public();
        let other = iroh::SecretKey::generate().public();
        let third = iroh::SecretKey::generate().public();
        let merged = merge_bootstrap(self_id, &[other, self_id, third, other], 2);
        assert_eq!(merged, vec![self_id, other]);
        assert!(!same_members(&[other], &merged));
        assert!(same_members(
            &[other, self_id],
            &merge_bootstrap(self_id, &[other], 12)
        ));
    }

    #[test]
    fn bulletin_packet_round_trips_ids() {
        let secret = iroh::SecretKey::generate();
        let ids: Vec<_> = (0..BOOTSTRAP_LIMIT)
            .map(|_| iroh::SecretKey::generate().public())
            .collect();
        let packet = fit_packet(&secret, &ids).expect("packet");
        let parsed = parse_ids(&packet_values(&packet));
        assert!(
            parsed.len() >= 4,
            "bulletin should hold several endpoint ids, held {}",
            parsed.len()
        );
        assert_eq!(parsed, ids[..parsed.len()]);
    }

    #[test]
    fn publish_key_is_derived_and_domain_separated() {
        let endpoint = iroh::SecretKey::generate();
        let other = iroh::SecretKey::generate();
        let first = board_secret(&endpoint).expect("derived secret");
        let again = board_secret(&endpoint).expect("derived secret");
        let second = board_secret(&other).expect("derived secret");
        assert_eq!(first.to_bytes(), again.to_bytes());
        assert_ne!(first.to_bytes(), endpoint.to_bytes());
        assert_ne!(first.to_bytes(), second.to_bytes());
    }

    #[test]
    fn publish_timestamp_exceeds_observed_seq() {
        let secret = iroh::SecretKey::generate();
        let ids = vec![secret.public()];
        let observed = iroh_dns::pkarr::Timestamp::from_micros(
            iroh_dns::pkarr::Timestamp::now().as_micros() + 10_000,
        );
        let packet = fit_packet_after(&secret, &ids, Some(observed)).expect("packet");
        assert!(packet.timestamp() > observed);
    }

    #[test]
    fn stale_cached_packet_is_accepted() {
        let secret = iroh::SecretKey::generate();
        let ids = vec![secret.public()];
        let now = iroh_dns::pkarr::Timestamp::now().as_micros();
        let stale = now.saturating_sub(60 * 1_000_000);
        let packet = signed_packet_at(&secret, &ids, stale).expect("packet");
        let max_age = CACHE_MAX_AGE.as_secs() * 1_000_000;
        assert!(accept_cached_packet(&packet, now, max_age));
        assert!(!accept_cached_packet(
            &packet,
            stale.saturating_sub(1),
            max_age
        ));
    }

    #[test]
    fn bad_signature_is_rejected() {
        let secret = iroh::SecretKey::generate();
        let ids = vec![secret.public()];
        let packet = signed_packet_at(&secret, &ids, 1_700_000_000_000_000).expect("packet");
        let mut payload = packet.to_relay_payload();
        payload[0] ^= 0x01;
        assert!(verify_packet(&secret.public(), &payload).is_none());
        assert!(verify_packet(&secret.public(), &packet.to_relay_payload()).is_some());
    }

    #[test]
    fn rebuilt_packet_keeps_the_requested_timestamp() {
        let secret = iroh::SecretKey::generate();
        let ids = vec![secret.public()];
        let base = fit_packet(&secret, &ids).expect("packet");
        let later = base.timestamp().as_micros() + 5_000;
        let rebuilt = rebuild_with_timestamp(&secret, &base, later).expect("packet");
        assert_eq!(rebuilt.timestamp().as_micros(), later);
        assert_eq!(parse_ids(&packet_values(&rebuilt)), ids);
    }

    #[test]
    fn legacy_public_key_is_a_read_only_anchor() {
        assert!(legacy_public().is_some());
        assert_eq!(BOARD_PUBLIC_Z32.len(), 52);
        assert!(DEFAULT_RELAYS.contains(&"https://dns.iroh.link/pkarr"));
    }

    #[ignore = "requires a live pkarr relay"]
    #[tokio::test]
    async fn live_relay_fetch_is_ignored_by_default() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(6))
            .build()
            .expect("client");
        let secret = iroh::SecretKey::generate();
        let result = super::fetch_bootstrap(&client, &secret).await;
        assert!(result.ids.len() <= BOOTSTRAP_LIMIT);
    }
}
