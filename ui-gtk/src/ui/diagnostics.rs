use glib::clone;
use gtk4::prelude::*;

use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::i18n::{t, K, Lang};
use crate::procs::spawn_node;
use crate::util::{chrono_like_timestamp, fake_remote_device_id};

use super::constants::DEFAULT_IMAGE_MODE_ID;

#[derive(Clone)]
pub struct DiagnosticsWidgets {
    pub send_test_text: gtk4::Button,
    pub send_test_image: gtk4::Button,
    pub send_test_file: gtk4::Button,
    pub show_clip_types: gtk4::Button,
}

#[derive(Clone)]
pub struct DiagnosticsInputs {
    pub window: gtk4::ApplicationWindow,
    pub relay_entry: gtk4::Entry,
    pub room_entry: gtk4::Entry,
    pub max_image_spin: gtk4::SpinButton,
    pub max_file_spin: gtk4::SpinButton,
    pub image_mode_combo: gtk4::ComboBoxText,
}

pub fn connect_diagnostics_handlers(
    w: DiagnosticsWidgets,
    inputs: DiagnosticsInputs,
    log_tx: mpsc::Sender<String>,
    lang_state: Arc<Mutex<Lang>>,
) {
    let DiagnosticsInputs {
        window,
        relay_entry,
        room_entry,
        max_image_spin,
        max_file_spin,
        image_mode_combo,
    } = inputs;

    w.send_test_text.connect_clicked(clone!(@strong log_tx, @weak relay_entry, @weak room_entry => move |_| {
        let relay = relay_entry.text().to_string();
        let room = room_entry.text().to_string();
        let text = format!("cliprelay test @{}", chrono_like_timestamp());
        // Important: `node wl-apply` intentionally ignores messages whose `device_id` equals the
        // local device id. For local end-to-end testing, we pretend this message comes from a
        // different device by overriding --device-id.
        let dev = fake_remote_device_id();
        let args_owned: Vec<String> = vec![
            "--device-id".to_string(),
            dev.clone(),
            "send-text".to_string(),
            "--room".to_string(),
            room,
            "--relay".to_string(),
            relay,
            "--text".to_string(),
            text,
        ];
        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
        match spawn_node(&log_tx, &args) {
            Ok(mut child) => {
                // fire-and-forget: wait in a thread
                thread::spawn(move || {
                    let _ = child.wait();
                });
                let _ = log_tx.send(format!("sent test text (as device_id={})", dev));
            }
            Err(e) => {
                let _ = log_tx.send(format!("failed to send test text: {e:?}"));
            }
        }
    }));

    w.send_test_image.connect_clicked(clone!(@strong log_tx, @strong lang_state, @weak window, @weak relay_entry, @weak room_entry, @weak max_image_spin, @weak image_mode_combo => move |_| {
        // Determine current UI language at click time.
        // (FileChooser strings are built on demand.)
        let lang = *lang_state.lock().unwrap();
        let dialog = gtk4::FileChooserNative::builder()
            .title(t(lang, K::ChooseImageTitle))
            .transient_for(&window)
            .action(gtk4::FileChooserAction::Open)
            .build();

        // Common image formats.
        let filter = gtk4::FileFilter::new();
        filter.set_name(Some(t(lang, K::ImageFilterName)));
        filter.add_mime_type("image/png");
        filter.add_mime_type("image/jpeg");
        filter.add_mime_type("image/webp");
        filter.add_mime_type("image/gif");
        filter.add_pattern("*.png");
        filter.add_pattern("*.jpg");
        filter.add_pattern("*.jpeg");
        filter.add_pattern("*.webp");
        filter.add_pattern("*.gif");
        dialog.add_filter(&filter);

        dialog.connect_response(clone!(@strong log_tx, @weak relay_entry, @weak room_entry, @weak max_image_spin, @weak image_mode_combo => move |d, resp| {
            if resp == gtk4::ResponseType::Accept {
                if let Some(file) = d.file() {
                    if let Some(path) = file.path() {
                        let relay = relay_entry.text().to_string();
                        let room = room_entry.text().to_string();
                        let max_bytes = max_image_spin.value() as usize;
                        let image_mode = image_mode_combo
                            .active_id()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| DEFAULT_IMAGE_MODE_ID.to_string());
                        // Same reason as send_test_text: simulate a different device id.
                        let dev = fake_remote_device_id();
                        let path_s = path.to_string_lossy().into_owned();
                        let max_s = max_bytes.to_string();
                        let args_owned: Vec<String> = vec![
                            "--device-id".to_string(),
                            dev.clone(),
                            "send-image".to_string(),
                            "--room".to_string(),
                            room,
                            "--relay".to_string(),
                            relay,
                            "--file".to_string(),
                            path_s,
                            "--max-bytes".to_string(),
                            max_s,
                            "--image-mode".to_string(),
                            image_mode,
                        ];
                        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
                        match spawn_node(&log_tx, &args) {
                            Ok(mut child) => {
                                thread::spawn(move || {
                                    let _ = child.wait();
                                });
                                let _ = log_tx.send(format!("sent test image (as device_id={})", dev));
                            }
                            Err(e) => {
                                let _ = log_tx.send(format!("failed to send test image: {e:?}"));
                            }
                        }
                    }
                }
            }
            d.destroy();
        }));
        dialog.show();
    }));

    w.send_test_file.connect_clicked(clone!(@strong log_tx, @strong lang_state, @weak window, @weak relay_entry, @weak room_entry, @weak max_file_spin => move |_| {
        let lang = *lang_state.lock().unwrap();
        let dialog = gtk4::FileChooserNative::builder()
            .title(t(lang, K::ChooseFileTitle))
            .transient_for(&window)
            .action(gtk4::FileChooserAction::Open)
            .build();

        let filter = gtk4::FileFilter::new();
        filter.set_name(Some(t(lang, K::FileFilterName)));
        filter.add_pattern("*");
        dialog.add_filter(&filter);

        dialog.connect_response(clone!(@strong log_tx, @weak relay_entry, @weak room_entry, @weak max_file_spin => move |d, resp| {
            if resp == gtk4::ResponseType::Accept {
                if let Some(file) = d.file() {
                    if let Some(path) = file.path() {
                        let relay = relay_entry.text().to_string();
                        let room = room_entry.text().to_string();
                        let max_bytes = max_file_spin.value() as usize;

                        // Same reason as send_test_text: simulate a different device id.
                        let dev = fake_remote_device_id();
                        let path_s = path.to_string_lossy().into_owned();
                        let max_s = max_bytes.to_string();

                        let args_owned: Vec<String> = vec![
                            "--device-id".to_string(),
                            dev.clone(),
                            "send-file".to_string(),
                            "--room".to_string(),
                            room,
                            "--relay".to_string(),
                            relay,
                            "--file".to_string(),
                            path_s,
                            "--max-file-bytes".to_string(),
                            max_s,
                        ];
                        let args: Vec<&str> = args_owned.iter().map(|s| s.as_str()).collect();
                        match spawn_node(&log_tx, &args) {
                            Ok(mut child) => {
                                thread::spawn(move || {
                                    let _ = child.wait();
                                });
                                let _ = log_tx.send(format!("sent test file (as device_id={})", dev));
                            }
                            Err(e) => {
                                let _ = log_tx.send(format!("failed to send test file: {e:?}"));
                            }
                        }
                    }
                }
            }
            d.destroy();
        }));
        dialog.show();
    }));

    w.show_clip_types.connect_clicked(clone!(@strong log_tx => move |_| {
        let tx = log_tx.clone();
        thread::spawn(move || {
            let out_clip = Command::new("wl-paste").arg("--list-types").output();
            match out_clip {
                Ok(o) => {
                    let s = String::from_utf8_lossy(&o.stdout);
                    let _ = tx.send(format!("clipboard types (clipboard):\n{}", s.trim_end()));
                }
                Err(e) => {
                    let _ = tx.send(format!("failed to run wl-paste --list-types: {e:?}"));
                }
            }

            let out_primary = Command::new("wl-paste")
                .arg("--primary")
                .arg("--list-types")
                .output();
            match out_primary {
                Ok(o) => {
                    let s = String::from_utf8_lossy(&o.stdout);
                    let _ = tx.send(format!("clipboard types (primary):\n{}", s.trim_end()));
                }
                Err(e) => {
                    let _ = tx.send(format!("failed to run wl-paste --primary --list-types: {e:?}"));
                }
            }
        });
    }));
}
