use gtk4::prelude::*;

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::i18n::{t, Lang, K};
use crate::systemd;

use crate::ui::table::{keep_scroll_tail, make_tabbed_table, ColumnSpec};

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

#[allow(dead_code)]
pub fn open_wl_clipboard_logs_window(
    app: &gtk4::Application,
    parent: &gtk4::ApplicationWindow,
    lang: Lang,
) {
    // Clipboard logs (systemd journal) - standalone window.
    let window = gtk4::Window::builder()
        .application(app)
        .title(t(lang, K::WindowWlClipboardLogs))
        .default_width(920)
        .default_height(560)
        .transient_for(parent)
        .build();
    let (root, alive) = build_wl_clipboard_logs_widget(lang);
    window.set_child(Some(&root));

    window.connect_close_request(glib::clone!(@strong alive => @default-return glib::Propagation::Proceed, move |_| {
        alive.store(false, Ordering::Relaxed);
        glib::Propagation::Proceed
    }));

    window.show();
}

pub fn build_wl_clipboard_logs_widget(lang: Lang) -> (gtk4::Widget, Arc<AtomicBool>) {
    if !systemd::enabled_from_env_or_auto() {
        let label = gtk4::Label::builder()
            .label("systemd user service is not available; clipboard journal logs are unavailable.")
            .wrap(true)
            .xalign(0.0)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        return (label.upcast::<gtk4::Widget>(), Arc::new(AtomicBool::new(false)));
    }

    let table = make_tabbed_table(&[
        ColumnSpec {
            title: "time",
            fixed_width: Some(240),
            expand: false,
            resizable: true,
            ellipsize: true,
        },
        ColumnSpec {
            title: "unit",
            fixed_width: Some(300),
            expand: false,
            resizable: true,
            ellipsize: true,
        },
        ColumnSpec {
            title: "message",
            fixed_width: None,
            expand: true,
            resizable: true,
            ellipsize: false,
        },
    ]);

    let root: gtk4::Widget = table.scroll.clone().upcast();
    let alive = Arc::new(AtomicBool::new(true));

    // Background reader -> UI model updater.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let last_rendered: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(@weak root, @strong table, @strong last_rendered => @default-return glib::ControlFlow::Break, move || {
            let mut latest: Option<String> = None;
            while let Ok(s) = rx.try_recv() {
                latest = Some(s);
            }
            if let Some(s) = latest {
                if last_rendered.borrow().as_deref() == Some(&s) {
                    return glib::ControlFlow::Continue;
                }

                let scroll = match root.clone().downcast::<gtk4::ScrolledWindow>() {
                    Ok(v) => v,
                    Err(_) => return glib::ControlFlow::Continue,
                };

                let vadj = scroll.vadjustment();
                let old_value = vadj.value();
                let old_upper = vadj.upper();
                let old_page = vadj.page_size();
                let at_bottom = old_value + old_page >= (old_upper - 2.0).max(0.0);

                table.store.remove_all();
                for line in s.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    table.store.append(&gtk4::StringObject::new(line));
                }
                keep_scroll_tail(&scroll, at_bottom, old_value, old_upper);
                *last_rendered.borrow_mut() = Some(s);
            }
            glib::ControlFlow::Continue
        }),
    );

    let alive_thread = alive.clone();
    std::thread::spawn(move || {
        while alive_thread.load(Ordering::Relaxed) {
            let s = read_wl_clipboard_journal();
            if tx.send(s).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    });

    let _ = lang;
    (root, alive)
}
