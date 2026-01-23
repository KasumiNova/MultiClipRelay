use anyhow::Context;
use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use x11rb::{COPY_FROM_PARENT, CURRENT_TIME};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, CreateWindowAux, EventMask, PropMode,
    PropertyNotifyEvent, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::{Event, xproto};
use x11rb::rust_connection::RustConnection;

const INCR_CHUNK_BYTES: usize = 64 * 1024;
const INCR_TIMEOUT: Duration = Duration::from_secs(5);

/// A minimal multi-target X11 clipboard owner.
///
/// This exists because `xclip` can only set *one* target at a time, which makes it impossible
/// to preserve file clipboard targets (e.g. text/uri-list) while also offering a coordination
/// marker target.
///
/// Implementation notes:
/// - This spawns a dedicated thread and blocks on X11 events.
/// - The thread exits when it loses selection ownership.
/// - INCR transfers are not implemented; callers should enforce reasonable size caps.
pub fn spawn_clipboard_owner(items: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    thread::Builder::new()
        .name("mcr-x11-owner".to_string())
        .spawn(move || {
            if let Err(e) = run_owner(items) {
                eprintln!("mcr-x11-owner: {:#}", e);
            }
        })
        .context("spawn x11 owner thread")?;
    Ok(())
}

fn run_owner(items: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    let (conn, screen_num) = RustConnection::connect(None).context("connect X11")?;
    let screen = &conn.setup().roots[screen_num];

    // Maximum X11 request length is expressed in 4-byte units.
    // We use it to decide when to switch to INCR. Keep a little headroom.
    // This is important: many X servers default to 65535 units (~256KiB).
    let max_req_bytes = (conn.setup().maximum_request_length as usize).saturating_mul(4);
    let max_direct_bytes = max_req_bytes.saturating_sub(1024).max(8 * 1024);

    let win: Window = conn.generate_id().context("gen window id")?;
    let cw = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
    conn.create_window(
        0,
        win,
        screen.root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        COPY_FROM_PARENT,
        &cw,
    )
    .context("create window")?;

    let clipboard = intern_atom(&conn, "CLIPBOARD")?;
    let targets_atom = intern_atom(&conn, "TARGETS")?;

    // Intern target atoms and keep payloads.
    let mut payloads: BTreeMap<Atom, Vec<u8>> = BTreeMap::new();
    for (name, bytes) in items {
        let a = intern_atom(&conn, &name)?;
        payloads.insert(a, bytes);
    }

    // We must always support TARGETS.
    payloads.entry(targets_atom).or_insert_with(Vec::new);

    // Become the clipboard owner.
    conn.set_selection_owner(win, clipboard, CURRENT_TIME)
        .context("set_selection_owner")?;
    conn.flush().ok();

    loop {
        let ev = conn.wait_for_event().context("wait_for_event")?;
        match ev {
            Event::SelectionRequest(req) => {
                handle_selection_request(&conn, win, clipboard, &payloads, req, max_direct_bytes)?;
            }
            Event::SelectionClear(_) => {
                // Lost ownership; exit.
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

fn intern_atom<C: Connection>(conn: &C, name: &str) -> anyhow::Result<Atom> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .context("intern_atom")?
        .reply()
        .context("intern_atom reply")?
        .atom)
}

fn handle_selection_request<C: Connection>(
    conn: &C,
    owner_window: Window,
    clipboard: Atom,
    payloads: &BTreeMap<Atom, Vec<u8>>,
    req: SelectionRequestEvent,
    max_direct_bytes: usize,
) -> anyhow::Result<()> {
    let targets_atom = intern_atom(conn, "TARGETS")?;
    let multiple_atom = intern_atom(conn, "MULTIPLE")?;
    let timestamp_atom = intern_atom(conn, "TIMESTAMP")?;
    let incr_atom = intern_atom(conn, "INCR")?;

    let mut property = req.property;
    if property == u32::from(AtomEnum::NONE) {
        // ICCCM: if property is None, use target.
        property = req.target;
    }

    if req.target == multiple_atom {
        // ICCCM MULTIPLE: requestor property contains pairs (target, property).
        // For each pair we try to satisfy the request by placing data into that property.
        // On failure, we set the property's atom to None in the returned list.
        // NOTE: We intentionally do not run INCR transfers inside MULTIPLE to keep complexity low.
        // Large entries are marked as failed; clients can retry with a direct request.
        let pairs = match get_atom_pairs(conn, req.requestor, property)? {
            Some(p) => p,
            None => {
                send_selection_notify(conn, req, u32::from(AtomEnum::NONE))?;
                conn.flush().ok();
                return Ok(());
            }
        };

        let mut out_pairs: Vec<(Atom, Atom)> = Vec::with_capacity(pairs.len());
        for (t, p) in pairs {
            if p == u32::from(AtomEnum::NONE) {
                out_pairs.push((t, p));
                continue;
            }

            if t == targets_atom {
                // TARGETS within MULTIPLE.
                let mut atoms: Vec<Atom> = payloads.keys().copied().collect();
                atoms.push(targets_atom);
                atoms.push(timestamp_atom);
                atoms.sort_unstable();
                atoms.dedup();
                let bytes: Vec<u8> = atoms.iter().flat_map(|a| a.to_ne_bytes()).collect();
                let ok = conn
                    .change_property(
                        PropMode::REPLACE,
                        req.requestor,
                        p,
                        AtomEnum::ATOM,
                        32,
                        bytes.len() as u32 / 4,
                        &bytes,
                    )
                    .is_ok();
                out_pairs.push((t, if ok { p } else { u32::from(AtomEnum::NONE) }));
                continue;
            }

            if t == timestamp_atom {
                let ts: u32 = 0;
                let ok = conn
                    .change_property(
                        PropMode::REPLACE,
                        req.requestor,
                        p,
                        AtomEnum::INTEGER,
                        32,
                        1,
                        &ts.to_ne_bytes(),
                    )
                    .is_ok();
                out_pairs.push((t, if ok { p } else { u32::from(AtomEnum::NONE) }));
                continue;
            }

            // Normal target within MULTIPLE.
            let Some(bytes) = payloads.get(&t) else {
                out_pairs.push((t, u32::from(AtomEnum::NONE)));
                continue;
            };

            // Avoid enormous ChangeProperty; mark failed and let requestor retry.
            if bytes.len() > max_direct_bytes {
                out_pairs.push((t, u32::from(AtomEnum::NONE)));
                continue;
            }

            let ok = conn
                .change_property(
                    PropMode::REPLACE,
                    req.requestor,
                    p,
                    t,
                    8,
                    bytes.len() as u32,
                    bytes,
                )
                .is_ok();
            out_pairs.push((t, if ok { p } else { u32::from(AtomEnum::NONE) }));
        }

        // Write back the (possibly modified) pairs.
        let mut out_bytes: Vec<u8> = Vec::with_capacity(out_pairs.len() * 8);
        for (t, p) in out_pairs {
            out_bytes.extend_from_slice(&t.to_ne_bytes());
            out_bytes.extend_from_slice(&p.to_ne_bytes());
        }
        let _ = conn.change_property(
            PropMode::REPLACE,
            req.requestor,
            property,
            AtomEnum::ATOM,
            32,
            (out_bytes.len() as u32) / 4,
            &out_bytes,
        );
        send_selection_notify(conn, req, property)?;
        conn.flush().ok();
        return Ok(());
    }

    if req.target == targets_atom {
        // Provide list of supported targets.
        let mut atoms: Vec<Atom> = payloads.keys().copied().collect();
        // Common extra targets.
        atoms.push(targets_atom);
        atoms.push(timestamp_atom);
        atoms.sort_unstable();
        atoms.dedup();

        let bytes: Vec<u8> = atoms
            .iter()
            .flat_map(|a| a.to_ne_bytes())
            .collect();

        conn.change_property(
            PropMode::REPLACE,
            req.requestor,
            property,
            AtomEnum::ATOM,
            32,
            bytes.len() as u32 / 4,
            &bytes,
        )
        .context("change_property TARGETS")?;
        send_selection_notify(conn, req, property)?;
        conn.flush().ok();
        return Ok(());
    }

    if req.target == timestamp_atom {
        // Best-effort: set 0.
        let ts: u32 = 0;
        conn.change_property(
            PropMode::REPLACE,
            req.requestor,
            property,
            AtomEnum::INTEGER,
            32,
            1,
            &ts.to_ne_bytes(),
        )
        .context("change_property TIMESTAMP")?;
        send_selection_notify(conn, req, property)?;
        conn.flush().ok();
        return Ok(());
    }

    // Normal targets.
    if let Some(bytes) = payloads.get(&req.target) {
        if bytes.len() <= max_direct_bytes {
            // Use target atom itself as the property type.
            conn.change_property(
                PropMode::REPLACE,
                req.requestor,
                property,
                req.target,
                8,
                bytes.len() as u32,
                bytes,
            )
            .context("change_property target")?;
            send_selection_notify(conn, req, property)?;
            conn.flush().ok();
            return Ok(());
        }

        // Large payload: use INCR.
        // 1) Place INCR + total length in the requestor property
        // 2) Notify
        // 3) Wait for requestor to delete the property, then stream chunks.
        let total_len: u32 = bytes.len().try_into().unwrap_or(u32::MAX);
        conn.change_property(
            PropMode::REPLACE,
            req.requestor,
            property,
            incr_atom,
            32,
            1,
            &total_len.to_ne_bytes(),
        )
        .context("change_property INCR")?;
        send_selection_notify(conn, req, property)?;
        conn.flush().ok();

        // Best-effort: listen for PropertyNotify on the requestor window.
        let _ = conn.change_window_attributes(
            req.requestor,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        );
        conn.flush().ok();

        incr_transfer(conn, owner_window, clipboard, req.requestor, property, req.target, bytes)?;
        conn.flush().ok();
        return Ok(());
    }

    // Unsupported.
    // Respond with property = None.
    send_selection_notify(conn, req, u32::from(AtomEnum::NONE))?;
    conn.flush().ok();
    // Ensure we still own the selection.
    let _ = conn.set_selection_owner(owner_window, clipboard, CURRENT_TIME);
    Ok(())
}

fn send_selection_notify<C: Connection>(
    conn: &C,
    req: SelectionRequestEvent,
    property: Atom,
) -> anyhow::Result<()> {
    let ev = SelectionNotifyEvent {
        response_type: xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property,
    };
    conn.send_event(false, req.requestor, EventMask::NO_EVENT, ev)
        .context("send_event SelectionNotify")?;
    Ok(())
}

fn get_atom_pairs<C: Connection>(
    conn: &C,
    requestor: Window,
    property: Atom,
) -> anyhow::Result<Option<Vec<(Atom, Atom)>>> {
    // MULTIPLE data is ATOM 32 pairs.
    let reply = match conn.get_property(false, requestor, property, AtomEnum::ATOM, 0, u32::MAX) {
        Ok(c) => c.reply(),
        Err(_) => return Ok(None),
    };
    let reply = match reply {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    if reply.format != 32 {
        return Ok(None);
    }

    let mut atoms: Vec<Atom> = Vec::new();
    for chunk in reply.value.chunks_exact(4) {
        atoms.push(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if atoms.len() % 2 != 0 {
        return Ok(None);
    }

    let mut pairs: Vec<(Atom, Atom)> = Vec::with_capacity(atoms.len() / 2);
    for i in (0..atoms.len()).step_by(2) {
        pairs.push((atoms[i], atoms[i + 1]));
    }
    Ok(Some(pairs))
}

fn incr_transfer<C: Connection>(
    conn: &C,
    owner_window: Window,
    clipboard: Atom,
    requestor: Window,
    property: Atom,
    target: Atom,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let start = Instant::now();
    let mut offset: usize = 0;

    loop {
        if start.elapsed() > INCR_TIMEOUT {
            // Abort transfer; ensure we still own the selection.
            let _ = conn.set_selection_owner(owner_window, clipboard, CURRENT_TIME);
            return Ok(());
        }

        if let Ok(Some(ev)) = conn.poll_for_event() {
            match ev {
                Event::SelectionClear(_) => {
                    // Lost ownership.
                    return Ok(());
                }
                Event::PropertyNotify(PropertyNotifyEvent { window, atom, state, .. }) => {
                    if window != requestor || atom != property {
                        continue;
                    }
                    // Requestor signals readiness by deleting the property.
                    if state != xproto::Property::DELETE {
                        continue;
                    }

                    if offset >= bytes.len() {
                        // Send zero-length property to signal end.
                        let empty: [u8; 0] = [];
                        let _ = conn.change_property(
                            PropMode::REPLACE,
                            requestor,
                            property,
                            target,
                            8,
                            0,
                            &empty,
                        );
                        conn.flush().ok();
                        return Ok(());
                    }

                    let end = (offset + INCR_CHUNK_BYTES).min(bytes.len());
                    let chunk = &bytes[offset..end];
                    offset = end;
                    conn.change_property(
                        PropMode::REPLACE,
                        requestor,
                        property,
                        target,
                        8,
                        chunk.len() as u32,
                        chunk,
                    )
                    .ok();
                    conn.flush().ok();
                }
                _ => {}
            }
        } else {
            // Avoid spinning.
            thread::sleep(Duration::from_millis(2));
        }
    }
}
