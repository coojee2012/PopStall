use iroh::{EndpointId, SecretKey};
use iroh_dns::pkarr::SignedPacket;

/// Shared plaza bulletin. The secret is public on purpose: every running
/// client merges its own endpoint id into this pkarr record. A stranger only
/// needs one live id to enter the gossip swarm; the record is not the plaza
/// itself and does not carry page bytes.
const BOARD_SECRET: [u8; 32] = [
    0x73, 0xf2, 0x9f, 0x6f, 0xff, 0x5f, 0x99, 0xae, 0x01, 0x28, 0x31, 0xdf, 0xa5, 0x9d, 0xc6, 0x44,
    0xc2, 0x61, 0x52, 0x1d, 0x68, 0x0b, 0x2a, 0x49, 0xc3, 0xaf, 0x76, 0x78, 0x6e, 0xe5, 0x1e, 0x40,
];
const BOARD_NAME: &str = "_popstall";
const RELAY: &str = "https://dns.iroh.link/pkarr";
pub const BOOTSTRAP_LIMIT: usize = 12;

pub fn board_secret() -> SecretKey {
    SecretKey::from_bytes(&BOARD_SECRET)
}

pub fn merge_bootstrap(self_id: EndpointId, existing: &[EndpointId], limit: usize) -> Vec<EndpointId> {
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

pub async fn fetch_bootstrap(client: &reqwest::Client) -> Vec<EndpointId> {
    let public = board_secret().public();
    let url = format!("{RELAY}/{}", public.to_z32());
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(?err, "plaza bulletin lookup failed");
            return Vec::new();
        }
    };
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Vec::new();
    }
    if !response.status().is_success() {
        tracing::debug!(status = %response.status(), "plaza bulletin lookup was refused");
        return Vec::new();
    }
    let payload = match response.bytes().await {
        Ok(payload) => payload,
        Err(err) => {
            tracing::debug!(?err, "plaza bulletin body failed");
            return Vec::new();
        }
    };
    let packet = match SignedPacket::from_relay_payload(&public, &payload) {
        Ok(packet) => packet,
        Err(err) => {
            tracing::debug!(?err, "plaza bulletin failed verification");
            return Vec::new();
        }
    };
    parse_ids(&packet_values(&packet))
}

pub async fn publish_bootstrap(client: &reqwest::Client, ids: &[EndpointId]) {
    let Some(packet) = fit_packet(ids) else {
        tracing::warn!("plaza bulletin did not fit into a pkarr packet");
        return;
    };
    let url = format!("{RELAY}/{}", packet.public_key().to_z32());
    match client.put(url).body(packet.to_relay_payload()).send().await {
        Ok(response) if response.status().is_success() => {
            tracing::info!(peers = ids.len(), "published plaza bulletin");
        }
        Ok(response) => {
            tracing::warn!(status = %response.status(), "plaza bulletin was refused");
        }
        Err(err) => tracing::warn!(?err, "plaza bulletin publish failed"),
    }
}

fn fit_packet(ids: &[EndpointId]) -> Option<SignedPacket> {
    let mut count = ids.len();
    while count > 0 {
        let values: Vec<String> = ids.iter().take(count).map(EndpointId::to_string).collect();
        if let Ok(packet) = SignedPacket::from_txt_strings(&board_secret(), BOARD_NAME, &values, 30)
            && parse_ids(&packet_values(&packet)).len() == count
        {
            return Some(packet);
        }
        count -= 1;
    }
    None
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

#[cfg(test)]
mod tests {
    use super::{fit_packet, merge_bootstrap, packet_values, parse_ids, same_members, BOOTSTRAP_LIMIT};

    #[test]
    fn merge_puts_self_first_and_keeps_strangers() {
        let self_id = iroh::SecretKey::generate().public();
        let other = iroh::SecretKey::generate().public();
        let third = iroh::SecretKey::generate().public();
        let merged = merge_bootstrap(self_id, &[other, self_id, third, other], 2);
        assert_eq!(merged, vec![self_id, other]);
        assert!(!same_members(&[other], &merged));
        assert!(same_members(&[other, self_id], &merge_bootstrap(self_id, &[other], 12)));
    }

    #[test]
    fn bulletin_packet_round_trips_ids() {
        let ids: Vec<_> = (0..BOOTSTRAP_LIMIT)
            .map(|_| iroh::SecretKey::generate().public())
            .collect();
        let packet = fit_packet(&ids).expect("packet");
        let parsed = parse_ids(&packet_values(&packet));
        assert!(
            parsed.len() >= 4,
            "bulletin should hold several endpoint ids, held {}",
            parsed.len()
        );
        assert_eq!(parsed, ids[..parsed.len()]);
    }
}
