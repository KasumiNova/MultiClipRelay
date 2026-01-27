use gtk4::prelude::*;
use gtk4::pango::{TabAlign, TabArray, SCALE};

use std::process::Command;
use std::rc::Rc;
use std::cell::RefCell;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::i18n::{t, Lang, K};
use crate::systemd;

fn format_journal_as_columns(text: &str) -> String {
    // Input (short-iso):
    //   2026-01-26T17:52:56+08:00 host unit[pid]: message...
    // Render as tab-separated columns:
    //   time<TAB>unit<TAB>message
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let mut it = line.splitn(4, ' ');
        let ts = it.next();
        let _host = it.next();
        let unit = it.next();
        let rest = it.next();

        if let (Some(ts), Some(unit), Some(rest)) = (ts, unit, rest) {
            let unit = unit.trim_end_matches(':');
            let msg = rest.trim_start();
            out.push_str(ts);
            out.push('\t');
            out.push_str(unit);
            out.push('\t');
            out.push_str(msg);
            out.push('\n');
        } else {
            // Fallback: keep original line in message column.
            out.push_str("\t\t");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn read_wl_clipboard_journal() -> String {
    let out = Command::new("journalctl")
        .args([
            "--user",
            "-u",
            systemd::UNIT_WL_WATCH,
            "-u",
            systemd::UNIT_WL_APPLY,
            "-u",
            systemd::UNIT_X11_SYNC,
            "-n",
            "200",
            "--no-pager",
            "-o",
            "short-iso",
        ])
        .output();

    match out {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).to_string();
            format_journal_as_columns(&s)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            format!(
                "journalctl failed (exit={}):\n{}",
                out.status.code().unwrap_or(-1),
                stderr
            )
        }
        Err(e) => format!("failed to run journalctl: {e:?}"),
    }
}

pub fn open_wl_clipboard_logs_window(
    app: &gtk4::Application,
    parent: &gtk4::ApplicationWindow,
    lang: Lang,
) {
    // Keep it minimal: a read-only TextView that refreshes periodically.
    let window = gtk4::Window::builder()
        .application(app)
        .title(t(lang, K::WindowWlClipboardLogs))
        .default_width(920)
        .default_height(560)
        .transient_for(parent)
        .build();

    let buf = gtk4::TextBuffer::new(None);
    let view = gtk4::TextView::builder()
        .buffer(&buf)
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .build();

    // Make it look like a table via tab stops.
    // Columns: time | unit | message
    let mut tabs = TabArray::new(3, true);
    // Positions are in Pango units (1/1024th of a point). Roughly:
    //  - time column ~ 30 chars
    //  - encourage message to use remaining width
    tabs.set_tab(0, TabAlign::Left, 320 * SCALE);
    tabs.set_tab(1, TabAlign::Left, 560 * SCALE);
    tabs.set_tab(2, TabAlign::Left, 800 * SCALE);
    view.set_tabs(&tabs);

    let scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&view)
        .build();

    window.set_child(Some(&scroll));

    if !systemd::enabled_from_env_or_auto() {
        buf.set_text("systemd user service is not available; logs are shown in the main Logs tab when not using systemd.");
        window.show();
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel::<String>();

    let last_rendered: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Poll the receiver on the main thread.
    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(@weak buf, @weak view, @weak scroll, @strong last_rendered => @default-return glib::ControlFlow::Break, move || {
            // Keep only the latest snapshot to avoid UI churn.
            let mut latest: Option<String> = None;
            while let Ok(s) = rx.try_recv() {
                latest = Some(s);
            }
            if let Some(s) = latest {
                // Do not reset the view if nothing changed; this prevents periodic scroll jumps.
                if last_rendered.borrow().as_deref() == Some(&s) {
                    return glib::ControlFlow::Continue;
                }

                // Capture current scroll state.
                let vadj = scroll.vadjustment();
                let old_value = vadj.value();
                let old_upper = vadj.upper();
                let old_page = vadj.page_size();
                let at_bottom = old_value + old_page >= (old_upper - 2.0).max(0.0);

                buf.set_text(&s);
                *last_rendered.borrow_mut() = Some(s);

                // After buffer update, adjustments are updated asynchronously; fix scroll on idle.
                glib::idle_add_local_once(glib::clone!(@weak view, @weak scroll => move || {
                    let vadj = scroll.vadjustment();
                    if at_bottom {
                        // Follow tail.
                        let buffer = view.buffer();
                        let mut end = buffer.end_iter();
                        view.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
                    } else {
                        // Restore prior scroll offset as best as possible.
                        let upper = vadj.upper();
                        let page = vadj.page_size();
                        let max_value = (upper - page).max(0.0);
                        let v = old_value.min(max_value);
                        // If the content shrank a lot, keep relative position instead of snapping.
                        if old_upper > 1.0 && upper > 1.0 {
                            let frac = (old_value / old_upper).clamp(0.0, 1.0);
                            let target = (frac * upper).min(max_value);
                            vadj.set_value(target);
                        } else {
                            vadj.set_value(v);
                        }
                    }
                }));
            }
            glib::ControlFlow::Continue
        }),
    );

    let alive = Arc::new(AtomicBool::new(true));
    window.connect_close_request(glib::clone!(@strong alive => @default-return glib::Propagation::Proceed, move |_| {
        alive.store(false, Ordering::Relaxed);
        glib::Propagation::Proceed
    }));

    std::thread::spawn(move || {
        // Emit once immediately, then refresh.
        while alive.load(Ordering::Relaxed) {
            let s = read_wl_clipboard_journal();
            if tx.send(s).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    });

    window.show();
}
