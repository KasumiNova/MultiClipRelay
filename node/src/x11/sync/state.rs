use std::path::PathBuf;

use tokio::net::UnixDatagram;

pub(crate) const SUBDIR: &str = "x11-sync";
const WL_NOTIFY_SOCK: &str = "wl_notify.sock";

pub(crate) const MARK_FROM_X11: &[u8] = b"from=x11";
pub(crate) const MARK_FROM_WL: &[u8] = b"from=wl";

fn wl_notify_socket_path(state_dir: &PathBuf) -> PathBuf {
    state_dir.join(SUBDIR).join(WL_NOTIFY_SOCK)
}

fn state_path(state_dir: &PathBuf, key: &str) -> PathBuf {
    state_dir.join(SUBDIR).join(key)
}

pub(crate) async fn ensure_state_dir(state_dir: &PathBuf) {
    let _ = tokio::fs::create_dir_all(state_dir.join(SUBDIR)).await;
}

pub(crate) async fn send_wl_notify(state_dir: &PathBuf) {
    ensure_state_dir(state_dir).await;
    let p = wl_notify_socket_path(state_dir);
    let sock = UnixDatagram::unbound();
    let Ok(sock) = sock else {
        return;
    };
    let _ = sock.send_to(b"changed", &p).await;
}

pub(crate) fn wl_notify_socket_path_for_bind(state_dir: &PathBuf) -> PathBuf {
    wl_notify_socket_path(state_dir)
}

pub(crate) async fn state_get(state_dir: &PathBuf, key: &str) -> Option<String> {
    let p = state_path(state_dir, key);
    let s = tokio::fs::read_to_string(&p).await.ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub(crate) async fn state_set(state_dir: &PathBuf, key: &str, val: &str) {
    let p = state_path(state_dir, key);
    let _ = tokio::fs::write(&p, val).await;
}
