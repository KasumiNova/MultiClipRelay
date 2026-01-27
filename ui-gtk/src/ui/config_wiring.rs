use glib::clone;
use gtk4::prelude::*;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use crate::config::{load_config, save_config};
use crate::i18n::{detect_lang_from_env, image_mode_hint_text, parse_lang_id, Lang};
use crate::systemd;

use super::constants::{DEFAULT_IMAGE_MODE_ID, LANG_AUTO_ID};

#[derive(Clone)]
pub struct ConfigWidgets {
    pub relay_entry: gtk4::Entry,
    pub room_entry: gtk4::Entry,
    pub max_text_spin: gtk4::SpinButton,
    pub max_image_spin: gtk4::SpinButton,
    pub max_file_spin: gtk4::SpinButton,
    pub x11_poll_spin: gtk4::SpinButton,
    pub language_combo: gtk4::ComboBoxText,
    pub image_mode_combo: gtk4::ComboBoxText,
    pub mode_hint: gtk4::Label,
    pub reload_btn: gtk4::Button,
}

pub struct ConfigWiringCtx {
    pub cfg_path: PathBuf,
    pub log_tx: mpsc::Sender<String>,
    pub lang_state: Arc<Mutex<Lang>>,
    pub apply_lang: Rc<dyn Fn(Lang)>,
    pub update_services_ui: Rc<dyn Fn()>,

    pub ui: ConfigWidgets,

    pub suppress_save_cfg: Rc<Cell<bool>>,
    pub suppress_lang_combo: Rc<Cell<bool>>,
    pub suppress_mode_combo: Rc<Cell<bool>>,
}

pub fn make_save_cfg(cfg_path: PathBuf, ui: ConfigWidgets) -> Rc<dyn Fn()> {
    Rc::new(move || {
        // Preserve any fields not present on the config form (e.g. column visibility).
        let mut cfg = load_config(&cfg_path).unwrap_or_default();

        let image_mode = ui
            .image_mode_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| DEFAULT_IMAGE_MODE_ID.to_string());
        let language = ui
            .language_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| LANG_AUTO_ID.to_string());

        cfg.relay_addr = ui.relay_entry.text().to_string();
        cfg.room = ui.room_entry.text().to_string();
        cfg.max_text_bytes = ui.max_text_spin.value() as usize;
        cfg.max_image_bytes = ui.max_image_spin.value() as usize;
        cfg.max_file_bytes = ui.max_file_spin.value() as usize;
        cfg.image_mode = image_mode;
        cfg.x11_poll_interval_ms = ui.x11_poll_spin.value() as u64;
        cfg.language = language;
        cfg.force_png = None;
        if let Err(e) = save_config(&cfg_path, &cfg) {
            eprintln!("save config failed: {:?}", e);
        }
        // Best-effort: keep systemd EnvironmentFile in sync.
        let _ = systemd::write_env_from_ui_config(&cfg);
    })
}

pub fn connect_config_wiring(ctx: ConfigWiringCtx) {
    let ConfigWiringCtx {
        cfg_path,
        log_tx,
        lang_state,
        apply_lang,
        update_services_ui,
        ui,
        suppress_save_cfg,
        suppress_lang_combo,
        suppress_mode_combo,
    } = ctx;

    let save_cfg = make_save_cfg(cfg_path.clone(), ui.clone());

    let ConfigWidgets {
        relay_entry,
        room_entry,
        max_text_spin,
        max_image_spin,
        max_file_spin,
        x11_poll_spin,
        language_combo,
        image_mode_combo,
        mode_hint,
        reload_btn,
    } = ui;

    // Save config on change (simple + good enough)
    relay_entry.connect_changed(
        clone!(@strong save_cfg, @strong suppress_save_cfg => move |_| {
            if suppress_save_cfg.get() {
                return;
            }
            (save_cfg)();
        }),
    );
    room_entry.connect_changed(
        clone!(@strong save_cfg, @strong suppress_save_cfg => move |_| {
            if suppress_save_cfg.get() {
                return;
            }
            (save_cfg)();
        }),
    );
    max_text_spin.connect_value_changed(
        clone!(@strong save_cfg, @strong suppress_save_cfg => move |_| {
            if suppress_save_cfg.get() {
                return;
            }
            (save_cfg)();
        }),
    );
    max_image_spin.connect_value_changed(
        clone!(@strong save_cfg, @strong suppress_save_cfg => move |_| {
            if suppress_save_cfg.get() {
                return;
            }
            (save_cfg)();
        }),
    );

    max_file_spin.connect_value_changed(
        clone!(@strong save_cfg, @strong suppress_save_cfg => move |_| {
            if suppress_save_cfg.get() {
                return;
            }
            (save_cfg)();
        }),
    );

    x11_poll_spin.connect_value_changed(
        clone!(@strong save_cfg, @strong suppress_save_cfg => move |_| {
            if suppress_save_cfg.get() {
                return;
            }
            (save_cfg)();
        }),
    );

    image_mode_combo.connect_changed(clone!(@strong save_cfg, @strong lang_state, @weak mode_hint, @weak image_mode_combo, @strong suppress_mode_combo, @strong suppress_save_cfg => move |_| {
        if suppress_mode_combo.get() || suppress_save_cfg.get() {
            return;
        }
        (save_cfg)();
        let lang = *lang_state.lock().unwrap();
        let mode = image_mode_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| DEFAULT_IMAGE_MODE_ID.to_string());
        mode_hint.set_text(image_mode_hint_text(lang, &mode));
    }));

    language_combo.connect_changed(clone!(@strong save_cfg, @strong lang_state, @strong apply_lang, @weak language_combo, @strong suppress_lang_combo, @strong suppress_save_cfg => move |_| {
        if suppress_lang_combo.get() || suppress_save_cfg.get() {
            return;
        }
        let id = language_combo
            .active_id()
            .map(|s| s.to_string())
            .unwrap_or_else(|| LANG_AUTO_ID.to_string());
        let lang = if id == LANG_AUTO_ID {
            detect_lang_from_env()
        } else {
            parse_lang_id(&id).unwrap_or_else(detect_lang_from_env)
        };
        *lang_state.lock().unwrap() = lang;
        (apply_lang)(lang);
        (save_cfg)();
    }));

    // Reload config from disk and apply into the UI.
    reload_btn.connect_clicked(clone!(
        @strong log_tx,
        @strong cfg_path,
        @strong lang_state,
        @strong apply_lang,
        @strong update_services_ui,
        @strong suppress_save_cfg,
        @strong suppress_lang_combo,
        @strong suppress_mode_combo,
        @weak relay_entry,
        @weak room_entry,
        @weak max_text_spin,
        @weak max_image_spin,
        @weak max_file_spin,
        @weak x11_poll_spin,
        @weak language_combo,
        @weak image_mode_combo,
        @weak mode_hint
        => move |_| {
            match load_config(&cfg_path) {
                Ok(cfg) => {
                    suppress_save_cfg.set(true);

                    relay_entry.set_text(&cfg.relay_addr);
                    room_entry.set_text(&cfg.room);
                    max_text_spin.set_value(cfg.max_text_bytes as f64);
                    max_image_spin.set_value(cfg.max_image_bytes as f64);
                    max_file_spin.set_value(cfg.max_file_bytes as f64);
                    x11_poll_spin.set_value(cfg.x11_poll_interval_ms as f64);

                    suppress_lang_combo.set(true);
                    language_combo.set_active_id(Some(&cfg.language));
                    suppress_lang_combo.set(false);

                    // Mode id update; labels are refreshed by apply_lang.
                    suppress_mode_combo.set(true);
                    image_mode_combo.set_active_id(Some(&cfg.image_mode));
                    suppress_mode_combo.set(false);

                    let lang = if cfg.language == LANG_AUTO_ID {
                        detect_lang_from_env()
                    } else {
                        parse_lang_id(&cfg.language).unwrap_or_else(detect_lang_from_env)
                    };
                    *lang_state.lock().unwrap() = lang;
                    (apply_lang)(lang);

                    let mode = image_mode_combo
                        .active_id()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| DEFAULT_IMAGE_MODE_ID.to_string());
                    mode_hint.set_text(image_mode_hint_text(lang, &mode));

                    suppress_save_cfg.set(false);
                    let _ = systemd::write_env_from_ui_config(&cfg);
                    (update_services_ui)();
                    let _ = log_tx.send(format!("reloaded config: {}", cfg_path.display()));
                }
                Err(e) => {
                    let _ = log_tx.send(format!("reload config failed: {e:?}"));
                }
            }
        }
    ));
}
