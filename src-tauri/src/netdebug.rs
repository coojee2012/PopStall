//! Persistent network debug log for packaged builds.
//! Written under `<app_data>/logs/netdebug.log`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 8 * 1024 * 1024;

static STATE: OnceLock<Mutex<NetDebug>> = OnceLock::new();

struct NetDebug {
    path: PathBuf,
}

pub fn init(data_dir: &Path) {
    let logs = data_dir.join("logs");
    if let Err(err) = fs::create_dir_all(&logs) {
        eprintln!("netdebug: create logs dir failed: {err}");
        return;
    }
    let path = logs.join("netdebug.log");
    rotate_if_huge(&path);
    let _ = STATE.set(Mutex::new(NetDebug { path: path.clone() }));
    log(format!(
        "==== netdebug start pid={} path={} ====",
        std::process::id(),
        path.display()
    ));
}

pub fn path() -> Option<PathBuf> {
    STATE
        .get()
        .and_then(|state| state.lock().ok().map(|g| g.path.clone()))
}

pub fn path_string() -> String {
    path().map(|p| p.display().to_string()).unwrap_or_default()
}

pub fn log(message: impl AsRef<str>) {
    let Some(state) = STATE.get() else {
        return;
    };
    let Ok(guard) = state.lock() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&guard.path)
    else {
        return;
    };
    let _ = writeln!(file, "{} {}", stamp(), message.as_ref());
    let _ = file.flush();
}

fn stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // UTC-ish unix time keeps this dependency-free on every platform.
    let secs = now.as_secs();
    let days = secs / 86_400;
    let day_secs = secs % 86_400;
    let hour = day_secs / 3600;
    let min = (day_secs % 3600) / 60;
    let sec = day_secs % 60;
    // Civil date from Unix day count (Howard Hinnant algorithm).
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{:03}Z",
        now.subsec_millis()
    )
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

fn rotate_if_huge(path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() < MAX_BYTES {
        return;
    }
    let bak = path.with_extension("log.1");
    let _ = fs::remove_file(&bak);
    let _ = fs::rename(path, &bak);
}
