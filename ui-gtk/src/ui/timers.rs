use glib::clone;
use gtk4::prelude::*;

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use crate::procs::{terminate_child, Procs};

pub fn install_log_drain(log_rx: mpsc::Receiver<String>, log_buf: gtk4::TextBuffer) {
    // glib 0.19 / gtk4: use a main-thread timeout to drain logs from std::sync::mpsc.
    glib::timeout_add_local(
        Duration::from_millis(50),
        clone!(@weak log_buf => @default-return glib::ControlFlow::Break, move || {
            while let Ok(line) = log_rx.try_recv() {
                let mut end = log_buf.end_iter();
                log_buf.insert(&mut end, &format!("{}\n", line));
            }
            glib::ControlFlow::Continue
        }),
    );
}

pub fn install_prune_timer(
    procs: Arc<Mutex<Procs>>,
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
            }
            if changed {
                update_services_ui();
            }
            glib::ControlFlow::Continue
        }),
    );
}

pub fn install_close_handler(window: &gtk4::ApplicationWindow, procs: Arc<Mutex<Procs>>, log_tx: mpsc::Sender<String>) {
    window.connect_close_request(clone!(@strong procs, @strong log_tx => @default-return glib::Propagation::Proceed, move |_| {
        let mut p = procs.lock().unwrap();
        // Don't block the UI thread: terminate in background.
        if let Some(c) = p.watch.take() { terminate_child(c, "node wl-watch", log_tx.clone()); }
        if let Some(c) = p.apply.take() { terminate_child(c, "node wl-apply", log_tx.clone()); }
        if let Some(c) = p.relay.take() { terminate_child(c, "relay", log_tx.clone()); }
        glib::Propagation::Proceed
    }));
}
