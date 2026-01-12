use anyhow::Context;
use std::collections::BTreeMap;
use std::thread;

use x11rb::{COPY_FROM_PARENT, CURRENT_TIME};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, Window, WindowClass,
};
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::protocol::{Event, xproto};
use x11rb::rust_connection::RustConnection;

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
                handle_selection_request(&conn, win, clipboard, &payloads, req)?;
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
) -> anyhow::Result<()> {
    let targets_atom = intern_atom(conn, "TARGETS")?;
    let multiple_atom = intern_atom(conn, "MULTIPLE")?;
    let timestamp_atom = intern_atom(conn, "TIMESTAMP")?;

    let mut property = req.property;
    if property == u32::from(AtomEnum::NONE) {
        // ICCCM: if property is None, use target.
        property = req.target;
    }

    // Handle MULTIPLE by declining (keeps implementation small).
    if req.target == multiple_atom {
        send_selection_notify(conn, req, u32::from(AtomEnum::NONE))?;
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
