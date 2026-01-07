use glib::clone;
use gtk4::prelude::*;

use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use crate::i18n::{t, K, Lang};
use crate::procs::{spawn_node, spawn_relay, terminate_child, Procs};
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

    pub status_relay: gtk4::Label,
    pub status_watch: gtk4::Label,
    pub status_apply: gtk4::Label,
}

#[derive(Clone)]
pub struct ServiceConfigInputs {
    pub relay_entry: gtk4::Entry,
    pub room_entry: gtk4::Entry,
    pub max_text_spin: gtk4::SpinButton,
    pub max_image_spin: gtk4::SpinButton,
    pub max_file_spin: gtk4::SpinButton,
    pub image_mode_combo: gtk4::ComboBoxText,
}

pub fn make_update_services_ui(procs: Arc<Mutex<Procs>>, lang_state: Arc<Mutex<Lang>>, w: ServiceWidgets) -> Rc<dyn Fn()> {
    Rc::new(move || {
        let lang = *lang_state.lock().unwrap();
        let p = procs.lock().unwrap();
        let relay_running = p.relay.is_some();
        let watch_running = p.watch.is_some();
        let apply_running = p.apply.is_some();

        let running_txt = t(lang, K::StatusRunning);
        let stopped_txt = t(lang, K::StatusStopped);
        w.status_relay
            .set_text(if relay_running { running_txt } else { stopped_txt });
        w.status_watch
            .set_text(if watch_running { running_txt } else { stopped_txt });
        w.status_apply
            .set_text(if apply_running { running_txt } else { stopped_txt });

        w.start_relay_btn.set_sensitive(!relay_running);
        w.stop_relay_btn.set_sensitive(relay_running);
        w.start_watch_btn.set_sensitive(!watch_running);
        w.stop_watch_btn.set_sensitive(watch_running);
        w.start_apply_btn.set_sensitive(!apply_running);
        w.stop_apply_btn.set_sensitive(apply_running);

        w.stop_all
            .set_sensitive(relay_running || watch_running || apply_running);
        w.start_all
            .set_sensitive(!(relay_running && watch_running && apply_running));
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
        image_mode_combo,
    } = inputs;

    // Relay
    w.start_relay_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @weak relay_entry, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
        if p.relay.is_some() {
            let _ = log_tx.send("relay already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let bind_addr = relay_entry.text().to_string();
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

    w.stop_relay_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
        if let Some(child) = p.relay.take() {
            terminate_child(child, "relay", log_tx.clone());
            let _ = log_tx.send("stopping relay".into());
        } else {
            let _ = log_tx.send("relay not running".into());
        }
        drop(p);
        update_services_ui();
    }));

    // Watch
    w.start_watch_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @weak relay_entry, @weak room_entry, @weak max_text_spin, @weak max_image_spin, @weak max_file_spin, @weak image_mode_combo, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
        if p.watch.is_some() {
            let _ = log_tx.send("wl-watch already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let relay_raw = relay_entry.text().to_string();
        let relay = normalize_relay_addr_for_connect(&relay_raw);
        let room = room_entry.text().to_string();
        let max_text = spin_usize(&max_text_spin);
        let max_img = spin_usize(&max_image_spin);
        let max_file = spin_usize(&max_file_spin);
        let image_mode = combo_active_id_or(&image_mode_combo, DEFAULT_IMAGE_MODE_ID);

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

    w.stop_watch_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
        if let Some(child) = p.watch.take() {
            terminate_child(child, "node wl-watch", log_tx.clone());
            let _ = log_tx.send("stopping wl-watch".into());
        } else {
            let _ = log_tx.send("wl-watch not running".into());
        }
        drop(p);
        update_services_ui();
    }));

    // Apply
    w.start_apply_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @weak relay_entry, @weak room_entry, @weak image_mode_combo, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
        if p.apply.is_some() {
            let _ = log_tx.send("wl-apply already running".into());
            drop(p);
            update_services_ui();
            return;
        }
        let relay_raw = relay_entry.text().to_string();
        let relay = normalize_relay_addr_for_connect(&relay_raw);
        let room = room_entry.text().to_string();
        let image_mode = combo_active_id_or(&image_mode_combo, DEFAULT_IMAGE_MODE_ID);

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

    w.stop_apply_btn.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
        if let Some(child) = p.apply.take() {
            terminate_child(child, "node wl-apply", log_tx.clone());
            let _ = log_tx.send("stopping wl-apply".into());
        } else {
            let _ = log_tx.send("wl-apply not running".into());
        }
        drop(p);
        update_services_ui();
    }));

    // Start/stop all
    w.start_all.connect_clicked(clone!(@strong procs, @strong log_tx, @weak relay_entry, @weak room_entry, @weak max_text_spin, @weak max_image_spin, @weak max_file_spin, @weak image_mode_combo, @strong update_services_ui => move |_| {
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

    w.stop_all.connect_clicked(clone!(@strong procs, @strong log_tx, @strong update_services_ui => move |_| {
        let mut p = procs.lock().unwrap();
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
    }));
}
