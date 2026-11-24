use iroh::{PublicKey, SecretKey, Signature};

pub const CARD_TTL_SECS: u32 = 90;
const MAGIC: &[u8; 4] = b"PSC1";
const VERSION: u8 = 2;
const KIND_OPEN: u8 = 1;
const KIND_BYE: u8 = 2;
const MAX_TITLE: usize = 48;
const MAX_LABEL: usize = 32;
pub const MAX_BLURB_CHARS: usize = 100;
const MAX_BLURB_BYTES: usize = MAX_BLURB_CHARS * 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    Open {
        endpoint_id: PublicKey,
        title: String,
        label: String,
        blurb: String,
        locked: bool,
        timestamp: u64,
        ttl: u32,
    },
    Bye {
        endpoint_id: PublicKey,
    },
}

pub fn encode_open(secret: &SecretKey, title: &str, label: &str, blurb: &str, locked: bool) -> Vec<u8> {
    encode(
        secret,
        KIND_OPEN,
        &clip(title, MAX_TITLE),
        &clip(label, MAX_LABEL),
        &clip_chars(blurb, MAX_BLURB_CHARS),
        CARD_TTL_SECS,
        locked,
    )
}

pub fn encode_bye(secret: &SecretKey) -> Vec<u8> {
    encode(secret, KIND_BYE, "", "", "", CARD_TTL_SECS, false)
}

pub fn decode(bytes: &[u8], now: u64) -> Option<Card> {
    if bytes.len() < 4 + 1 + 1 + 32 + 8 + 4 + 64 || &bytes[..4] != MAGIC {
        return None;
    }
    let signed_len = bytes.len() - 64;
    let signed = &bytes[..signed_len];
    let mut cursor = &bytes[4..signed_len];
    let version = take_u8(&mut cursor)?;
    if version != 1 && version != 2 && version != 3 {
        return None;
    }
    let kind = take_u8(&mut cursor)?;
    let id_bytes = take_array::<32>(&mut cursor)?;
    let endpoint_id = PublicKey::from_bytes(&id_bytes).ok()?;
    let timestamp = u64::from_be_bytes(take_array::<8>(&mut cursor)?);
    let ttl = u32::from_be_bytes(take_array::<4>(&mut cursor)?);
    let title = take_str(&mut cursor, MAX_TITLE)?;
    let label = take_str(&mut cursor, MAX_LABEL)?;
    let blurb = if version >= 2 {
        take_text(&mut cursor, MAX_BLURB_BYTES)?
    } else {
        String::new()
    };
    let locked = if version >= 3 { take_u8(&mut cursor)? == 1 } else { false };
    if !cursor.is_empty() {
        return None;
    }
    let signature = Signature::from_bytes(&bytes[signed_len..].try_into().ok()?);
    endpoint_id.verify(signed, &signature).ok()?;
    if timestamp == 0 || ttl == 0 || timestamp > now.saturating_add(180) {
        return None;
    }
    if timestamp + u64::from(ttl) + 30 < now {
        return None;
    }
    match kind {
        KIND_OPEN => Some(Card::Open {
            endpoint_id,
            title: if title.is_empty() {
                "未命名摊位".to_string()
            } else {
                title
            },
            label,
            blurb,
            locked,
            timestamp,
            ttl,
        }),
        KIND_BYE => Some(Card::Bye { endpoint_id }),
        _ => None,
    }
}

pub fn clip_chars(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    cleaned.trim().chars().take(max_chars).collect()
}

pub fn clip(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let trimmed = cleaned.trim();
    let mut end = trimmed.len().min(max);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_string()
}

fn encode(secret: &SecretKey, kind: u8, title: &str, label: &str, blurb: &str, ttl: u32, locked: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);
    body.extend_from_slice(MAGIC);
    body.push(if locked { 3 } else { VERSION });
    body.push(kind);
    body.extend_from_slice(secret.public().as_bytes());
    body.extend_from_slice(&unix_now().to_be_bytes());
    body.extend_from_slice(&ttl.to_be_bytes());
    push_str(&mut body, title);
    push_str(&mut body, label);
    push_text(&mut body, blurb);
    if locked {
        body.push(1);
    }
    let signature = secret.sign(&body);
    body.extend_from_slice(&signature.to_bytes());
    body
}

fn push_str(body: &mut Vec<u8>, text: &str) {
    let text = clip(text, 255);
    body.push(text.len() as u8);
    body.extend_from_slice(text.as_bytes());
}

fn push_text(body: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    body.extend_from_slice(&len.to_be_bytes());
    body.extend_from_slice(&bytes[..usize::from(len)]);
}

fn take_u8(input: &mut &[u8]) -> Option<u8> {
    let (first, rest) = input.split_first()?;
    *input = rest;
    Some(*first)
}

fn take_array<const N: usize>(input: &mut &[u8]) -> Option<[u8; N]> {
    if input.len() < N {
        return None;
    }
    let mut array = [0u8; N];
    array.copy_from_slice(&input[..N]);
    *input = &input[N..];
    Some(array)
}

fn take_str(input: &mut &[u8], max: usize) -> Option<String> {
    let len = take_u8(input)? as usize;
    take_bytes(input, len, max)
}

fn take_text(input: &mut &[u8], max: usize) -> Option<String> {
    let len = u16::from_be_bytes(take_array(input)?) as usize;
    take_bytes(input, len, max)
}

fn take_bytes(input: &mut &[u8], len: usize, max: usize) -> Option<String> {
    if len > max || input.len() < len {
        return None;
    }
    let text = std::str::from_utf8(&input[..len]).ok()?.to_string();
    *input = &input[len..];
    Some(text)
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode_open, unix_now};

    #[test]
    fn open_card_round_trips_and_rejects_tampering() {
        let secret = iroh::SecretKey::generate();
        let blurb = "夜里的爵士 ".repeat(20);
        let bytes = encode_open(&secret, "海边的图片\n", "图片展厅", &blurb, false);
        let card = decode(&bytes, unix_now()).expect("card");
        match card {
            super::Card::Open {
                title,
                label,
                blurb,
                locked,
                endpoint_id,
                ..
            } => {
                assert_eq!(title, "海边的图片");
                assert_eq!(label, "图片展厅");
                assert!(!locked);
                assert_eq!(blurb.chars().count(), super::MAX_BLURB_CHARS);
                assert!(blurb.starts_with("夜里的爵士"));
                assert_eq!(endpoint_id, secret.public());
            }
            super::Card::Bye { .. } => panic!("expected an open card"),
        }

        let mut tampered = bytes.clone();
        tampered[51] ^= 0x1;
        assert!(decode(&tampered, unix_now()).is_none());

        let locked_bytes = encode_open(&secret, "南窗", "图片展厅", "只给有密码的人", true);
        match decode(&locked_bytes, unix_now()).expect("locked card") {
            super::Card::Open { locked, blurb, .. } => {
                assert!(locked);
                assert_eq!(blurb, "只给有密码的人");
            }
            super::Card::Bye { .. } => panic!("expected an open card"),
        }
    }
}
