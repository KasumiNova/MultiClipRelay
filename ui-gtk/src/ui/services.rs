use glib::clone;
use gtk4::prelude::*;

use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use crate::i18n::{t, Lang, K};
use crate::procs::{spawn_node, spawn_relay, terminate_child, Procs};
use crate::systemd;
use crate::util::normalize_relay_addr_for_connect;

use super::constants::DEFAULT_IMAGE_MODE_ID;
use super::helpers::{combo_active_id_or, spin_usize};

#[derive(Clone)]
pub struct ServiceWidgets {
    pub start_all: gtk4::Button,
    pub stop_all: gtk4::Button,

    pub start_relay_btn: gtk4::Button,
    pub stop_relay_btn: gtk4::Button,
    pub start_watch_btn: gtk4::Button,
    pub stop_watch_btn: gtk4::Button,
    pub start_apply_btn: gtk4::Button,
    pub stop_apply_btn: gtk4::Button,
    pub start_x11_btn: gtk4::Button,
    pub stop_x11_btn: gtk4::Button,

    pub status_relay: gtk4::Label,
    pub status_watch: gtk4::Label,
    pub status_apply: gtk4::Label,
    pub status_x11: gtk4::Label,
}

#[derive(Clone)]
pub struct ServiceConfigInputs {
    pub relay_entry: gtk4::Entry,
    pub room_entry: gtk4::Entry,
    pub max_text_spin: gtk4::SpinButton,
    pub max_image_spin: gtk4::SpinButton,
    pub max_file_spin: gtk4::SpinButton,
    pub x11_poll_spin: gtk4::SpinButton,
    pub image_mode_combo: gtk4::ComboBoxText,
}

pub fn make_update_services_ui(
    procs: Arc<Mutex<Procs>>,
    status: Arc<Mutex<systemd::ServiceStatus>>,
    use_systemd: bool,
    lang_state: Arc<Mutex<Lang>>,
    w: ServiceWidgets,
) -> Rc<dyn Fn()> {
    Rc::new(move || {
        let lang = *lang_state.lock().unwrap();
        let (relay_running, watch_running, apply_running, x11_running) = if use_systemd {
            let s = *status.lock().unwrap();
            (s.relay, s.watch, s.apply, s.x11)
        } else {
            let p = procs.lock().unwrap();
            (p.relay.is_some(), p.watch.is_some(), p.apply.is_some(), p.x11.is_some())
        };

        let running_txt = t(lang, K::StatusRunning);
        let stopped_txt = t(lang, K::StatusStopped);
        w.status_relay.set_text(if relay_running {
            running_txt
        } else {
            stopped_txt
        });
        w.status_watch.set_text(if watch_running {
            running_txt
        } else {
            stopped_txt
        });
        w.status_apply.set_text(if apply_running {
            running_txt
        } else {
            stopped_txt
        });
        w.status_x11.set_text(if x11_running {
            running_txt
        } else {
            stopped_txt
        });

        w.start_relay_btn.set_sensitive(!relay_running);
        w.stop_relay_btn.set_sensitive(relay_running);
        w.start_watch_btn.set_sensitive(!watch_running);
        w.stop_watch_btn.set_sensitive(watch_running);
        w.start_apply_btn.set_sensitive(!apply_running);
        w.stop_apply_btn.set_sensitive(apply_running);
        w.start_x11_btn.set_sensitive(!x11_running);
        w.stop_x11_btn.set_sensitive(x11_running);

        w.stop_all
            .set_sensitive(relay_running || watch_running || apply_running || x11_running);
        w.start_all.set_sensitive(
            !(relay_running && watch_running && apply_running && (!use_systemd || x11_running)),
        );
    })
}

pub fn connect_service_handlers(
    procs: Arc<Mutex<Procs>>,
    log_tx: mpsc::Sender<String>,
    update_services_ui: Rc<dyn Fn()>,
    w: ServiceWidgets,
    inputs: ServiceConfigInputs,
) {
    let ServiceConfigInputs {
        relay_entry,
        room_entry,
        max_text_spin,
        max_image_spin,
        max_file_spin,
        x11_poll_spin,
        image_mode_combo,
    } = inputs;

    let use_systemd = systemd::enabled_from_env_or_auto();

    // Each GTK signal handler is a separate 'move' closure. Clone the widgets / Rc helpers per
    // handler to avoid moving a single value into the first closure and failing subsequent ones.
    let relay_entry_for_relay = relay_entry.clone();
    let relay_entry_for_watch = relay_entry.clone();
    let relay_entry_for_apply = relay_entry.clone();

    let room_entry_for_watch = room_entry.clone();
    let room_entry_for_apply = room_entry.clone();

    let x11_poll_spin_for_cfg = x11_poll_spin.clone();
    let x11_poll_spin_for_x11 = x11_poll_spin.clone();

    let max_text_spin_for_watch = max_text_spin.clone();
    let max_image_spin_for_watch = max_image_spin.clone();
    let max_file_spin_for_watch = max_file_spin.clone();
    let image_mode_combo_for_watch = image_mode_combo.clone();

    let max_text_spin_for_apply = max_text_spin.clone();
    let image_mode_combo_for_apply = image_mode_combo.clone();

    let max_text_spin_for_x11 = max_text_spin.clone();
    let max_image_spin_for_x11 = max_image_spin.clone();

    let relay_entry_c = relay_entry.clone();
    let room_entry_c = room_entry.clone();
    let max_text_spin_c = max_text_spin.clone();
    let max_image_spin_c = max_image_spin.clone();
    let max_file_spin_c = max_file_spin.clone();
    let image_mode_combo_c = image_mode_combo.clone();

    let mk_cfg_from_ui: Rc<dyn Fn() -> crate::config::UiConfig> = Rc::new(move || {
        crate::config::UiConfig {
            relay_addr: relay_entry_c.text().to_string(),
            room: room_entry_c.text().to_string(),
            max_text_bytes: spin_usize(&max_text_spin_c),
            max_image_bytes: spin_usize(&max_image_spin_c),
            max_file_bytes: spin_usize(&max_file_spin_c),
            image_mode: combo_active_id_or(&image_mode_combo_c, DEFAULT_IMAGE_MODE_ID),
            x11_poll_interval_ms: spin_usize(&x11_poll_spin_for_cfg) as u64,
            language: "auto".to_string(),
            history_columns: Default::default(),
            force_png: None,
        }
    });

    let mk_cfg_from_ui_for_relay = mk_cfg_from_ui.clone();
    let mk_cfg_from_ui_for_watch = mk_cfg_from_ui.clone();
    let mk_cfg_from_ui_for_apply = mk_cfg_from_ui.clone();
    let mk_cfg_from_ui_for_x11 = mk_cfg_from_ui.clone();
    let mk_cfg_from_ui_for_all = mk_cfg_from_ui.clone();

    // Relay
    w.start_relay_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui, @strong mk_cfg_from_ui_for_relay, @strong relay_entry_for_relay => move |_| {
        if use_systemd {
            let cfg = (mk_cfg_from_ui_for_relay)();
            let _ = systemd::write_env_from_ui_config(&cfg);
            match systemd::start(systemd::UNIT_RELAY) {
                Ok(()) => { let _ = log_tx.send("started relay (systemd)".into()); }
                Err(e) => { let _ = log_tx.send(format!("failed to start relay (systemd): {e:?}")); }
            }
            update_services_ui();
            return;
        }

        let mut p = procs.lock().unwrap();
        if p.relay.is_some() {
            let _ = log_tx.send("relay already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let bind_addr = relay_entry_for_relay.text().to_string();
        match spawn_relay(&log_tx, &bind_addr) {
            Ok(child) => {
                p.relay = Some(child);
                let _ = log_tx.send("started relay".into());
            }
            Err(e) => {
                let _ = log_tx.send(format!("failed to start relay: {e:?}"));
            }
        }
        drop(p);
        update_services_ui();
    }));

    w.stop_relay_btn.connect_clicked(
        clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
            if use_systemd {
                match systemd::stop(systemd::UNIT_RELAY) {
                    Ok(()) => { let _ = log_tx.send("stopping relay (systemd)".into()); }
                    Err(e) => { let _ = log_tx.send(format!("failed to stop relay (systemd): {e:?}")); }
                }
                update_services_ui();
                return;
            }
            let mut p = procs.lock().unwrap();
            if let Some(child) = p.relay.take() {
                terminate_child(child, "relay", log_tx.clone());
                let _ = log_tx.send("stopping relay".into());
            } else {
                let _ = log_tx.send("relay not running".into());
            }
            drop(p);
            update_services_ui();
        }),
    );

    // Watch
    w.start_watch_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui, @strong mk_cfg_from_ui_for_watch, @strong relay_entry_for_watch, @strong room_entry_for_watch, @strong max_text_spin_for_watch, @strong max_image_spin_for_watch, @strong max_file_spin_for_watch, @strong image_mode_combo_for_watch => move |_| {
        if use_systemd {
            let cfg = (mk_cfg_from_ui_for_watch)();
            let _ = systemd::write_env_from_ui_config(&cfg);
            match systemd::start(systemd::UNIT_WL_WATCH) {
                Ok(()) => { let _ = log_tx.send("started wl-watch (systemd)".into()); }
                Err(e) => { let _ = log_tx.send(format!("failed to start wl-watch (systemd): {e:?}")); }
            }
            update_services_ui();
            return;
        }

        let mut p = procs.lock().unwrap();
        if p.watch.is_some() {
            let _ = log_tx.send("wl-watch already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let relay_raw = relay_entry_for_watch.text().to_string();
        let relay = normalize_relay_addr_for_connect(&relay_raw);
        let room = room_entry_for_watch.text().to_string();
        let max_text = spin_usize(&max_text_spin_for_watch);
        let max_img = spin_usize(&max_image_spin_for_watch);
        let max_file = spin_usize(&max_file_spin_for_watch);
        let image_mode = combo_active_id_or(&image_mode_combo_for_watch, DEFAULT_IMAGE_MODE_ID);

        let args_owned: Vec<String> = vec![
            "wl-watch".to_string(),
            "--room".to_string(),
            room,
            "--relay".to_string(),
            relay,
            "--mode".to_string(),
            "watch".to_string(),
            "--max-text-bytes".to_string(),
            max_text.to_string(),
            "--max-image-bytes".to_string(),
            max_img.to_string(),
            "--max-file-bytes".to_string(),
            max_file.to_string(),
            "--image-mode".to_string(),
            image_mode,
        ];
        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
        match spawn_node(&log_tx, &args) {
            Ok(child) => {
                p.watch = Some(child);
                let _ = log_tx.send("started wl-watch".into());
            }
            Err(e) => {
                let _ = log_tx.send(format!("failed to start wl-watch: {e:?}"));
            }
        }
        drop(p);
        update_services_ui();
    }));

    w.stop_watch_btn.connect_clicked(
        clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
            if use_systemd {
                match systemd::stop(systemd::UNIT_WL_WATCH) {
                    Ok(()) => { let _ = log_tx.send("stopping wl-watch (systemd)".into()); }
                    Err(e) => { let _ = log_tx.send(format!("failed to stop wl-watch (systemd): {e:?}")); }
                }
                update_services_ui();
                return;
            }
            let mut p = procs.lock().unwrap();
            if let Some(child) = p.watch.take() {
                terminate_child(child, "node wl-watch", log_tx.clone());
                let _ = log_tx.send("stopping wl-watch".into());
            } else {
                let _ = log_tx.send("wl-watch not running".into());
            }
            drop(p);
            update_services_ui();
        }),
    );

    // Apply
    w.start_apply_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui, @strong mk_cfg_from_ui_for_apply, @strong relay_entry_for_apply, @strong room_entry_for_apply, @strong max_text_spin_for_apply, @strong image_mode_combo_for_apply => move |_| {
        if use_systemd {
            let cfg = (mk_cfg_from_ui_for_apply)();
            let _ = systemd::write_env_from_ui_config(&cfg);
            match systemd::start(systemd::UNIT_WL_APPLY) {
                Ok(()) => { let _ = log_tx.send("started wl-apply (systemd)".into()); }
                Err(e) => { let _ = log_tx.send(format!("failed to start wl-apply (systemd): {e:?}")); }
            }
            update_services_ui();
            return;
        }

        let mut p = procs.lock().unwrap();
        if p.apply.is_some() {
            let _ = log_tx.send("wl-apply already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let relay_raw = relay_entry_for_apply.text().to_string();
        let relay = normalize_relay_addr_for_connect(&relay_raw);
        let room = room_entry_for_apply.text().to_string();
        let image_mode = combo_active_id_or(&image_mode_combo_for_apply, DEFAULT_IMAGE_MODE_ID);

        let args_owned: Vec<String> = vec![
            "wl-apply".to_string(),
            "--room".to_string(),
            room,
            "--relay".to_string(),
            relay,
            "--image-mode".to_string(),
            image_mode,
        ];
        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
        match spawn_node(&log_tx, &args) {
            Ok(child) => {
                p.apply = Some(child);
                let _ = log_tx.send("started wl-apply".into());
            }
            Err(e) => {
                let _ = log_tx.send(format!("failed to start wl-apply: {e:?}"));
            }
        }
        drop(p);
        update_services_ui();
    }));

    w.stop_apply_btn.connect_clicked(
        clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
            if use_systemd {
                match systemd::stop(systemd::UNIT_WL_APPLY) {
                    Ok(()) => { let _ = log_tx.send("stopping wl-apply (systemd)".into()); }
                    Err(e) => { let _ = log_tx.send(format!("failed to stop wl-apply (systemd): {e:?}")); }
                }
                update_services_ui();
                return;
            }
            let mut p = procs.lock().unwrap();
            if let Some(child) = p.apply.take() {
                terminate_child(child, "node wl-apply", log_tx.clone());
                let _ = log_tx.send("stopping wl-apply".into());
            } else {
                let _ = log_tx.send("wl-apply not running".into());
            }
            drop(p);
            update_services_ui();
        }),
    );

    // X11 sync
    w.start_x11_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui, @strong mk_cfg_from_ui_for_x11, @strong x11_poll_spin_for_x11, @strong max_text_spin_for_x11, @strong max_image_spin_for_x11 => move |_| {
        if use_systemd {
            if !systemd::node_supports_x11_sync() {
                let _ = log_tx.send("multicliprelay-node does not support x11-sync; please upgrade/reinstall binaries (or adjust unit ExecStart)".into());
                update_services_ui();
                return;
            }
            let cfg = (mk_cfg_from_ui_for_x11)();
            let _ = systemd::write_env_from_ui_config(&cfg);
            match systemd::start(systemd::UNIT_X11_SYNC) {
                Ok(()) => { let _ = log_tx.send("started x11-sync (systemd)".into()); }
                Err(e) => { let _ = log_tx.send(format!("failed to start x11-sync (systemd): {e:?}")); }
            }
            update_services_ui();
            return;
        }

        let mut p = procs.lock().unwrap();
        if p.x11.is_some() {
            let _ = log_tx.send("x11-sync already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let args_owned: Vec<String> = vec![
            "x11-sync".to_string(),
            "--x11-poll-interval-ms".to_string(),
            spin_usize(&x11_poll_spin_for_x11).to_string(),
            "--max-text-bytes".to_string(),
            spin_usize(&max_text_spin_for_x11).to_string(),
            "--max-image-bytes".to_string(),
            spin_usize(&max_image_spin_for_x11).to_string(),
        ];
        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
        match spawn_node(&log_tx, &args) {
            Ok(child) => {
                p.x11 = Some(child);
                let _ = log_tx.send("started x11-sync".into());
            }
            Err(e) => {
                let _ = log_tx.send(format!("failed to start x11-sync: {e:?}"));
            }
        }
        drop(p);
        update_services_ui();
    }));

    w.stop_x11_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
        if use_systemd {
            match systemd::stop(systemd::UNIT_X11_SYNC) {
                Ok(()) => { let _ = log_tx.send("stopping x11-sync (systemd)".into()); }
                Err(e) => { let _ = log_tx.send(format!("failed to stop x11-sync (systemd): {e:?}")); }
            }
            update_services_ui();
            return;
        }
        let mut p = procs.lock().unwrap();
        if let Some(child) = p.x11.take() {
            terminate_child(child, "node x11-sync", log_tx.clone());
            let _ = log_tx.send("stopping x11-sync".into());
        } else {
            let _ = log_tx.send("x11-sync not running".into());
        }
        drop(p);
        update_services_ui();
    }));

    // Start/stop all
    w.start_all.connect_clicked(clone!(@strong procs, @strong log_tx, @strong mk_cfg_from_ui_for_all, @weak relay_entry, @weak room_entry, @weak max_text_spin, @weak max_image_spin, @weak max_file_spin, @weak image_mode_combo, @strong update_services_ui => move |_| {
        if use_systemd {
            if !systemd::node_supports_x11_sync() {
                let _ = log_tx.send("multicliprelay-node does not support x11-sync; please upgrade/reinstall binaries (or adjust unit ExecStart)".into());
                update_services_ui();
                return;
            }
            let cfg = (mk_cfg_from_ui_for_all)();
            let _ = systemd::write_env_from_ui_config(&cfg);
            let _ = systemd::start(systemd::UNIT_RELAY);
            let _ = systemd::start(systemd::UNIT_WL_WATCH);
            let _ = systemd::start(systemd::UNIT_WL_APPLY);
            let _ = systemd::start(systemd::UNIT_X11_SYNC);
            let _ = log_tx.send("started all (systemd)".into());
            update_services_ui();
            return;
        }

        let relay_bind = relay_entry.text().to_string();
        let relay = normalize_relay_addr_for_connect(&relay_bind);
        let room = room_entry.text().to_string();
        let max_text = spin_usize(&max_text_spin);
        let max_img = spin_usize(&max_image_spin);
        let max_file = spin_usize(&max_file_spin);
        let image_mode = combo_active_id_or(&image_mode_combo, DEFAULT_IMAGE_MODE_ID);

        let mut p = procs.lock().unwrap();

        if p.relay.is_none() {
            match spawn_relay(&log_tx, &relay_bind) {
                Ok(child) => {
                    p.relay = Some(child);
                    let _ = log_tx.send("started relay".into());
                }
                Err(e) => {
                    let _ = log_tx.send(format!("failed to start relay: {e:?}"));
                }
            }
        }

        if p.watch.is_none() {
            let args_owned: Vec<String> = vec![
                "wl-watch".to_string(),
                "--room".to_string(),
                room.clone(),
                "--relay".to_string(),
                relay.clone(),
                "--mode".to_string(),
                "watch".to_string(),
                "--max-text-bytes".to_string(),
                max_text.to_string(),
                "--max-image-bytes".to_string(),
                max_img.to_string(),
                "--max-file-bytes".to_string(),
                max_file.to_string(),
                "--image-mode".to_string(),
                image_mode.clone(),
            ];
            let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
            match spawn_node(&log_tx, &args) {
                Ok(child) => {
                    p.watch = Some(child);
                    let _ = log_tx.send("started wl-watch".into());
                }
                Err(e) => {
                    let _ = log_tx.send(format!("failed to start wl-watch: {e:?}"));
                }
            }
        }

        if p.apply.is_none() {
            let args_owned: Vec<String> = vec![
                "wl-apply".to_string(),
                "--room".to_string(),
                room,
                "--relay".to_string(),
                relay,
                "--image-mode".to_string(),
                image_mode,
            ];
            let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
            match spawn_node(&log_tx, &args) {
                Ok(child) => {
                    p.apply = Some(child);
                    let _ = log_tx.send("started wl-apply".into());
                }
                Err(e) => {
                    let _ = log_tx.send(format!("failed to start wl-apply: {e:?}"));
                }
            }
        }

        drop(p);
        update_services_ui();
    }));

    w.stop_all.connect_clicked(
        clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
            if use_systemd {
                let _ = systemd::stop(systemd::UNIT_X11_SYNC);
                let _ = systemd::stop(systemd::UNIT_WL_WATCH);
                let _ = systemd::stop(systemd::UNIT_WL_APPLY);
                let _ = systemd::stop(systemd::UNIT_RELAY);
                let _ = log_tx.send("stopping all (systemd)".into());
                update_services_ui();
                return;
            }
            let mut p = procs.lock().unwrap();
            if let Some(child) = p.x11.take() {
                terminate_child(child, "node x11-sync", log_tx.clone());
                let _ = log_tx.send("stopping x11-sync".into());
            }
            if let Some(child) = p.watch.take() {
                terminate_child(child, "node wl-watch", log_tx.clone());
                let _ = log_tx.send("stopping wl-watch".into());
            }
            if let Some(child) = p.apply.take() {
                terminate_child(child, "node wl-apply", log_tx.clone());
                let _ = log_tx.send("stopping wl-apply".into());
            }
            if let Some(child) = p.relay.take() {
                terminate_child(child, "relay", log_tx.clone());
                let _ = log_tx.send("stopping relay".into());
            }
            drop(p);
            update_services_ui();
        }),
    );
}
