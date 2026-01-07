use glib::clone;
use gtk4::prelude::*;

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::i18n::{t, K, Lang};
use crate::util::normalize_relay_addr_for_connect;

#[derive(Debug, Clone)]
struct ProbeResult {
    ok: bool,
    detail: String,
}

fn probe_tcp(addr: &str, timeout: Duration) -> ProbeResult {
    let addr = normalize_relay_addr_for_connect(addr);
    let addr = addr.trim();
    if addr.is_empty() {
        return ProbeResult {
            ok: false,
            detail: "empty address".to_string(),
        };
    }

    let mut addrs = match addr.to_socket_addrs() {
        Ok(it) => it,
        Err(e) => {
            return ProbeResult {
                ok: false,
                detail: format!("resolve failed: {e}"),
            };
        }
    };

    let Some(sock) = addrs.next() else {
        return ProbeResult {
            ok: false,
            detail: "no socket addresses".to_string(),
        };
    };

    match TcpStream::connect_timeout(&sock, timeout) {
        Ok(_) => ProbeResult {
            ok: true,
            detail: sock.to_string(),
        },
        Err(e) => ProbeResult {
            ok: false,
            detail: format!("{sock}: {e}"),
        },
    }
}

pub fn install_relay_probe(
    relay_entry: gtk4::Entry,
    status_label: gtk4::Label,
    log_tx: mpsc::Sender<String>,
    lang_state: Arc<Mutex<Lang>>, 
) {
    // Thread input: latest relay addr.
    let (addr_tx, addr_rx) = mpsc::channel::<String>();

    // Update address when user edits the field.
    let addr_tx_changed = addr_tx.clone();
    relay_entry.connect_changed(move |e| {
        let _ = addr_tx_changed.send(e.text().to_string());
    });

    // Seed initial address.
    let _ = addr_tx.send(relay_entry.text().to_string());

    // Thread output: std channel; UI polls it on a small timeout.
    let (res_tx, res_rx) = mpsc::channel::<ProbeResult>();

    glib::timeout_add_local(
        Duration::from_millis(200),
        clone!(@weak status_label, @strong log_tx, @strong lang_state => @default-return glib::ControlFlow::Break, move || {
            while let Ok(r) = res_rx.try_recv() {
                let lang = *lang_state.lock().unwrap();
                if r.ok {
                    status_label.set_text(t(lang, K::StatusConnected));
                    status_label.set_tooltip_text(Some(&r.detail));
                } else {
                    status_label.set_text(t(lang, K::StatusDisconnected));
                    status_label.set_tooltip_text(Some(&r.detail));
                    if !r.detail.trim().is_empty() {
                        let _ = log_tx.send(format!("relay probe: {}", r.detail));
                    }
                }
            }
            glib::ControlFlow::Continue
        }),
    );

    thread::spawn(move || {
        let mut last_addr = String::new();
        let mut last_sent: Option<(bool, String)> = None;

        loop {
            // Drain latest address.
            while let Ok(a) = addr_rx.try_recv() {
                last_addr = a;
            }

            let r = probe_tcp(&last_addr, Duration::from_millis(250));

            // Avoid spamming the UI channel with identical results.
            let fingerprint = (r.ok, r.detail.clone());
            if last_sent.as_ref() != Some(&fingerprint) {
                if res_tx.send(r).is_err() {
                    return;
                }
                last_sent = Some(fingerprint);
            }

            thread::sleep(Duration::from_millis(1000));
        }
    });
}
