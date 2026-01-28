use std::path::PathBuf;

fn safe(s: &str) -> String {
    s.replace(['/', ';', '=', ' '], "_")
}

fn last_sent_path(state_dir: &PathBuf, room: &str, key: &str) -> PathBuf {
    // Keep it per-room so multiple rooms on the same machine don't interfere.
    state_dir.join(format!("lastsent_{}_{}", safe(room), safe(key)))
}

/// Read the last sent sha for a given (room, key).
///
/// This is used by short-lived `wl-watch-hook` processes to avoid re-sending the
/// exact same clipboard payload when `wl-paste --watch` fires multiple times.
pub async fn last_sent_get(state_dir: &PathBuf, room: &str, key: &str) -> Option<String> {
    let p = last_sent_path(state_dir, room, key);
    let s = tokio::fs::read_to_string(&p).await.ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Persist the last sent sha for a given (room, key).
pub async fn last_sent_set(state_dir: &PathBuf, room: &str, key: &str, sha: &str) {
    let p = last_sent_path(state_dir, room, key);
    let _ = tokio::fs::write(&p, format!("{}\n", sha)).await;
}
