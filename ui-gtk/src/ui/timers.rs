use glib::clone;
use gtk4::prelude::*;
use gtk4::gio;

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::procs::{terminate_child, Procs};

use super::table::keep_scroll_tail;

fn now_hms_millis() -> String {
    if let Ok(dt) = glib::DateTime::now_local() {
        dt.format("%T")
            .map(|s| s.to_string())
            .unwrap_or_else(|_| "?".into())
    } else {
        "?".to_string()
    }
}

fn split_prefix(line: &str) -> (String, String) {
    // Expected examples:
    //   "[node:stdout] hello"
    //   "started relay"
    if let Some(rest) = line.strip_prefix('[') {
        if let Some((p, m)) = rest.split_once("] ") {
            return (p.to_string(), m.to_string());
        }
        if let Some((p, m)) = rest.split_once("]") {
            return (p.to_string(), m.trim_start().to_string());
        }
    }
    ("ui".to_string(), line.to_string())
}

pub fn install_log_drain(
    log_rx: mpsc::Receiver<String>,
    store: gio::ListStore,
    scroll: gtk4::ScrolledWindow,
) {
    // glib 0.19 / gtk4: use a main-thread timeout to drain logs from std::sync::mpsc.
    glib::timeout_add_local(
        Duration::from_millis(50),
        clone!(@strong store, @weak scroll => @default-return glib::ControlFlow::Break, move || {
            // Capture scroll state once per tick.
            let vadj = scroll.vadjustment();
            let old_value = vadj.value();
            let old_upper = vadj.upper();
            let old_page = vadj.page_size();
            let at_bottom = old_value + old_page >= (old_upper - 2.0).max(0.0);

            let mut appended = 0usize;
            while let Ok(line) = log_rx.try_recv() {
                let ts = now_hms_millis();
                let (src, msg) = split_prefix(&line);
                let row = format!("{}\t{}\t{}", ts, src, msg);
                store.append(&gtk4::StringObject::new(&row));
                appended += 1;
            }

            // Prevent unbounded growth.
            let max_rows: u32 = 5000;
            let n = store.n_items();
            if n > max_rows {
                // Drop oldest rows.
                let drop = n - max_rows;
                for _ in 0..drop {
                    store.remove(0);
                }
            }

            if appended > 0 {
                keep_scroll_tail(&scroll, at_bottom, old_value, old_upper);
            }
            glib::ControlFlow::Continue
        }),
    );
}

pub fn install_prune_timer(
    procs: Arc<Mutex<Procs>>,
    use_systemd: bool,
    log_tx: mpsc::Sender<String>,
    update_services_ui: std::rc::Rc<dyn Fn()>,
) {
    glib::timeout_add_local(
        Duration::from_millis(500),
        clone!(@strong procs, @strong log_tx, @strong update_services_ui => @default-return glib::ControlFlow::Continue, move || {
            let mut changed = false;
            {
                let mut p = procs.lock().unwrap();

                let relay_exited = p.relay.as_mut().and_then(|c| c.try_wait().ok()).flatten().is_some();
                if relay_exited {
                    p.relay = None;
                    changed = true;
                    let _ = log_tx.send("relay exited".into());
                }

                let watch_exited = p.watch.as_mut().and_then(|c| c.try_wait().ok()).flatten().is_some();
                if watch_exited {
                    p.watch = None;
                    changed = true;
                    let _ = log_tx.send("wl-watch exited".into());
                }

                let apply_exited = p.apply.as_mut().and_then(|c| c.try_wait().ok()).flatten().is_some();
                if apply_exited {
                    p.apply = None;
                    changed = true;
                    let _ = log_tx.send("wl-apply exited".into());
                }

                let x11_exited = p.x11.as_mut().and_then(|c| c.try_wait().ok()).flatten().is_some();
                if x11_exited {
                    p.x11 = None;
                    changed = true;
                    let _ = log_tx.send("x11-sync exited".into());
                }
            }
            if changed || use_systemd {
                update_services_ui();
            }
            glib::ControlFlow::Continue
        }),
    );
}

pub fn install_close_handler(
    window: &gtk4::ApplicationWindow,
    use_systemd: bool,
    procs: Arc<Mutex<Procs>>,
    log_tx: mpsc::Sender<String>,
) {
    window.connect_close_request(clone!(@strong procs, @strong log_tx => @default-return glib::Propagation::Proceed, move |_| {
        if use_systemd {
            return glib::Propagation::Proceed;
        }
        let mut p = procs.lock().unwrap();
        // Don't block the UI thread: terminate in background.
        if let Some(c) = p.x11.take() { terminate_child(c, "node x11-sync", log_tx.clone()); }
        if let Some(c) = p.watch.take() { terminate_child(c, "node wl-watch", log_tx.clone()); }
        if let Some(c) = p.apply.take() { terminate_child(c, "node wl-apply", log_tx.clone()); }
        if let Some(c) = p.relay.take() { terminate_child(c, "relay", log_tx.clone()); }
        glib::Propagation::Proceed
    }));
}
