use log::{debug, info, warn};
use std::path::PathBuf;
use tokio::process::Command;

use crate::clipboard::wl_paste;
use crate::consts::{
    GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME, URI_LIST_MIME, X11_SYNC_MARKER_MIME,
};
use crate::hash::sha256_hex;
use crate::x11_native;

use super::state::{self, MARK_FROM_X11};

pub async fn x11_hook_apply_wayland_to_x11(
    state_dir: &PathBuf,
    _kind: &str,
    _stdin_bytes: Vec<u8>,
) {
    // IMPORTANT:
    // This function runs in a short-lived subprocess (spawned by `wl-paste --watch`).
    // If it directly owns the X11 CLIPBOARD selection, the ownership disappears as soon as
    // this process exits, which looks like the clipboard being cleared.
    //
    // Instead, we only notify the long-lived x11-sync service process, which keeps ownership.
    state::send_wl_notify(state_dir).await;
}

async fn wl_list_types() -> String {
    let out = Command::new("wl-paste").arg("--list-types").output().await;
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

async fn wl_marker_origin_is_x11(wl_types: &str) -> bool {
    if !wl_types
        .lines()
        .any(|l| l.trim() == X11_SYNC_MARKER_MIME)
    {
        return false;
    }

    // Best-effort: read marker payload to know direction.
    // If the marker exists but we fail to read it, be conservative and treat it as originating
    // from X11 to avoid loop / accidental overwrites.
    match wl_paste(X11_SYNC_MARKER_MIME).await {
        Ok(b) => {
            let b = b
                .split(|c| *c == b'\n' || *c == b'\r' || *c == 0)
                .next()
                .unwrap_or(&b);
            b.starts_with(MARK_FROM_X11)
        }
        Err(e) => {
            debug!("wl marker present but failed to read payload: {e:?}");
            true
        }
    }
}

pub(super) async fn apply_wayland_to_x11_full(state_dir: &PathBuf) {
    state::ensure_state_dir(state_dir).await;

    // Marker-based loop prevention:
    // If Wayland clipboard is marked as originating from X11, do NOT sync it back to X11.
    let wl_types = wl_list_types().await;
    let wl_marked_from_x11 = wl_marker_origin_is_x11(&wl_types).await;
    if wl_marked_from_x11 {
        debug!("wl->x11 skip: wl clipboard marked from x11");
        return;
    }

    // Build a multi-MIME snapshot from Wayland and publish it to X11, adding our sync marker.
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();

    // Always publish the marker target (direction in payload).
    items.push((X11_SYNC_MARKER_MIME.to_string(), b"from=wl".to_vec()));

    // File clipboard targets.
    for m in [URI_LIST_MIME, KDE_URI_LIST_MIME, GNOME_COPIED_FILES_MIME] {
        if wl_types.lines().any(|l| l.trim() == m) {
            if let Ok(b) = wl_paste(m).await {
                if !b.is_empty() {
                    items.push((m.to_string(), b));
                }
            }
        }
    }

    // Image targets.
    for m in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        if wl_types.lines().any(|l| l.trim() == m) {
            if let Ok(b) = wl_paste(m).await {
                if !b.is_empty() {
                    items.push((m.to_string(), b));
                    break;
                }
            }
        }
    }

    // Text targets.
    let mut text_bytes: Option<Vec<u8>> = None;
    for m in ["text/plain;charset=utf-8", "text/plain"] {
        if wl_types.lines().any(|l| l.trim() == m) {
            if let Ok(b) = wl_paste(m).await {
                if !b.is_empty() {
                    text_bytes = Some(b);
                    break;
                }
            }
        }
    }

    if let Some(b) = text_bytes {
        // Provide both common X11 text targets.
        items.push(("UTF8_STRING".to_string(), b.clone()));
        items.push(("STRING".to_string(), b.clone()));
        items.push(("text/plain;charset=utf-8".to_string(), b.clone()));
        items.push(("text/plain".to_string(), b));
    }

    // Deduplicate by mime name (keep first).
    let mut seen = std::collections::BTreeSet::new();
    items.retain(|(m, _)| seen.insert(m.clone()));

    // Never publish an "empty" clipboard that only contains our marker.
    if items.len() <= 1 {
        debug!("wl->x11 skip: marker-only (no payload types)");
        return;
    }

    // Hash guard to avoid repeated owner churn when multiple wl-paste watchers fire.
    let hash_material = items
        .iter()
        .map(|(m, b)| format!("{}:{}", m, sha256_hex(b)))
        .collect::<Vec<_>>()
        .join("\n");
    let sha = sha256_hex(hash_material.as_bytes());
    if let Some(last) = state::state_get(state_dir, "wl_full_hash").await {
        if last == sha {
            debug!("wl->x11 skip: same hash {sha}");
            return;
        }
    }

    match x11_native::spawn_clipboard_owner(items) {
        Ok(()) => {
            info!("wl->x11 applied (hash={sha})");
            state::state_set(state_dir, "wl_full_hash", &sha).await;
        }
        Err(e) => {
            warn!("wl->x11 failed to own clipboard: {e:?}");
        }
    }
}
