use std::path::PathBuf;
use std::time::Duration;

use crate::consts::FILE_SUPPRESS_KEY;

pub fn suppress_path(state_dir: &PathBuf, room: &str, mime: &str) -> PathBuf {
    // include room to allow multiple rooms on same machine
    let safe_room = room.replace('/', "_");
    let safe_mime = mime.replace('/', "_").replace(';', "_").replace('=', "_");
    state_dir.join(format!("suppress_{}_{}", safe_room, safe_mime))
}

pub async fn set_suppress(state_dir: &PathBuf, room: &str, mime: &str, sha: &str, ttl: Duration) {
    let expires = utils::now_ms().saturating_add(ttl.as_millis() as u64);
    let p = suppress_path(state_dir, room, mime);
    let _ = tokio::fs::write(p, format!("{}\n{}\n", sha, expires)).await;
}

pub async fn is_suppressed(state_dir: &PathBuf, room: &str, mime: &str, sha: &str) -> bool {
    let p = suppress_path(state_dir, room, mime);
    let s = match tokio::fs::read_to_string(p).await {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut it = s.lines();
    let ssha = it.next().unwrap_or("").trim();
    let sexp = it.next().unwrap_or("0").trim();
    if ssha != sha {
        return false;
    }
    let exp: u64 = sexp.parse().unwrap_or(0);
    utils::now_ms() <= exp
}

pub async fn is_file_suppressed(state_dir: &PathBuf, room: &str, sha: &str) -> bool {
    is_suppressed(state_dir, room, FILE_SUPPRESS_KEY, sha).await
}

pub async fn set_file_suppress(state_dir: &PathBuf, room: &str, sha: &str, ttl: Duration) {
    set_suppress(state_dir, room, FILE_SUPPRESS_KEY, sha, ttl).await;
}
