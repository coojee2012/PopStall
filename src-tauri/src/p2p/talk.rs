use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, Signature};
use iroh_gossip::{Event, Gossip, JoinOptions, TopicId};
use serde::Serialize;
use tokio::sync::{Mutex, mpsc};

use super::plaza::Plaza;

pub const ALPN: &[u8] = b"popstall/talk/1";
const TOPIC: TopicId = TopicId::from_bytes([
    0xbc, 0x6b, 0x08, 0x58, 0xbe, 0x9f, 0xdc, 0x23, 0x08, 0x00, 0x64, 0xca, 0x3c, 0xf8, 0xae, 0x08,
    0xfa, 0xd7, 0x45, 0x20, 0xdb, 0xb9, 0xb4, 0xd9, 0x15, 0xdd, 0xcd, 0x22, 0x71, 0x70, 0x64, 0xc1,
]);
const MAGIC: &[u8; 4] = b"PTK1";
const MAX_LINES: usize = 300;
const MAX_CHARS: usize = 200;
const MAX_TEXT_BYTES: usize = MAX_CHARS * 4;
const MAX_NAME_CHARS: usize = 12;
const MAX_NAME_BYTES: usize = MAX_NAME_CHARS * 4;
const MAX_FRAME: usize = 4 + 1 + 32 + 16 + 8 + 2 + MAX_TEXT_BYTES + 1 + MAX_NAME_BYTES + 64;
const SYNC_LIMIT: usize = 2 + MAX_LINES * (4 + MAX_FRAME);
const SEND_GAP: Duration = Duration::from_millis(700);

enum TalkCmd {
    Send(Vec<u8>),
    Join(Vec<EndpointId>),
}

struct Line {
    id: [u8; 16],
    sender: EndpointId,
    lamport: u64,
    text: String,
    name: String,
    raw: Vec<u8>,
}

struct Room {
    lines: Vec<Line>,
    seen: HashSet<[u8; 16]>,
    lamport: u64,
    seq: u64,
    last_sent: Option<Instant>,
}

impl Room {
    fn compose(
        &mut self,
        secret: &SecretKey,
        text: &str,
        name: &str,
    ) -> Result<Vec<u8>, &'static str> {
        if let Some(previous) = self.last_sent
            && previous.elapsed() < SEND_GAP
        {
            return Err("稍等一下再发");
        }
        let text = normalize(text)?;
        let name = normalize_name(name)?;
        self.lamport += 1;
        self.seq += 1;
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&self.seq.to_be_bytes());
        id[8..].copy_from_slice(&secret.public().as_bytes()[..8]);
        let raw = encode(secret, id, self.lamport, &text, &name);
        self.insert(Line {
            id,
            sender: secret.public(),
            lamport: self.lamport,
            text,
            name,
            raw: raw.clone(),
        });
        self.last_sent = Some(Instant::now());
        Ok(raw)
    }

    fn ingest_frame(&mut self, bytes: &[u8]) -> bool {
        let Some(line) = decode(bytes) else {
            return false;
        };
        self.observe(line.lamport);
        self.insert(line)
    }

    fn import_blob(&mut self, bytes: &[u8]) -> usize {
        if bytes.len() < 2 {
            return 0;
        }
        let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        let mut cursor = 2;
        let mut accepted = 0;
        for _ in 0..count.min(MAX_LINES) {
            if cursor + 4 > bytes.len() {
                break;
            }
            let len =
                u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap_or([0; 4])) as usize;
            cursor += 4;
            if len == 0 || len > MAX_FRAME || cursor + len > bytes.len() {
                break;
            }
            if self.ingest_frame(&bytes[cursor..cursor + len]) {
                accepted += 1;
            }
            cursor += len;
        }
        accepted
    }

    fn export_blob(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let count = u16::try_from(self.lines.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&count.to_be_bytes());
        for line in &self.lines {
            let len = u32::try_from(line.raw.len()).unwrap_or(0);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(&line.raw);
        }
        out
    }

    fn recent_senders(&self, limit: usize) -> Vec<EndpointId> {
        let mut ids = Vec::new();
        for line in self.lines.iter().rev() {
            if ids.contains(&line.sender) {
                continue;
            }
            ids.push(line.sender);
            if ids.len() == limit {
                break;
            }
        }
        ids
    }

    fn observe(&mut self, lamport: u64) {
        if lamport > self.lamport {
            self.lamport = lamport;
        }
    }

    fn insert(&mut self, line: Line) -> bool {
        if !self.seen.insert(line.id) {
            return false;
        }
        let key = sort_key(&line);
        let index = self
            .lines
            .partition_point(|existing| sort_key(existing) < key);
        self.lines.insert(index, line);
        while self.lines.len() > MAX_LINES {
            let removed = self.lines.remove(0);
            self.seen.remove(&removed.id);
        }
        true
    }
}

fn sort_key(line: &Line) -> (u64, [u8; 32], [u8; 16]) {
    (line.lamport, *line.sender.as_bytes(), line.id)
}

fn normalize(text: &str) -> Result<String, &'static str> {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err("先写一句话");
    }
    if trimmed.chars().count() > MAX_CHARS {
        return Err("一句话最多 200 字");
    }
    Ok(trimmed.to_string())
}

fn normalize_name(name: &str) -> Result<String, &'static str> {
    let cleaned: String = name
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err("名字最多 12 个字");
    }
    Ok(trimmed.to_string())
}

fn encode(secret: &SecretKey, id: [u8; 16], lamport: u64, text: &str, name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(160 + text.len() + name.len());
    body.extend_from_slice(MAGIC);
    body.push(2);
    body.extend_from_slice(secret.public().as_bytes());
    body.extend_from_slice(&id);
    body.extend_from_slice(&lamport.to_be_bytes());
    let bytes = text.as_bytes();
    body.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    body.extend_from_slice(bytes);
    let name_bytes = name.as_bytes();
    body.push(name_bytes.len() as u8);
    body.extend_from_slice(name_bytes);
    let signature = secret.sign(&body);
    body.extend_from_slice(&signature.to_bytes());
    body
}

fn decode(bytes: &[u8]) -> Option<Line> {
    if bytes.len() < 4 + 1 + 32 + 16 + 8 + 2 + 64 || &bytes[..4] != MAGIC {
        return None;
    }
    let signed_len = bytes.len() - 64;
    let signed = &bytes[..signed_len];
    let mut cursor = &bytes[4..signed_len];
    let version = take_u8(&mut cursor)?;
    if version != 1 && version != 2 {
        return None;
    }
    let sender = EndpointId::from_bytes(&take_array::<32>(&mut cursor)?).ok()?;
    let id = take_array::<16>(&mut cursor)?;
    let lamport = u64::from_be_bytes(take_array::<8>(&mut cursor)?);
    let text_len = u16::from_be_bytes(take_array::<2>(&mut cursor)?) as usize;
    if text_len == 0 || text_len > MAX_TEXT_BYTES || cursor.len() < text_len {
        return None;
    }
    let text = std::str::from_utf8(&cursor[..text_len]).ok()?.to_string();
    cursor = &cursor[text_len..];
    if text.chars().any(|ch| ch.is_control())
        || text.chars().count() > MAX_CHARS
        || text.trim().is_empty()
    {
        return None;
    }
    let name = if version >= 2 {
        let name_len = take_u8(&mut cursor)? as usize;
        if name_len > MAX_NAME_BYTES || cursor.len() != name_len {
            return None;
        }
        let name = std::str::from_utf8(cursor).ok()?.to_string();
        if name.chars().any(|ch| ch.is_control())
            || name.chars().count() > MAX_NAME_CHARS
            || name != name.trim()
        {
            return None;
        }
        name
    } else if cursor.is_empty() {
        String::new()
    } else {
        return None;
    };
    let signature = Signature::from_bytes(&bytes[signed_len..].try_into().ok()?);
    sender.verify(signed, &signature).ok()?;
    if lamport == 0 {
        return None;
    }
    Some(Line {
        id,
        sender,
        lamport,
        text,
        name,
        raw: bytes.to_vec(),
    })
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

#[derive(Clone)]
pub struct TalkHandler {
    room: Arc<Mutex<Room>>,
}

impl std::fmt::Debug for TalkHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TalkHandler")
            .finish_non_exhaustive()
    }
}

impl ProtocolHandler for TalkHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        loop {
            let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                break;
            };
            let request = recv.read_to_end(8).await.unwrap_or_default();
            if request.as_slice() != b"SYNC" {
                continue;
            }
            let blob = self.room.lock().await.export_blob();
            if send.write_all(&blob).await.is_err() || send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TalkLineDto {
    pub id: String,
    pub sender_id: String,
    pub name: String,
    pub text: String,
    pub mine: bool,
}

pub struct Talk {
    room: Arc<Mutex<Room>>,
    tx: mpsc::Sender<TalkCmd>,
    secret: SecretKey,
    handler: TalkHandler,
}

impl Talk {
    pub fn handler(&self) -> TalkHandler {
        self.handler.clone()
    }

    pub async fn lines(&self) -> Vec<TalkLineDto> {
        let room = self.room.lock().await;
        let mine = self.secret.public();
        room.lines
            .iter()
            .map(|line| TalkLineDto {
                id: hex_id(&line.id),
                sender_id: line.sender.to_string(),
                name: line.name.clone(),
                text: line.text.clone(),
                mine: line.sender == mine,
            })
            .collect()
    }

    pub async fn say(&self, text: &str, name: &str) -> Result<(), String> {
        let raw = {
            let mut room = self.room.lock().await;
            room.compose(&self.secret, text, name)
                .map_err(str::to_string)?
        };
        let _ = self.tx.try_send(TalkCmd::Send(raw));
        Ok(())
    }
}

pub fn start(endpoint: Endpoint, gossip: Gossip, plaza: Plaza) -> Talk {
    let secret = endpoint.secret_key().clone();
    let (tx, rx) = mpsc::channel(64);
    let room = Arc::new(Mutex::new(Room {
        lines: Vec::new(),
        seen: HashSet::new(),
        lamport: 0,
        seq: 0,
        last_sent: None,
    }));
    let handler = TalkHandler { room: room.clone() };
    tokio::spawn(serve(gossip, rx, room.clone()));
    tokio::spawn(sync_loop(
        endpoint,
        plaza,
        room.clone(),
        tx.clone(),
        secret.public(),
    ));
    Talk {
        room,
        tx,
        secret,
        handler,
    }
}

async fn serve(gossip: Gossip, mut rx: mpsc::Receiver<TalkCmd>, room: Arc<Mutex<Room>>) {
    loop {
        match session(&gossip, &mut rx, &room).await {
            Ok(()) => tokio::time::sleep(Duration::from_secs(5)).await,
            Err(()) => return,
        }
    }
}

async fn session(
    gossip: &Gossip,
    rx: &mut mpsc::Receiver<TalkCmd>,
    room: &Mutex<Room>,
) -> Result<(), ()> {
    let options = JoinOptions::with_bootstrap(std::iter::empty()).with_subscription_capacity(256);
    let topic = gossip
        .subscribe_with_opts(TOPIC, options)
        .await
        .map_err(|err| {
            tracing::warn!(?err, "talk topic failed");
        })?;
    let (sender, mut receiver) = topic.split();
    loop {
        tokio::select! {
            event = receiver.next() => {
                let Some(event) = event else {
                    return Ok(());
                };
                if let Ok(Event::Received(message)) = event {
                    room.lock().await.ingest_frame(&message.content);
                }
            }
            cmd = rx.recv() => {
                let Some(cmd) = cmd else {
                    return Err(());
                };
                match cmd {
                    TalkCmd::Send(bytes) => {
                        if let Err(err) = sender.broadcast(Bytes::from(bytes)).await {
                            tracing::debug!(?err, "talk broadcast failed");
                        }
                    }
                    TalkCmd::Join(ids) => {
                        if let Err(err) = sender.join_peers(ids).await {
                            tracing::debug!(?err, "talk join failed");
                        }
                    }
                }
            }
        }
    }
}

async fn sync_loop(
    endpoint: Endpoint,
    plaza: Plaza,
    room: Arc<Mutex<Room>>,
    tx: mpsc::Sender<TalkCmd>,
    self_id: EndpointId,
) {
    tokio::time::sleep(Duration::from_secs(4)).await;
    let mut cursor = 0usize;
    loop {
        let mut ids = plaza.peer_ids(8).await;
        for id in room.lock().await.recent_senders(4) {
            if id != self_id && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.retain(|id| *id != self_id);
        if !ids.is_empty() {
            let _ = tx.try_send(TalkCmd::Join(ids.clone()));
            let picks = 3.min(ids.len());
            for step in 0..picks {
                let id = ids[(cursor + step) % ids.len()];
                match fetch_buffer(&endpoint, id).await {
                    Ok(bytes) => {
                        room.lock().await.import_blob(&bytes);
                    }
                    Err(err) => tracing::debug!(%id, ?err, "talk sync skipped"),
                }
            }
            cursor = cursor.wrapping_add(picks);
        }
        tokio::time::sleep(Duration::from_secs(20)).await;
    }
}

async fn fetch_buffer(endpoint: &Endpoint, id: EndpointId) -> anyhow::Result<Vec<u8>> {
    let connection = tokio::time::timeout(
        Duration::from_secs(8),
        endpoint.connect(EndpointAddr::from(id), ALPN),
    )
    .await
    .map_err(|_| anyhow::anyhow!("sync timed out"))?
    .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    send.write_all(b"SYNC").await?;
    send.finish()?;
    let bytes = recv
        .read_to_end(SYNC_LIMIT)
        .await
        .map_err(|err| anyhow::anyhow!("{err:?}"))?;
    connection.close(0u8.into(), b"done");
    Ok(bytes)
}

fn hex_id(id: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for byte in id {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_keep_lamport_order_and_drop_the_oldest() {
        let alice = SecretKey::generate();
        let bob = SecretKey::generate();
        let mut room = Room {
            lines: Vec::new(),
            seen: HashSet::new(),
            lamport: 0,
            seq: 0,
            last_sent: None,
        };
        let first = encode(&alice, [1; 16], 5, "后说的", "阿宁");
        let second = encode(&bob, [2; 16], 2, "先发生", "");
        assert!(room.ingest_frame(&first));
        assert!(room.ingest_frame(&second));
        assert!(!room.ingest_frame(&first));
        assert_eq!(room.lines[0].text, "先发生");
        assert_eq!(room.lines[1].text, "后说的");

        for number in 0..MAX_LINES {
            let mut id = [9u8; 16];
            id[..8].copy_from_slice(&(number as u64).to_be_bytes());
            let frame = encode(&alice, id, 10 + number as u64, "又一句", "阿宁");
            assert!(room.ingest_frame(&frame));
        }
        assert_eq!(room.lines.len(), MAX_LINES);
        assert!(room.lines.iter().all(|line| line.lamport >= 10));
    }

    #[test]
    fn a_buffer_imports_on_b_without_a_clock() {
        let alice = SecretKey::generate();
        let mut kept = Room {
            lines: Vec::new(),
            seen: HashSet::new(),
            lamport: 0,
            seq: 0,
            last_sent: None,
        };
        kept.compose(&alice, " 夜里见 ", " 阿宁 ").unwrap();
        kept.last_sent = None;
        kept.compose(&alice, "还在", "阿宁").unwrap();
        assert_eq!(kept.lines[0].name, "阿宁");
        kept.last_sent = None;
        assert!(
            kept.compose(&alice, "太长", "一二三四五六七八九十一二三四")
                .is_err()
        );
        let blob = kept.export_blob();

        let mut arrived = Room {
            lines: Vec::new(),
            seen: HashSet::new(),
            lamport: 0,
            seq: 0,
            last_sent: None,
        };
        assert_eq!(arrived.import_blob(&blob), 2);
        assert_eq!(arrived.import_blob(&blob), 0);
        assert_eq!(arrived.lines[0].text, "夜里见");
        assert_eq!(arrived.lines[1].text, "还在");
        assert!(arrived.lamport >= 2);

        let mut tampered = kept.lines[0].raw.clone();
        tampered[40] ^= 0x1;
        assert!(decode(&tampered).is_none());
    }
}
