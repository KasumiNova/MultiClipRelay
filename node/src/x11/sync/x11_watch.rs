use anyhow::Context;
use log::{debug, warn};

use x11rb::{COPY_FROM_PARENT, CURRENT_TIME};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, SelectionEventMask};
use x11rb::protocol::xproto::{self, Atom, AtomEnum, EventMask, Window};
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::consts::{
    GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME, URI_LIST_MIME, X11_SYNC_MARKER_MIME,
};

use super::state::{MARK_FROM_WL};

pub(super) struct X11Snapshot {
    pub(super) marked_from_wayland: bool,
    pub(super) items: Vec<(String, Vec<u8>)>,
}

fn looks_like_file_uri_list_text(s: &str) -> bool {
    s.lines().any(|l| {
        let t = l.trim();
        t.starts_with("file://") || t.starts_with("file:/")
    })
}

pub(super) fn x11_watch_clipboard_loop(
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
                if let Ok(snap) = read_x11_clipboard_snapshot(
                    &conn,
                    win,
                    clipboard,
                    max_text_bytes,
                    max_image_bytes,
                ) {
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

    let targets_bytes =
        convert_selection_get(conn, win, clipboard, targets_atom, property)?.unwrap_or_default();

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
                if b.is_empty() {
                    continue;
                }
                if b.len() > max_text_bytes {
                    warn!(
                        "x11->wl: large file mime payload ({} bytes > max_text_bytes={}); still syncing",
                        b.len(),
                        max_text_bytes
                    );
                }
                items.push((m.to_string(), b));
            }
        }
    }

    // Image.
    for m in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
        let a = intern_atom(conn, m)?;
        if atoms.contains(&a) {
            if let Some(b) = convert_selection_get(conn, win, clipboard, a, property)? {
                if b.is_empty() {
                    continue;
                }
                if b.len() > max_image_bytes {
                    warn!(
                        "x11->wl: large image payload ({} bytes > max_image_bytes={}); still syncing",
                        b.len(),
                        max_image_bytes
                    );
                }
                items.push((m.to_string(), b));
                break;
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
                if b.is_empty() {
                    continue;
                }
                if b.len() > max_text_bytes {
                    warn!(
                        "x11->wl: large text payload ({} bytes > max_text_bytes={}); still syncing",
                        b.len(),
                        max_text_bytes
                    );
                }
                text_bytes = Some(b);
                break;
            }
        }
    }

    if let Some(b) = text_bytes {
        // Always expose it as text/plain on Wayland.
        items.push(("text/plain;charset=utf-8".to_string(), b.clone()));
        items.push(("text/plain".to_string(), b.clone()));

        // If it looks like file:// URIs and no uri-list exists, synthesize text/uri-list.
        if !items.iter().any(|(m, _)| {
            m == URI_LIST_MIME || m == KDE_URI_LIST_MIME || m == GNOME_COPIED_FILES_MIME
        }) {
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
