use anyhow::Context;
use log::{debug, info, warn};
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixDatagram;
use tokio::process::Command;

use crate::clipboard::wl_paste;
use crate::consts::{
    GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME, URI_LIST_MIME, X11_SYNC_MARKER_MIME,
};
use crate::hash::sha256_hex;
use crate::x11_native;

use x11rb::{COPY_FROM_PARENT, CURRENT_TIME};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, SelectionEventMask};
use x11rb::protocol::xproto::{self, Atom, AtomEnum, EventMask, Window};
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

const SUBDIR: &str = "x11-sync";
const WL_NOTIFY_SOCK: &str = "wl_notify.sock";

const MARK_FROM_X11: &[u8] = b"from=x11";
const MARK_FROM_WL: &[u8] = b"from=wl";

fn wl_notify_socket_path(state_dir: &PathBuf) -> PathBuf {
    state_dir.join(SUBDIR).join(WL_NOTIFY_SOCK)
}

fn state_path(state_dir: &PathBuf, key: &str) -> PathBuf {
    state_dir.join(SUBDIR).join(key)
}

async fn ensure_state_dir(state_dir: &PathBuf) {
    let _ = tokio::fs::create_dir_all(state_dir.join(SUBDIR)).await;
}

async fn send_wl_notify(state_dir: &PathBuf) {
    ensure_state_dir(state_dir).await;
    let p = wl_notify_socket_path(state_dir);
    let sock = UnixDatagram::unbound();
    let Ok(sock) = sock else {
        return;
    };
    let _ = sock.send_to(b"changed", &p).await;
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

async fn wl_marker_origin_is_x11(wl_types: &str) -> bool {
    if !wl_types.lines().any(|l| l.trim() == X11_SYNC_MARKER_MIME) {
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

fn looks_like_file_uri_list_text(s: &str) -> bool {
    s.lines().any(|l| {
        let t = l.trim();
        t.starts_with("file://") || t.starts_with("file:/")
    })
}

pub struct X11SyncOpts {
    pub state_dir: PathBuf,
    pub poll_interval: Duration,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
}

pub async fn x11_hook_apply_wayland_to_x11(state_dir: &PathBuf, _kind: &str, _stdin_bytes: Vec<u8>) {
    // IMPORTANT:
    // This function runs in a short-lived subprocess (spawned by `wl-paste --watch`).
    // If it directly owns the X11 CLIPBOARD selection, the ownership disappears as soon as
    // this process exits, which looks like the clipboard being cleared.
    //
    // Instead, we only notify the long-lived x11-sync service process, which keeps ownership.
    send_wl_notify(state_dir).await;
}

async fn apply_wayland_to_x11_full(state_dir: &PathBuf) {
    ensure_state_dir(state_dir).await;

    // Marker-based loop prevention:
    // If Wayland clipboard is marked as originating from X11, do NOT sync it back to X11.
    // This replaces the old "hash echo" and sleep-based suppression.
    let wl_types = wl_list_types().await;
    let wl_marked_from_x11 = wl_marker_origin_is_x11(&wl_types).await;
    if wl_marked_from_x11 {
        debug!("wl->x11 skip: wl clipboard marked from x11");
        return;
    }

    // Build a multi-MIME snapshot from Wayland and publish it to X11, adding our sync marker.
    // We intentionally do a full scan on trigger to preserve file clipboard targets.
    // Note: wl-paste exits non-zero if a requested type is unavailable.
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
    // Doing so would effectively clear the user's clipboard content.
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
    if let Some(last) = state_get(state_dir, "wl_full_hash").await {
        if last == sha {
            debug!("wl->x11 skip: same hash {sha}");
            return;
        }
    }

    match x11_native::spawn_clipboard_owner(items) {
        Ok(()) => {
            info!("wl->x11 applied (hash={sha})");
        state_set(state_dir, "wl_full_hash", &sha).await;
        }
        Err(e) => {
            warn!("wl->x11 failed to own clipboard: {e:?}");
        }
    }
}

pub async fn x11_sync_service(opts: X11SyncOpts) -> anyhow::Result<()> {
    ensure_state_dir(&opts.state_dir).await;

    // IPC for Wayland -> X11 triggers.
    // x11-hook sends datagrams here; we do the actual X11 clipboard write in this process.
    let sock_path = wl_notify_socket_path(&opts.state_dir);
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

struct X11Snapshot {
    marked_from_wayland: bool,
    items: Vec<(String, Vec<u8>)>,
}

fn x11_watch_clipboard_loop(
    tx: tokio::sync::mpsc::Sender<X11Snapshot>,
    max_text_bytes: usize,
    max_image_bytes: usize,
) -> anyhow::Result<()> {
    let (conn, screen_num) = RustConnection::connect(None).context("connect X11")?;
    let screen = &conn.setup().roots[screen_num];

    // Ensure XFixes is available.
    let _ = xfixes::query_version(&conn, 5, 0)
        .context("xfixes query_version")?
        .reply()
        .context("xfixes query_version reply")?;

    let win: Window = conn.generate_id().context("gen window id")?;
    conn.create_window(
        0,
        win,
        screen.root,
        0,
        0,
        1,
        1,
        0,
        xproto::WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &xproto::CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .context("create window")?;

    let clipboard = intern_atom(&conn, "CLIPBOARD")?;
    xfixes::select_selection_input(&conn, win, clipboard, SelectionEventMask::SET_SELECTION_OWNER)
        .context("xfixes select_selection_input")?;
    conn.flush().ok();

    loop {
        let ev = conn.wait_for_event().context("wait_for_event")?;
        match ev {
            Event::XfixesSelectionNotify(_n) => {
                if let Ok(snap) = read_x11_clipboard_snapshot(&conn, win, clipboard, max_text_bytes, max_image_bytes) {
                    let _ = tx.blocking_send(snap);
                }
            }
            _ => {}
        }
    }
}

fn intern_atom<C: Connection>(conn: &C, name: &str) -> anyhow::Result<Atom> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .context("intern_atom")?
        .reply()
        .context("intern_atom reply")?
        .atom)
}

fn convert_selection_get(
    conn: &RustConnection,
    win: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
) -> anyhow::Result<Option<Vec<u8>>> {
    conn.convert_selection(win, selection, target, property, CURRENT_TIME)
        .context("convert_selection")?;
    conn.flush().ok();

    loop {
        let ev = conn.wait_for_event().context("wait_for_event (convert)")?;
        if let Event::SelectionNotify(n) = ev {
            if n.selection != selection || n.target != target {
                continue;
            }
            if n.property == u32::from(AtomEnum::NONE) {
                return Ok(None);
            }
            let reply = conn
                .get_property(false, win, property, AtomEnum::ANY, 0, u32::MAX)
                .context("get_property")?
                .reply()
                .context("get_property reply")?;
            // INCR not supported.
            if reply.type_ == intern_atom(conn, "INCR")? {
                return Ok(None);
            }
            let bytes = reply.value;
            let _ = conn.delete_property(win, property);
            conn.flush().ok();
            return Ok(Some(bytes));
        }
    }
}

fn read_x11_clipboard_snapshot(
    conn: &RustConnection,
    win: Window,
    clipboard: Atom,
    max_text_bytes: usize,
    max_image_bytes: usize,
) -> anyhow::Result<X11Snapshot> {
    let targets_atom = intern_atom(conn, "TARGETS")?;
    let property = intern_atom(conn, "MCR_X11_PROP")?;

    let targets_bytes = convert_selection_get(conn, win, clipboard, targets_atom, property)?
        .unwrap_or_default();

    let mut atoms: Vec<Atom> = Vec::new();
    for chunk in targets_bytes.chunks_exact(4) {
        atoms.push(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }

    let marker_atom = intern_atom(conn, X11_SYNC_MARKER_MIME)?;
    let marked_from_wayland = if atoms.contains(&marker_atom) {
        // Best-effort: read marker payload.
        // If read fails, be conservative and treat it as marked (avoid echo-loop).
        match convert_selection_get(conn, win, clipboard, marker_atom, property)? {
            Some(b) => {
                let b = b
                    .split(|c| *c == b'\n' || *c == b'\r' || *c == 0)
                    .next()
                    .unwrap_or(&b);
                b.starts_with(MARK_FROM_WL)
            }
            None => true,
        }
    } else {
        false
    };

    // Read the most relevant payloads.
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();

    // File types first.
    for m in [URI_LIST_MIME, KDE_URI_LIST_MIME, GNOME_COPIED_FILES_MIME] {
        let a = intern_atom(conn, m)?;
        if atoms.contains(&a) {
            if let Some(b) = convert_selection_get(conn, win, clipboard, a, property)? {
                if !b.is_empty() && b.len() <= max_text_bytes {
                    items.push((m.to_string(), b));
                }
            }
        }
    }

    // Image.
    for m in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        let a = intern_atom(conn, m)?;
        if atoms.contains(&a) {
            if let Some(b) = convert_selection_get(conn, win, clipboard, a, property)? {
                if !b.is_empty() && b.len() <= max_image_bytes {
                    items.push((m.to_string(), b));
                    break;
                }
            }
        }
    }

    // Text.
    let utf8 = intern_atom(conn, "UTF8_STRING")?;
    let string_atom = AtomEnum::STRING.into();
    let text_plain_utf8 = intern_atom(conn, "text/plain;charset=utf-8")?;
    let text_plain = intern_atom(conn, "text/plain")?;

    let mut text_bytes: Option<Vec<u8>> = None;
    for a in [utf8, text_plain_utf8, text_plain, string_atom] {
        if atoms.contains(&a) {
            if let Some(b) = convert_selection_get(conn, win, clipboard, a, property)? {
                if !b.is_empty() && b.len() <= max_text_bytes {
                    text_bytes = Some(b);
                    break;
                }
            }
        }
    }

    if let Some(b) = text_bytes {
        // Always expose it as text/plain on Wayland.
        items.push(("text/plain;charset=utf-8".to_string(), b.clone()));
        items.push(("text/plain".to_string(), b.clone()));

        // If it looks like file:// URIs and no uri-list exists, synthesize text/uri-list.
        if !items.iter().any(|(m, _)| m == URI_LIST_MIME || m == KDE_URI_LIST_MIME || m == GNOME_COPIED_FILES_MIME) {
            let s = String::from_utf8_lossy(&b);
            if looks_like_file_uri_list_text(&s) {
                items.push((URI_LIST_MIME.to_string(), b));
            }
        }
    }

    // Dedup by mime.
    let mut seen = std::collections::BTreeSet::new();
    items.retain(|(m, _)| seen.insert(m.clone()));

    debug!(
        "x11 snapshot: marked_from_wl={} items={}",
        marked_from_wayland,
        items.len()
    );

    Ok(X11Snapshot {
        marked_from_wayland,
        items,
    })
}
