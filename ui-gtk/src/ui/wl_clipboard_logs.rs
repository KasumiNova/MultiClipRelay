use gtk4::prelude::*;

use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::i18n::{t, Lang, K};
use crate::systemd;

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
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
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
        .build();
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

    // Poll the receiver on the main thread.
    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(@weak buf => @default-return glib::ControlFlow::Break, move || {
            // Keep only the latest snapshot to avoid UI churn.
            let mut latest: Option<String> = None;
            while let Ok(s) = rx.try_recv() {
                latest = Some(s);
            }
            if let Some(s) = latest {
                buf.set_text(&s);
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
