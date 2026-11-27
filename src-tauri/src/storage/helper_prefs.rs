use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use iroh::{EndpointId, RelayMode, RelayUrl};
use serde::{Deserialize, Serialize};

const FILE: &str = "helper_prefs.json";
const BOARD_FILE: &str = "board_cache.json";
const APP_IDENTIFIER: &str = "app.popstall.desktop";
const MAX_BOOTSTRAP_PEERS: usize = 32;
const MAX_CUSTOM_RELAYS: usize = 8;
const MAX_RELAY_URL_LEN: usize = 256;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelayModePref {
    #[default]
    Default,
    Custom,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperPrefs {
    /// Preferred UDP listen port for hole-punch / router forwarding.
    /// `None` or `0` means let the OS pick an ephemeral port.
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// Best-effort router port mapping is opt-in; relays remain the fallback.
    #[serde(default)]
    pub allow_port_mapping: bool,
    #[serde(default)]
    pub relay_mode: RelayModePref,
    #[serde(default)]
    pub relay_urls: Vec<String>,
}

impl HelperPrefs {
    /// Parsed relay URLs for the configured mode, with invalid entries skipped.
    pub fn relay_urls(&self) -> Vec<RelayUrl> {
        let mut out = Vec::new();
        for raw in &self.relay_urls {
            if out.len() == MAX_CUSTOM_RELAYS {
                break;
            }
            if let Some(url) = parse_relay_url(raw) {
                if !out.contains(&url) {
                    out.push(url);
                }
            }
        }
        out
    }

    /// Iroh relay configuration derived from persisted preferences.
    pub fn relay_mode(&self) -> RelayMode {
        match self.relay_mode {
            RelayModePref::Default => RelayMode::Default,
            RelayModePref::Custom => {
                let urls = self.relay_urls();
                if urls.is_empty() {
                    RelayMode::Default
                } else {
                    RelayMode::custom(urls)
                }
            }
        }
    }
}

fn parse_relay_url(raw: &str) -> Option<RelayUrl> {
    if raw.is_empty() || raw.len() > MAX_RELAY_URL_LEN {
        return None;
    }
    let url: RelayUrl = raw.parse().ok()?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    Some(url)
}

/// Persisted pkarr relay configuration and the last verified bulletin.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardCache {
    #[serde(default)]
    pub relay_urls: Vec<String>,
    #[serde(default)]
    pub bootstrap_peers: Vec<String>,
    #[serde(default)]
    pub cached_public: Option<String>,
    /// Hex-encoded relay payload of the last verified signed packet.
    #[serde(default)]
    pub cached_payload: Option<String>,
    #[serde(default)]
    pub cached_timestamp: Option<u64>,
}

/// Records the app data directory seen by `Node::start` so the plaza can persist
/// its board cache without changing the node's public API.
pub fn note_data_dir(data_dir: &Path) {
    let _ = DATA_DIR.set(data_dir.to_path_buf());
}

pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("POPSTALL_DATA_DIR") {
        return PathBuf::from(path);
    }
    if let Some(path) = DATA_DIR.get() {
        return path.clone();
    }
    fallback_data_dir()
}

fn fallback_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support")
                .join(APP_IDENTIFIER);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(root) = std::env::var_os("APPDATA") {
            return PathBuf::from(root).join(APP_IDENTIFIER);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(root) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(root).join(APP_IDENTIFIER);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local/share")
                .join(APP_IDENTIFIER);
        }
    }
    std::env::temp_dir().join("popstall")
}

pub fn load(data_dir: &Path) -> HelperPrefs {
    note_data_dir(data_dir);
    let path = data_dir.join(FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return HelperPrefs::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save(data_dir: &Path, prefs: &HelperPrefs) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|err| err.to_string())?;
    let path = data_dir.join(FILE);
    let bytes = serde_json::to_vec_pretty(prefs).map_err(|err| err.to_string())?;
    std::fs::write(path, bytes).map_err(|err| err.to_string())
}

pub fn normalize_port(port: Option<u16>) -> Option<u16> {
    match port {
        Some(0) | None => None,
        Some(port) => Some(port),
    }
}

pub fn load_board_cache() -> BoardCache {
    load_board_cache_from(&data_dir())
}

pub fn load_board_cache_from(data_dir: &Path) -> BoardCache {
    let Ok(bytes) = std::fs::read(data_dir.join(BOARD_FILE)) else {
        return BoardCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_board_cache(cache: &BoardCache) -> Result<(), String> {
    save_board_cache_to(&data_dir(), cache)
}

pub fn save_board_cache_to(data_dir: &Path, cache: &BoardCache) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|err| err.to_string())?;
    let bytes = serde_json::to_vec_pretty(cache).map_err(|err| err.to_string())?;
    std::fs::write(data_dir.join(BOARD_FILE), bytes).map_err(|err| err.to_string())
}

pub fn remember_bootstrap_peers(ids: &[EndpointId]) {
    remember_bootstrap_peers_in(&data_dir(), ids);
}

pub fn remember_bootstrap_peers_in(data_dir: &Path, ids: &[EndpointId]) {
    let mut cache = load_board_cache_from(data_dir);
    for id in ids {
        let value = id.to_string();
        if !cache.bootstrap_peers.contains(&value) {
            cache.bootstrap_peers.push(value);
        }
    }
    if cache.bootstrap_peers.len() > MAX_BOOTSTRAP_PEERS {
        let excess = cache.bootstrap_peers.len() - MAX_BOOTSTRAP_PEERS;
        cache.bootstrap_peers.drain(..excess);
    }
    if save_board_cache_to(data_dir, &cache).is_err() {
        tracing::debug!("could not persist plaza bootstrap peers");
    }
}

pub fn cache_board_packet(public: &str, payload: &[u8], timestamp: u64) {
    cache_board_packet_in(&data_dir(), public, payload, timestamp);
}

pub fn cache_board_packet_in(data_dir: &Path, public: &str, payload: &[u8], timestamp: u64) {
    let mut cache = load_board_cache_from(data_dir);
    cache.cached_public = Some(public.to_string());
    cache.cached_payload = Some(hex_encode(payload));
    cache.cached_timestamp = Some(timestamp);
    if save_board_cache_to(data_dir, &cache).is_err() {
        tracing::debug!("could not persist plaza board cache");
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let seq = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("popstall-board-prefs-{}-{seq}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn helper_prefs_default_to_plain_relays_without_port_mapping() {
        let prefs: HelperPrefs = serde_json::from_str(r#"{"listenPort":7777}"#).unwrap();
        assert_eq!(prefs.listen_port, Some(7777));
        assert!(!prefs.allow_port_mapping);
        assert_eq!(prefs.relay_mode, RelayModePref::Default);
        assert!(prefs.relay_urls.is_empty());
        assert!(matches!(prefs.relay_mode(), RelayMode::Default));
    }

    #[test]
    fn helper_prefs_round_trip_custom_relay_settings() {
        let dir = temp_dir();
        let prefs = HelperPrefs {
            listen_port: Some(9999),
            allow_port_mapping: true,
            relay_mode: RelayModePref::Custom,
            relay_urls: vec![
                "https://relay.example/".to_string(),
                "http://relay.example/".to_string(),
            ],
        };
        save(&dir, &prefs).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.listen_port, Some(9999));
        assert!(loaded.allow_port_mapping);
        assert_eq!(loaded.relay_mode, RelayModePref::Custom);
        assert_eq!(loaded.relay_urls().len(), 1);
        assert!(matches!(loaded.relay_mode(), RelayMode::Custom(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn board_cache_round_trips_packet_and_relays() {
        let dir = temp_dir();
        let cache = BoardCache {
            relay_urls: vec!["https://relay.example/pkarr".to_string()],
            bootstrap_peers: Vec::new(),
            cached_public: Some("public".to_string()),
            cached_payload: Some("00ff10".to_string()),
            cached_timestamp: Some(42),
        };
        save_board_cache_to(&dir, &cache).unwrap();
        let loaded = load_board_cache_from(&dir);
        assert_eq!(loaded.relay_urls, cache.relay_urls);
        assert_eq!(loaded.cached_payload.as_deref(), Some("00ff10"));
        assert_eq!(loaded.cached_timestamp, Some(42));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remembered_peers_are_unique_and_bounded() {
        let dir = temp_dir();
        let ids: Vec<EndpointId> = (0..40)
            .map(|_| iroh::SecretKey::generate().public())
            .collect();
        remember_bootstrap_peers_in(&dir, &ids);
        remember_bootstrap_peers_in(&dir, &ids[..2]);
        let cache = load_board_cache_from(&dir);
        assert_eq!(cache.bootstrap_peers.len(), MAX_BOOTSTRAP_PEERS);
        let unique: std::collections::BTreeSet<_> = cache.bootstrap_peers.iter().collect();
        assert_eq!(unique.len(), cache.bootstrap_peers.len());
        let _ = std::fs::remove_dir_all(dir);
    }
}
