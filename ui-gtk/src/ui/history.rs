use glib::clone;
use gtk4::prelude::*;
use gtk4::gio;
use serde::Deserialize;

use std::collections::BTreeMap;
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::i18n::{t, Lang, K};

use super::table::keep_scroll_tail;

#[derive(Debug, Clone)]
struct HistoryRow {
    /// A detail row shows the `extra` field only.
    is_detail: bool,
    ts: String,
    dir: String,
    name: String,
    peer: String,
    kind: String,
    bytes: String,
    extra: String,
    preview_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryEvent {
    ts_ms: Option<u64>,
    dir: Option<String>,
    room: Option<String>,
    relay: Option<String>,
    local_device_id: Option<String>,
    local_device_name: Option<String>,
    remote_device_id: Option<String>,
    remote_device_name: Option<String>,
    kind: Option<String>,
    mime: Option<String>,
    name: Option<String>,
    bytes: Option<usize>,
    sha256: Option<String>,
}

pub struct HistoryTable {
    pub store: gio::ListStore,
    #[allow(dead_code)]
    pub selection: gtk4::SingleSelection,
    #[allow(dead_code)]
    pub view: gtk4::ColumnView,
    pub scroll: gtk4::ScrolledWindow,
    pub columns: Vec<(String, gtk4::ColumnViewColumn)>,
}

fn data_dir_base() -> PathBuf {
    // Match node default_data_dir().
    let base = dirs::data_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    match base {
        Some(d) => d.join("multicliprelay"),
        None => PathBuf::from("/tmp").join("multicliprelay"),
    }
}

fn received_dir() -> PathBuf {
    data_dir_base().join("received")
}

fn safe_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

fn first_8(s: &str) -> &str {
    if s.len() >= 8 { &s[..8] } else { s }
}

fn is_tar_payload(name: &str, mime: Option<&str>) -> bool {
    mime == Some("application/x-tar") || name.to_ascii_lowercase().ends_with(".tar")
}

fn image_ext_from_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/jpg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn fmt_bytes(bytes: Option<usize>) -> String {
    let Some(b) = bytes else {
        return "?".to_string();
    };
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let v = b as f64;
    if v < KIB {
        format!("{} B", b)
    } else if v < MIB {
        format!("{:.1} KiB", v / KIB)
    } else if v < GIB {
        format!("{:.1} MiB", v / MIB)
    } else {
        format!("{:.1} GiB", v / GIB)
    }
}

fn preview_path_for(e: &HistoryEvent) -> Option<PathBuf> {
    let kind = e.kind.as_deref().unwrap_or("");
    let sha = e.sha256.as_deref()?;
    let sha8 = first_8(sha).to_string();

    if kind == "image" {
        let dir = received_dir().join(&sha8);
        if !dir.exists() {
            return None;
        }

        // Prefer png if present (we may store a converted fallback).
        let p_png = dir.join("image.png");
        if p_png.exists() {
            return Some(p_png);
        }

        if let Some(mime) = e.mime.as_deref() {
            if let Some(ext) = image_ext_from_mime(mime) {
                let p = dir.join(format!("image.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
        }

        // Fallback scan common extensions.
        for ext in ["png", "jpg", "jpeg", "webp", "gif"] {
            let p = dir.join(format!("image.{ext}"));
            if p.exists() {
                return Some(p);
            }
        }
        return None;
    }

    if kind != "file" {
        return None;
    }

    let name = e
        .name
        .as_deref()
        .unwrap_or("multicliprelay")
        .to_string();
    let safe = safe_for_filename(&name);

    // Mirror node/wl-apply receive paths.
    if is_tar_payload(&name, e.mime.as_deref()) {
        let stem = safe
            .trim_end_matches(".tar")
            .trim_end_matches(".TAR")
            .to_string();
        let out_dir = received_dir().join(format!("{}_{}", sha8, stem));
        if out_dir.exists() {
            return Some(out_dir);
        }
        return None;
    }

    let out_path = received_dir().join(&sha8).join(&safe);
    if out_path.exists() {
        Some(out_path)
    } else {
        None
    }
}

fn row_signature(rows: &[HistoryRow]) -> String {
    // Used to avoid pointless UI churn.
    // Include a preview-exists bit so the button state updates when the file disappears.
    let mut out = String::new();
    for r in rows {
        out.push_str(if r.is_detail { "D" } else { "M" });
        out.push('\t');
        out.push_str(&r.ts);
        out.push('\t');
        out.push_str(&r.dir);
        out.push('\t');
        out.push_str(&r.name);
        out.push('\t');
        out.push_str(&r.peer);
        out.push('\t');
        out.push_str(&r.kind);
        out.push('\t');
        out.push_str(&r.bytes);
        out.push('\t');
        out.push_str(&r.extra);
        out.push('\t');
        out.push_str(if r.preview_path.is_some() { "1" } else { "0" });
        out.push('\n');
    }
    out
}

pub fn history_path() -> PathBuf {
    // Match node default_data_dir():
    // - $XDG_DATA_HOME/multicliprelay (or ~/.local/share/multicliprelay)
    data_dir_base().join("history.jsonl")
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

fn format_event_rows(e: HistoryEvent) -> Vec<HistoryRow> {
    let preview_path = preview_path_for(&e);

    let ts = fmt_ts(e.ts_ms);
    let dir = e.dir.unwrap_or_else(|| "?".into());
    let kind = e.kind.unwrap_or_else(|| "?".into());
    let bytes = fmt_bytes(e.bytes);
    let room = e.room.unwrap_or_default();

    // Prefer human-friendly name for display, but keep peer id available as an optional column.
    let local_id = e.local_device_id.clone().unwrap_or_else(|| "?".into());
    let local_name = e.local_device_name.clone().unwrap_or_default();
    let remote_id = e.remote_device_id.clone().unwrap_or_else(|| "?".into());
    let remote_name = e.remote_device_name.clone().unwrap_or_default();

    let (name, peer) = match dir.as_str() {
        // recv: show remote name, peer column is remote id
        "recv" => {
            let n = if remote_name.trim().is_empty() {
                remote_id.clone()
            } else {
                remote_name
            };
            (n, remote_id)
        }
        // send: show local name, peer column is local id
        "send" => {
            let n = if local_name.trim().is_empty() {
                local_id.clone()
            } else {
                local_name
            };
            (n, local_id)
        }
        _ => {
            let n = if remote_name.trim().is_empty() {
                remote_id.clone()
            } else {
                remote_name
            };
            (n, remote_id)
        }
    };

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
        extra.join(" ")
    };

    let extra = if room.is_empty() {
        extra
    } else if extra.is_empty() {
        format!("room={room}")
    } else {
        format!("room={room} {extra}")
    };

    // Main row + optional detail row.
    // Detail row is meant to "own" the extra info so the main row stays compact.
    let mut rows = Vec::new();
    rows.push(HistoryRow {
        is_detail: false,
        ts,
        dir,
        name,
        peer,
        kind,
        bytes,
        extra: String::new(),
        preview_path,
    });

    if !extra.trim().is_empty() {
        rows.push(HistoryRow {
            is_detail: true,
            ts: String::new(),
            dir: String::new(),
            name: String::new(),
            peer: String::new(),
            kind: String::new(),
            bytes: String::new(),
            extra: format!("↳ {extra}"),
            preview_path: None,
        });
    }
    rows
}

fn col_visible(id: &str, cfg: &BTreeMap<String, bool>, default_visible: bool) -> bool {
    cfg.get(id).copied().unwrap_or(default_visible)
}

pub fn make_history_table(lang: Lang, columns_cfg: &BTreeMap<String, bool>) -> HistoryTable {
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk4::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    let view = gtk4::ColumnView::new(Some(selection.clone()));
    view.set_vexpand(true);
    view.set_hexpand(true);
    view.add_css_class("boxed-list");

    let make_text_col = |title: &str, getter: fn(&HistoryRow) -> &str, fixed_width: Option<i32>, expand: bool| {
        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup(move |_, list_item| {
            let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
            root.set_hexpand(true);
            root.set_halign(gtk4::Align::Fill);
            root.set_valign(gtk4::Align::Fill);
            root.add_css_class("mcr-cell");

            let label = gtk4::Label::builder()
                .xalign(0.0)
                .selectable(true)
                .single_line_mode(true)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .build();
            label.add_css_class("monospace");
            label.set_hexpand(true);
            label.set_halign(gtk4::Align::Fill);
            label.set_valign(gtk4::Align::Center);
            label.set_vexpand(false);
            root.append(&label);
            list_item.set_child(Some(&root));
        });
        factory.connect_bind(move |_, list_item| {
            let Some(item) = list_item.item() else { return; };
            let Ok(obj) = item.downcast::<glib::BoxedAnyObject>() else { return; };
            let row = obj.borrow::<HistoryRow>();
            let Some(child) = list_item.child() else { return; };
            let Ok(root) = child.downcast::<gtk4::Box>() else { return; };

            let Some(first) = root.first_child() else { return; };
            let Ok(label) = first.downcast::<gtk4::Label>() else { return; };
            label.set_text(getter(&row));
        });
        let col = gtk4::ColumnViewColumn::new(Some(title), Some(factory));
        if let Some(w) = fixed_width {
            col.set_fixed_width(w);
        }
        col.set_expand(expand);
        col.set_resizable(true);
        col
    };

    // time(detail) | dir | name | peer(id) | kind | bytes | preview
    let mut columns: Vec<(String, gtk4::ColumnViewColumn)> = Vec::new();

    // First column: shows time for main rows, shows extra for detail rows.
    // Make it expandable so detail text can use available width without pushing other columns.
    let time_factory = gtk4::SignalListItemFactory::new();
    time_factory.connect_setup(move |_, list_item| {
        let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        root.set_hexpand(true);
        root.set_halign(gtk4::Align::Fill);
        root.set_valign(gtk4::Align::Fill);
        root.add_css_class("mcr-cell");

        let label = gtk4::Label::builder()
            .xalign(0.0)
            .selectable(true)
            .single_line_mode(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .build();
        label.add_css_class("monospace");
        label.set_hexpand(true);
        label.set_halign(gtk4::Align::Fill);
        label.set_valign(gtk4::Align::Center);
        label.set_vexpand(false);
        root.append(&label);
        list_item.set_child(Some(&root));
    });
    time_factory.connect_bind(move |_, list_item| {
        let Some(item) = list_item.item() else { return; };
        let Ok(obj) = item.downcast::<glib::BoxedAnyObject>() else { return; };
        let row = obj.borrow::<HistoryRow>();
        let Some(child) = list_item.child() else { return; };
        let Ok(root) = child.downcast::<gtk4::Box>() else { return; };

        let Some(first) = root.first_child() else { return; };
        let Ok(label) = first.downcast::<gtk4::Label>() else { return; };

        // Keep both main and detail rows single-line for consistent (compressed) height.
        // Full extra text is available via tooltip.
        if row.is_detail {
            label.set_text(&row.extra);
            label.set_tooltip_text(Some(&row.extra));
            label.set_single_line_mode(true);
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        } else {
            label.set_text(&row.ts);
            label.set_tooltip_text(None);
            label.set_single_line_mode(true);
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        }
    });
    let time_col = gtk4::ColumnViewColumn::new(Some("time"), Some(time_factory));
    time_col.set_fixed_width(220);
    time_col.set_expand(true);
    time_col.set_resizable(true);
    time_col.set_visible(col_visible("time", columns_cfg, true));
    view.append_column(&time_col);
    columns.push(("time".to_string(), time_col));

    let dir_col = make_text_col("dir", |r| &r.dir, Some(70), false);
    dir_col.set_visible(col_visible("dir", columns_cfg, true));
    view.append_column(&dir_col);
    columns.push(("dir".to_string(), dir_col));

    let name_col = make_text_col("name", |r| &r.name, Some(160), false);
    name_col.set_visible(col_visible("name", columns_cfg, true));
    view.append_column(&name_col);
    columns.push(("name".to_string(), name_col));

    let peer_col = make_text_col("peer", |r| &r.peer, Some(140), false);
    // Default hidden (IDs are noisy). Can be enabled via column settings.
    peer_col.set_visible(col_visible("peer", columns_cfg, false));
    view.append_column(&peer_col);
    columns.push(("peer".to_string(), peer_col));

    let kind_col = make_text_col("kind", |r| &r.kind, Some(90), false);
    kind_col.set_visible(col_visible("kind", columns_cfg, true));
    view.append_column(&kind_col);
    columns.push(("kind".to_string(), kind_col));

    let bytes_col = make_text_col("bytes", |r| &r.bytes, Some(110), false);
    bytes_col.set_visible(col_visible("bytes", columns_cfg, true));
    view.append_column(&bytes_col);
    columns.push(("bytes".to_string(), bytes_col));

    // preview button column
    let preview_factory = gtk4::SignalListItemFactory::new();
    preview_factory.connect_setup(move |_, list_item| {
        let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        root.set_hexpand(true);
        root.set_halign(gtk4::Align::Fill);
        root.set_valign(gtk4::Align::Fill);
        root.add_css_class("mcr-cell");

        let btn = gtk4::Button::with_label(match lang {
            Lang::ZhCn => "预览",
            Lang::En => "Open",
        });
        btn.add_css_class("flat");
        btn.set_valign(gtk4::Align::Center);
        btn.set_vexpand(false);
        btn.connect_clicked(|b| {
            let Some(p) = b.tooltip_text().map(|s| s.to_string()) else {
                return;
            };
            if p.trim().is_empty() {
                return;
            }
            let _ = std::process::Command::new("xdg-open").arg(p).spawn();
        });
        root.append(&btn);
        list_item.set_child(Some(&root));
    });
    preview_factory.connect_bind(move |_, list_item| {
        let Some(item) = list_item.item() else { return; };
        let Ok(obj) = item.downcast::<glib::BoxedAnyObject>() else { return; };
        let row = obj.borrow::<HistoryRow>();
        let Some(child) = list_item.child() else { return; };
        let Ok(root) = child.downcast::<gtk4::Box>() else { return; };

        let Some(first) = root.first_child() else { return; };
        let Ok(btn) = first.downcast::<gtk4::Button>() else { return; };

        // Show only on main rows; enable only when the target exists.
        btn.set_visible(!row.is_detail);
        let can = row.preview_path.is_some();
        btn.set_sensitive(can);
        if can {
            let p = row.preview_path.as_ref().unwrap();
            btn.set_tooltip_text(Some(&p.display().to_string()));
        } else {
            btn.set_tooltip_text(None);
        }
    });
    let preview_col = gtk4::ColumnViewColumn::new(Some(""), Some(preview_factory));
    preview_col.set_fixed_width(70);
    preview_col.set_expand(false);
    preview_col.set_resizable(false);
    preview_col.set_visible(col_visible("preview", columns_cfg, true));
    view.append_column(&preview_col);
    columns.push(("preview".to_string(), preview_col.clone()));

    let scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .child(&view)
        .build();

    HistoryTable {
        store,
        selection,
        view,
        scroll,
        columns,
    }
}

pub fn install_history_refresh(
    store: gio::ListStore,
    scroll: gtk4::ScrolledWindow,
    clear_btn: gtk4::Button,
    log_tx: mpsc::Sender<String>,
    lang_state: Arc<Mutex<Lang>>,
) {
    // Button: clear history file.
    clear_btn.connect_clicked(clone!(@strong store, @strong log_tx => move |_| {
        let p = history_path();
        match std::fs::write(&p, "") {
            Ok(()) => {
                store.remove_all();
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
        clone!(@weak scroll, @strong store, @strong log_tx, @strong lang_state, @strong last_render => @default-return glib::ControlFlow::Break, move || {
            let p = history_path();
            let lines = read_tail_lines(&p, 1024 * 1024, 250);

            let mut rows: Vec<HistoryRow> = Vec::new();
            for l in lines {
                match serde_json::from_str::<HistoryEvent>(&l) {
                    Ok(ev) => rows.extend(format_event_rows(ev)),
                    Err(_) => {
                        // Keep raw line as a detail row.
                        rows.push(HistoryRow {
                            is_detail: true,
                            ts: String::new(),
                            dir: String::new(),
                            name: String::new(),
                            peer: String::new(),
                            kind: String::new(),
                            bytes: String::new(),
                            extra: format!("↳ {l}"),
                            preview_path: None,
                        });
                    }
                }
            }

            if rows.is_empty() {
                // Keep it friendly: show a hint when no history exists yet.
                let lang = *lang_state.lock().unwrap();
                let hint = t(lang, K::HistoryEmptyHint).to_string();
                rows.push(HistoryRow {
                    is_detail: true,
                    ts: String::new(),
                    dir: String::new(),
                    name: String::new(),
                    peer: String::new(),
                    kind: String::new(),
                    bytes: String::new(),
                    extra: hint,
                    preview_path: None,
                });
            }

            let rendered = row_signature(&rows);
            if rendered != *last_render.borrow() {
                // Capture scroll state.
                let vadj = scroll.vadjustment();
                let old_value = vadj.value();
                let old_upper = vadj.upper();
                let old_page = vadj.page_size();
                let at_bottom = old_value + old_page >= (old_upper - 2.0).max(0.0);

                store.remove_all();
                for r in rows {
                    store.append(&glib::BoxedAnyObject::new(r));
                }

                keep_scroll_tail(&scroll, at_bottom, old_value, old_upper);
                *last_render.borrow_mut() = rendered;
            }

            // Silence errors: history is best-effort.
            let _ = &log_tx;
            glib::ControlFlow::Continue
        }),
    );
}
