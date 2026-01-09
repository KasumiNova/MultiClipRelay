use anyhow::Context;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::clipboard::{wl_copy, wl_paste};
use crate::consts::{APPLIED_MARKER_MIME, GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME, URI_LIST_MIME};
use crate::hash::sha256_hex;

const SUBDIR: &str = "x11-sync";
const PAUSE_TEXT_SYNC_KEY: &str = "pause_text_sync";

fn state_path(state_dir: &PathBuf, key: &str) -> PathBuf {
    state_dir.join(SUBDIR).join(key)
}

async fn ensure_state_dir(state_dir: &PathBuf) {
    let _ = tokio::fs::create_dir_all(state_dir.join(SUBDIR)).await;
}

async fn state_get(state_dir: &PathBuf, key: &str) -> Option<String> {
    let p = state_path(state_dir, key);
    let s = tokio::fs::read_to_string(&p).await.ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

async fn state_set(state_dir: &PathBuf, key: &str, val: &str) {
    let p = state_path(state_dir, key);
    let _ = tokio::fs::write(&p, val).await;
}

/// Pause x11-sync text synchronization for the specified duration.
/// Called by wl-watch when it sends file clipboard to prevent x11-sync from overriding.
pub async fn pause_x11_text_sync(state_dir: &PathBuf, ttl: Duration) {
    ensure_state_dir(state_dir).await;
    let expires = utils::now_ms().saturating_add(ttl.as_millis() as u64);
    state_set(state_dir, PAUSE_TEXT_SYNC_KEY, &expires.to_string()).await;
}

/// Check if x11-sync text synchronization is currently paused.
async fn is_text_sync_paused(state_dir: &PathBuf) -> bool {
    let Some(s) = state_get(state_dir, PAUSE_TEXT_SYNC_KEY).await else {
        return false;
    };
    let exp: u64 = s.parse().unwrap_or(0);
    utils::now_ms() <= exp
}

async fn run_output(mut cmd: Command) -> anyhow::Result<Vec<u8>> {
    let out = cmd.output().await.context("spawn command")?;
    if !out.status.success() {
        anyhow::bail!("command failed")
    }
    Ok(out.stdout)
}

async fn xclip_targets() -> String {
    let out = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

async fn xclip_read(mime: &str) -> anyhow::Result<Vec<u8>> {
    let mut cmd = Command::new("xclip");
    cmd.args(["-selection", "clipboard", "-t", mime, "-o"]);
    run_output(cmd).await
}

async fn xclip_set(mime: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let child = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", mime, "-i"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawn xclip")?;

    let mut child = child;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(bytes).await.ok();
    }
    let _ = child.wait().await;
    Ok(())
}

async fn xclip_clear() {
    let child = Command::new("xclip")
        .args(["-selection", "clipboard", "-i"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.shutdown().await;
        }
        let _ = child.wait().await;
    }
}

async fn wl_list_types() -> String {
    let out = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

fn pick_x11_image_type(targets: &str) -> Option<&'static str> {
    // Prefer png first.
    for t in ["image/png", "image/jpeg", "image/gif"] {
        if targets.contains(t) {
            return Some(t);
        }
    }
    None
}

pub struct X11SyncOpts {
    pub state_dir: PathBuf,
    pub poll_interval: Duration,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
}

pub async fn x11_hook_apply_wayland_to_x11(state_dir: &PathBuf, kind: &str, bytes: Vec<u8>) {
    ensure_state_dir(state_dir).await;

    let max = match kind {
        "text" => 1usize * 1024 * 1024,
        "image" => 20usize * 1024 * 1024,
        _ => 20usize * 1024 * 1024,
    };
    let bytes = if bytes.len() > max {
        return;
    } else {
        bytes
    };

    let sha = sha256_hex(&bytes);

    match kind {
        "text" => {
            // Skip echo: if we just copied X11 -> Wayland, don't copy back.
            if let Some(last_x11) = state_get(state_dir, "x11_text_hash").await {
                if last_x11 == sha {
                    return;
                }
            }
            let _ = xclip_set("UTF8_STRING", &bytes).await;
            state_set(state_dir, "wl_text_hash", &sha).await;
            state_set(state_dir, "x11_text_hash", &sha).await;
        }
        "image" => {
            if let Some(last_x11) = state_get(state_dir, "x11_img_hash").await {
                if last_x11 == sha {
                    return;
                }
            }
            // wl-paste --type image typically yields image/png.
            let _ = xclip_set("image/png", &bytes).await;
            state_set(state_dir, "wl_img_hash", &sha).await;
            state_set(state_dir, "x11_img_hash", &sha).await;
        }
        _ => {}
    }
}

pub async fn x11_sync_service(opts: X11SyncOpts) -> anyhow::Result<()> {
    ensure_state_dir(&opts.state_dir).await;

    let mut last_x11_text_hash = state_get(&opts.state_dir, "x11_text_hash").await;
    let mut last_x11_img_hash = state_get(&opts.state_dir, "x11_img_hash").await;

    loop {
        let targets = xclip_targets().await;
        let mut img_synced = false;

        if let Some(img_type) = pick_x11_image_type(&targets) {
            let bytes = xclip_read(img_type).await.unwrap_or_default();
            if !bytes.is_empty() && bytes.len() <= opts.max_image_bytes {
                let sha = sha256_hex(&bytes);
                if Some(sha.clone()) != last_x11_img_hash {
                    // Copy X11 -> Wayland
                    let _ = wl_copy(img_type, &bytes).await;
                    last_x11_img_hash = Some(sha.clone());
                    state_set(&opts.state_dir, "x11_img_hash", &sha).await;
                    state_set(&opts.state_dir, "wl_img_hash", &sha).await;
                    img_synced = true;
                    // Make Wayland the source of truth; reduces echo loops.
                    xclip_clear().await;
                }
            }
        }

        if !img_synced {
            // Text: only attempt if it doesn't look like an image selection.
            let x11_text = if pick_x11_image_type(&targets).is_none() {
                match xclip_read("UTF8_STRING").await {
                    Ok(b) => b,
                    Err(_) => xclip_read("STRING").await.unwrap_or_default(),
                }
            } else {
                Vec::new()
            };

            if !x11_text.is_empty() && x11_text.len() <= opts.max_text_bytes {
                // Check if wl-watch has signaled us to pause text sync (e.g. during file transfer).
                if is_text_sync_paused(&opts.state_dir).await {
                    tokio::time::sleep(opts.poll_interval).await;
                    continue;
                }

                // Skip if the text looks like file:// URIs (e.g. copied from a file manager).
                // These should NOT override Wayland's text/uri-list which has proper file semantics.
                let text_str = String::from_utf8_lossy(&x11_text);
                let looks_like_file_uri = text_str.lines().any(|l| {
                    let t = l.trim();
                    t.starts_with("file://") || t.starts_with("file:/")
                });
                if looks_like_file_uri {
                    tokio::time::sleep(opts.poll_interval).await;
                    continue;
                }

                // Also skip if Wayland clipboard currently offers file-copy MIMEs.
                // This prevents X11 text from overriding an active file selection.
                let wl_types = wl_list_types().await;
                let wl_has_file_mime = wl_types.lines().any(|l| {
                    let t = l.trim();
                    t == URI_LIST_MIME
                        || t == KDE_URI_LIST_MIME
                        || t == GNOME_COPIED_FILES_MIME
                        || t == APPLIED_MARKER_MIME
                });
                if wl_has_file_mime {
                    tokio::time::sleep(opts.poll_interval).await;
                    continue;
                }

                let sha = sha256_hex(&x11_text);
                let wl_text = wl_paste("text/plain").await.unwrap_or_default();
                if wl_text != x11_text {
                    if Some(sha.clone()) != last_x11_text_hash {
                        let _ = wl_copy("text/plain;charset=utf-8", &x11_text).await;
                        last_x11_text_hash = Some(sha.clone());
                        state_set(&opts.state_dir, "x11_text_hash", &sha).await;
                        state_set(&opts.state_dir, "wl_text_hash", &sha).await;
                    }
                }
            }
        }

        tokio::time::sleep(opts.poll_interval).await;
    }
}
