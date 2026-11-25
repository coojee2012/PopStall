use std::path::Path;

use serde::{Deserialize, Serialize};

const FILE: &str = "helper_prefs.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperPrefs {
    /// Preferred UDP listen port for hole-punch / router forwarding.
    /// `None` or `0` means let the OS pick an ephemeral port.
    #[serde(default)]
    pub listen_port: Option<u16>,
}

pub fn load(data_dir: &Path) -> HelperPrefs {
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
