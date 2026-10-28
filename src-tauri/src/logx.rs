//! Minimal append-only file logger, and the diagnostic tail the user hands
//! to support. One line per entry, tagged with a source tag.
//!
//! The path comes from Tauri's per-platform log directory, handed over once
//! at startup by [`init`] — `log` is called from places with no `AppHandle`,
//! so it cannot resolve the path itself. Before `init` (and if resolution
//! fails) it falls back to the config directory next to the profile store.
//! The previous version read `%APPDATA%` directly, which meant the log went
//! nowhere useful on Linux and macOS.
//!
//! Deliberately dependency-free (`std` only), and every failure is ignored:
//! logging must never break the app.
//!
//! NOTHING SECRET GOES IN HERE. Not the gateway admin token, not a pairing
//! code, not a password, not the contents of a key file — the file is plain
//! text on disk and its whole point is to be copied to someone else. See
//! `docs/specs/diagnostics.md`.
//!
//! The rule that keeps that true: a remote command's output is logged only
//! when the command FAILED (non-zero exit), because a failed command printed
//! neither a code nor a token — that is the one case where the output is
//! only a shell error, and also the one case support actually needs. On
//! success, callers log the shape of the output and never its content.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Largest the log may get before the oldest lines are dropped. This is a
/// support artifact for the last session, not an audit trail.
const MAX_BYTES: u64 = 1_000_000;
/// How many lines survive a truncation.
const KEEP_LINES: usize = 200;

/// Tell the logger where to write. Called once from `setup` with the
/// platform's log directory; later calls are ignored.
pub fn init(path: PathBuf) {
    let _ = LOG_PATH.set(path);
}

/// Where the log lives — the path `init` was given, or the fallback.
pub fn path() -> PathBuf {
    LOG_PATH.get().cloned().unwrap_or_else(fallback_path)
}

/// Pre-`init` (and if Tauri cannot resolve a log directory): the config
/// directory the profile store already uses.
fn fallback_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ru.metalmon.volt-admin").join("debug.log")
}

/// Append `msg` to the debug log under `tag`. Cheap and infallible.
pub fn log(tag: &str, msg: &str) {
    let path = path();
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    cap(&path);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{ts}] {tag}: {msg}");
    }
}

/// The last `n` lines of the log, for the diagnostics the user copies.
pub fn tail(n: usize) -> String {
    match std::fs::read_to_string(path()) {
        Ok(text) => last_lines(&text, n),
        Err(e) => format!("(no log: {e})"),
    }
}

/// Drop the oldest lines once the file has grown past [`MAX_BYTES`].
fn cap(path: &std::path::Path) {
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES);
    if !too_big {
        return;
    }
    if let Ok(text) = std::fs::read_to_string(path) {
        let _ = std::fs::write(path, last_lines(&text, KEEP_LINES));
    }
}

/// The last `n` lines of `text`, newline-terminated. Pure, so it is the part
/// worth testing.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut out = lines[start..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_lines_keeps_the_newest_ones() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(last_lines(text, 2), "c\nd\n");
        assert_eq!(last_lines(text, 99), "a\nb\nc\nd\n");
        assert_eq!(last_lines("", 10), "");
    }

    #[test]
    fn cap_trims_a_file_that_grew_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("volt-logx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("debug.log");
        // One line per 100 bytes, comfortably over MAX_BYTES.
        let line = "x".repeat(99);
        let text: String = std::iter::repeat_n(line.as_str(), 20_000)
            .map(|l| format!("{l}\n"))
            .collect();
        std::fs::write(&path, &text).unwrap();
        cap(&path);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after.lines().count(), KEEP_LINES);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
