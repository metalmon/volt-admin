//! Minimal append-only file logger used for diagnosing connection hangs.
//! Writes one line per entry, tagged with a source tag, to
//! `%APPDATA%\ru.metalmon.volt-admin\debug.log` — the same directory the
//! profile store lives in, so it is easy to find. Deliberately dependency-free
//! (`std` only): failures to open/append are silently ignored so logging never
//! breaks the app.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

/// Append `msg` to the debug log under `tag`. Cheap and infallible.
pub fn log(tag: &str, msg: &str) {
    let dir = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let mut dir = std::path::PathBuf::from(dir);
    dir.push("ru.metalmon.volt-admin");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    dir.push("debug.log");
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&dir) {
        let _ = writeln!(f, "[{ts}] {tag}: {msg}");
    }
}