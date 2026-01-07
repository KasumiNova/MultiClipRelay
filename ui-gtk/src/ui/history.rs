use glib::clone;
use gtk4::prelude::*;
use serde::Deserialize;

use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::i18n::{t, Lang, K};

#[derive(Debug, Clone, Deserialize)]
struct HistoryEvent {
    ts_ms: Option<u64>,
    dir: Option<String>,
    room: Option<String>,
    relay: Option<String>,
    local_device_id: Option<String>,
    remote_device_id: Option<String>,
    kind: Option<String>,
    mime: Option<String>,
    name: Option<String>,
    bytes: Option<usize>,
    sha256: Option<String>,
}

pub fn history_path() -> PathBuf {
    // Match node default_data_dir():
    // - $XDG_DATA_HOME/multicliprelay (or ~/.local/share/multicliprelay)
    let base = dirs::data_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    match base {
        Some(d) => d.join("multicliprelay").join("history.jsonl"),
        None => PathBuf::from("/tmp")
            .join("multicliprelay")
            .join("history.jsonl"),
    }
}

fn read_tail_lines(path: &PathBuf, max_bytes: u64, max_lines: usize) -> Vec<String> {
    let mut f = match File::open(path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        let _ = f.seek(SeekFrom::Start(start));
    }

    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }

    let s = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = s.lines().collect();

    // If we started in the middle of a line, drop the first partial line.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let take_from = lines.len().saturating_sub(max_lines);
    lines[take_from..]
        .iter()
        .map(|l| l.to_string())
        .filter(|l| !l.trim().is_empty())
        .collect()
}

fn fmt_ts(ts_ms: Option<u64>) -> String {
    let Some(ms) = ts_ms else {
        return "?".to_string();
    };
    let sec = (ms / 1000) as i64;
    let msec = (ms % 1000) as i32;

    if let Ok(dt) = glib::DateTime::from_unix_local(sec) {
        // Example: 2026-01-07 12:34:56.789
        let base = dt.format("%F %T").unwrap_or_else(|_| "?".into());
        format!("{base}.{:03}", msec)
    } else {
        ms.to_string()
    }
}

fn format_event(e: HistoryEvent) -> String {
    let ts = fmt_ts(e.ts_ms);
    let dir = e.dir.unwrap_or_else(|| "?".into());
    let kind = e.kind.unwrap_or_else(|| "?".into());
    let bytes = e.bytes.map(|b| b.to_string()).unwrap_or_else(|| "?".into());
    let room = e.room.unwrap_or_default();

    let peer = e
        .remote_device_id
        .or(e.local_device_id)
        .unwrap_or_else(|| "?".into());

    let mut extra = Vec::new();
    if let Some(name) = e.name {
        if !name.is_empty() {
            extra.push(format!("name={}", name));
        }
    }
    if let Some(mime) = e.mime {
        if !mime.is_empty() {
            extra.push(format!("mime={}", mime));
        }
    }
    if let Some(sha) = e.sha256 {
        if !sha.is_empty() {
            let short = if sha.len() > 8 { &sha[..8] } else { &sha };
            extra.push(format!("sha={}", short));
        }
    }
    if let Some(relay) = e.relay {
        if !relay.is_empty() {
            extra.push(format!("relay={}", relay));
        }
    }

    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("  {}", extra.join(" "))
    };

    format!("[{ts}] {dir} peer={peer} kind={kind} bytes={bytes} room={room}{extra}")
}

pub fn install_history_refresh(
    history_buf: gtk4::TextBuffer,
    clear_btn: gtk4::Button,
    log_tx: mpsc::Sender<String>,
    lang_state: Arc<Mutex<Lang>>,
) {
    // Button: clear history file.
    clear_btn.connect_clicked(clone!(@strong history_buf, @strong log_tx => move |_| {
        let p = history_path();
        match std::fs::write(&p, "") {
            Ok(()) => {
                history_buf.set_text("");
                let _ = log_tx.send(format!("cleared history: {}", p.display()));
            }
            Err(e) => {
                let _ = log_tx.send(format!("failed to clear history: {e}"));
            }
        }
    }));

    // Periodic refresh.
    let last_render: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    glib::timeout_add_local(
        Duration::from_millis(800),
        clone!(@weak history_buf, @strong log_tx, @strong lang_state, @strong last_render => @default-return glib::ControlFlow::Break, move || {
            let p = history_path();
            let lines = read_tail_lines(&p, 1024 * 1024, 250);

            let mut out_lines: Vec<String> = Vec::new();
            for l in lines {
                match serde_json::from_str::<HistoryEvent>(&l) {
                    Ok(ev) => out_lines.push(format_event(ev)),
                    Err(_) => out_lines.push(l),
                }
            }

            if out_lines.is_empty() {
                // Keep it friendly: show a hint when no history exists yet.
                let lang = *lang_state.lock().unwrap();
                let hint = t(lang, K::HistoryEmptyHint);
                out_lines.push(hint.to_string());
            }

            let rendered = out_lines.join("\n");
            if rendered != *last_render.borrow() {
                history_buf.set_text(&rendered);
                *last_render.borrow_mut() = rendered;
            }

            // Silence errors: history is best-effort.
            let _ = &log_tx;
            glib::ControlFlow::Continue
        }),
    );
}
