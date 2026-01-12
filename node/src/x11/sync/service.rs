use anyhow::Context;
use log::{debug, info, warn};
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixDatagram;

use crate::consts::X11_SYNC_MARKER_MIME;
use crate::hash::sha256_hex;

use super::state;
use super::wl_to_x11::apply_wayland_to_x11_full;
use super::x11_watch::{x11_watch_clipboard_loop, X11Snapshot};

pub struct X11SyncOpts {
    pub state_dir: PathBuf,
    pub poll_interval: Duration,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
}

pub async fn x11_sync_service(opts: X11SyncOpts) -> anyhow::Result<()> {
    state::ensure_state_dir(&opts.state_dir).await;

    // IPC for Wayland -> X11 triggers.
    // x11-hook sends datagrams here; we do the actual X11 clipboard write in this process.
    let sock_path = state::wl_notify_socket_path_for_bind(&opts.state_dir);
    let _ = tokio::fs::remove_file(&sock_path).await;
    let wl_sock = UnixDatagram::bind(&sock_path).context("bind wl notify socket")?;

    // Event-driven X11 -> Wayland.
    // We watch XFixes selection notifications and only sync when the selection changes.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<X11Snapshot>(8);
    let max_text = opts.max_text_bytes;
    let max_img = opts.max_image_bytes;

    tokio::task::spawn_blocking(move || {
        if let Err(e) = x11_watch_clipboard_loop(tx, max_text, max_img) {
            eprintln!("x11-sync: x11 watch loop failed: {:#}", e);
        }
    });

    let mut last_hash: Option<String> = None;
    let mut wl_buf = vec![0u8; 128];

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                let Some(snap) = maybe else { break; };

                // Skip echo: if X11 clipboard was produced by us from Wayland, it will contain our marker with payload from=wl.
                if snap.marked_from_wayland {
                    debug!("x11->wl skip: x11 clipboard marked from wl");
                    continue;
                }

                // If X11 has no meaningful data (e.g. no owner / selection cleared), do not override Wayland.
                // Otherwise we'd replace the current clipboard with a marker-only selection.
                if snap.items.is_empty() {
                    debug!("x11->wl skip: empty snapshot");
                    continue;
                }

                // Construct Wayland multi-mime set, and tag it as originating from X11.
                let mut items: Vec<(String, Vec<u8>)> = Vec::new();
                // Marker is always included, but we MUST ensure we have at least one non-marker payload item.
                items.push((X11_SYNC_MARKER_MIME.to_string(), b"from=x11".to_vec()));

                let mut payload_count = 0usize;
                for (mime, bytes) in snap.items {
                    if bytes.is_empty() {
                        continue;
                    }
                    // Never forward the marker itself (if it somehow appears in payload list).
                    if mime == X11_SYNC_MARKER_MIME {
                        continue;
                    }
                    payload_count += 1;
                    items.push((mime, bytes));
                }

                // HARD GUARD:
                // Never publish a marker-only clipboard to Wayland.
                if payload_count == 0 {
                    debug!("x11->wl skip: marker-only payload");
                    continue;
                }

                // Hash guard.
                let hash_material = items
                    .iter()
                    .map(|(m, b)| format!("{}:{}", m, sha256_hex(b)))
                    .collect::<Vec<_>>()
                    .join("\n");
                let sha = sha256_hex(hash_material.as_bytes());
                if last_hash.as_deref() == Some(&sha) {
                    debug!("x11->wl skip: same hash {sha}");
                    continue;
                }

                match crate::clipboard::wl_copy_multi(items).await {
                    Ok(()) => {
                        info!("x11->wl applied (hash={sha})");
                    }
                    Err(e) => {
                        warn!("x11->wl failed to write wl clipboard: {e:?}");
                    }
                }
                last_hash = Some(sha);
            }
            recv = wl_sock.recv_from(&mut wl_buf) => {
                if recv.is_ok() {
                    debug!("wl notify received -> wl->x11 scan/apply");
                    // Coalesce storms via our own hashing guard inside apply_wayland_to_x11_full.
                    apply_wayland_to_x11_full(&opts.state_dir).await;
                }
            }
        }
    }

    Ok(())
}
