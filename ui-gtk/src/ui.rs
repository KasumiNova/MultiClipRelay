use glib::clone;
use gtk4::prelude::*;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use crate::config::{config_path, load_config};
use crate::i18n::{
    detect_lang_from_env, help_text, image_mode_hint_text, parse_lang_id, populate_image_mode_combo, t, K, Lang,
};
use crate::procs::Procs;

mod constants;
mod helpers;
mod apply_lang;
mod config_wiring;
mod connection;
mod diagnostics;
mod history;
mod services;
mod timers;

use self::apply_lang::{make_apply_lang, ApplyLangCtx};
use self::constants::{LANG_AUTO_ID, PAGE_CONTROL, PAGE_HELP, PAGE_HISTORY, PAGE_LOGS};
use self::config_wiring::connect_config_wiring;
use self::diagnostics::connect_diagnostics_handlers;
use self::connection::install_relay_probe;
use self::history::install_history_refresh;
use self::services::{connect_service_handlers, make_update_services_ui, ServiceConfigInputs, ServiceWidgets};
use self::timers::{install_close_handler, install_log_drain, install_prune_timer};

pub fn build_ui(app: &gtk4::Application) {
    // Safety net: even within a single process, `build_ui` might be called more than once
    // (e.g. if some code path accidentally calls it on repeated activations).
    // If a window already exists, just focus it instead of creating another panel.
    if let Some(win) = app.active_window().or_else(|| app.windows().into_iter().next()) {
        win.present();
        return;
    }

    let cfg_path = config_path();
    let cfg = load_config(&cfg_path).unwrap_or_default();

    let initial_lang = if cfg.language == LANG_AUTO_ID {
        detect_lang_from_env()
    } else {
        parse_lang_id(&cfg.language).unwrap_or_else(detect_lang_from_env)
    };
    let lang_state: Arc<Mutex<Lang>> = Arc::new(Mutex::new(initial_lang));

    let procs: Arc<Mutex<Procs>> = Arc::new(Mutex::new(Procs::default()));

    let (log_tx, log_rx) = mpsc::channel::<String>();

    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(t(initial_lang, K::WindowTitle))
        .default_width(800)
        // Keep a sensible default so we don't start below the natural minimum on HiDPI setups.
        .default_height(680)
        .build();

    // Modern-ish tab style: HeaderBar + StackSwitcher.
    // (No libadwaita dependency needed.)
    let header = gtk4::HeaderBar::new();
    let stack = gtk4::Stack::new();
    stack.set_vexpand(true);
    stack.set_hexpand(true);
    stack.set_transition_type(gtk4::StackTransitionType::SlideLeftRight);
    let switcher = gtk4::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));

    let reload_btn = gtk4::Button::with_label(t(initial_lang, K::BtnReloadConfig));
    header.pack_end(&reload_btn);
    window.set_titlebar(Some(&header));

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    let relay_entry = gtk4::Entry::builder().text(&cfg.relay_addr).build();
    let room_entry = gtk4::Entry::builder().text(&cfg.room).build();

    let max_text_adj = gtk4::Adjustment::new(
        cfg.max_text_bytes as f64,
        1.0,
        100.0 * 1024.0 * 1024.0,
        1024.0,
        1024.0,
        0.0,
    );
    let max_image_adj = gtk4::Adjustment::new(
        cfg.max_image_bytes as f64,
        1.0,
        200.0 * 1024.0 * 1024.0,
        1024.0,
        1024.0,
        0.0,
    );
    let max_file_adj = gtk4::Adjustment::new(
        cfg.max_file_bytes as f64,
        1.0,
        200.0 * 1024.0 * 1024.0,
        1024.0,
        1024.0,
        0.0,
    );
    let max_text_spin = gtk4::SpinButton::new(Some(&max_text_adj), 1024.0, 0);
    let max_image_spin = gtk4::SpinButton::new(Some(&max_image_adj), 1024.0, 0);
    let max_file_spin = gtk4::SpinButton::new(Some(&max_file_adj), 1024.0, 0);

    let language_combo = gtk4::ComboBoxText::new();
    language_combo.append(Some(LANG_AUTO_ID), t(initial_lang, K::LangAuto));
    language_combo.append(Some("zh-cn"), t(initial_lang, K::LangZhCn));
    language_combo.append(Some("en"), t(initial_lang, K::LangEn));
    language_combo.set_active_id(Some(&cfg.language));

    // Guard against signal recursion: when we update combo entries programmatically (for i18n),
    // GTK may emit `changed`, which would call back into apply_lang.
    let suppress_lang_combo = Rc::new(Cell::new(false));

    let image_mode_combo = gtk4::ComboBoxText::new();
    let suppress_mode_combo = Rc::new(Cell::new(false));

    // When applying config values programmatically (e.g. reload), don't auto-save.
    let suppress_save_cfg = Rc::new(Cell::new(false));

    populate_image_mode_combo(&image_mode_combo, initial_lang, Some(&cfg.image_mode));

    let mode_hint = gtk4::Label::builder().wrap(true).xalign(0.0).build();
    mode_hint.set_margin_top(4);
    mode_hint.set_margin_bottom(4);
    mode_hint.set_text(image_mode_hint_text(initial_lang, &cfg.image_mode));

    // -------- Controls tab --------
    let control_box = gtk4::Box::new(gtk4::Orientation::Vertical, 10);

    let config_frame = gtk4::Frame::builder().label(t(initial_lang, K::SectionConfig)).build();
    config_frame.set_margin_bottom(6);
    let config_grid = gtk4::Grid::builder().row_spacing(6).column_spacing(8).build();
    config_grid.set_margin_top(10);
    config_grid.set_margin_bottom(10);
    config_grid.set_margin_start(10);
    config_grid.set_margin_end(10);

    let lbl_relay = gtk4::Label::builder().xalign(0.0).build();
    let lbl_room = gtk4::Label::builder().xalign(0.0).build();
    let lbl_max_text = gtk4::Label::builder().xalign(0.0).build();
    let lbl_max_img = gtk4::Label::builder().xalign(0.0).build();
    let lbl_max_file = gtk4::Label::builder().xalign(0.0).build();
    let lbl_img_mode = gtk4::Label::builder().xalign(0.0).build();
    let lbl_lang = gtk4::Label::builder().xalign(0.0).build();

    config_grid.attach(&lbl_relay, 0, 0, 1, 1);
    config_grid.attach(&relay_entry, 1, 0, 1, 1);
    config_grid.attach(&lbl_room, 2, 0, 1, 1);
    config_grid.attach(&room_entry, 3, 0, 1, 1);

    config_grid.attach(&lbl_max_text, 0, 1, 1, 1);
    config_grid.attach(&max_text_spin, 1, 1, 1, 1);
    config_grid.attach(&lbl_max_img, 2, 1, 1, 1);
    config_grid.attach(&max_image_spin, 3, 1, 1, 1);

    config_grid.attach(&lbl_max_file, 0, 2, 1, 1);
    config_grid.attach(&max_file_spin, 1, 2, 3, 1);

    config_grid.attach(&lbl_img_mode, 0, 3, 1, 1);
    config_grid.attach(&image_mode_combo, 1, 3, 3, 1);
    config_grid.attach(&mode_hint, 1, 4, 3, 1);

    config_grid.attach(&lbl_lang, 0, 5, 1, 1);
    config_grid.attach(&language_combo, 1, 5, 3, 1);

    config_frame.set_child(Some(&config_grid));

    let services_frame = gtk4::Frame::builder().label(t(initial_lang, K::SectionServices)).build();
    services_frame.set_margin_bottom(6);
    let services_grid = gtk4::Grid::builder().row_spacing(6).column_spacing(10).build();
    services_grid.set_margin_top(10);
    services_grid.set_margin_bottom(10);
    services_grid.set_margin_start(10);
    services_grid.set_margin_end(10);

    let start_all = gtk4::Button::with_label(t(initial_lang, K::BtnStartAll));
    let stop_all = gtk4::Button::with_label(t(initial_lang, K::BtnStopAll));

    let start_relay_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStartRelay));
    let stop_relay_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStopRelay));
    let start_watch_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStartWatch));
    let stop_watch_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStopWatch));
    let start_apply_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStartApply));
    let stop_apply_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStopApply));

    let status_relay = gtk4::Label::builder().xalign(0.0).build();
    let status_watch = gtk4::Label::builder().xalign(0.0).build();
    let status_apply = gtk4::Label::builder().xalign(0.0).build();
    let status_relay_tcp = gtk4::Label::builder().xalign(0.0).build();
    status_relay.add_css_class("dim-label");
    status_watch.add_css_class("dim-label");
    status_apply.add_css_class("dim-label");
    status_relay_tcp.add_css_class("dim-label");
    status_relay_tcp.set_text(t(initial_lang, K::StatusChecking));

    let svc_lbl_relay = gtk4::Label::builder().xalign(0.0).label("relay").build();
    let svc_lbl_watch = gtk4::Label::builder().xalign(0.0).label("node wl-watch").build();
    let svc_lbl_apply = gtk4::Label::builder().xalign(0.0).label("node wl-apply").build();
    let svc_lbl_relay_tcp = gtk4::Label::builder().xalign(0.0).label(t(initial_lang, K::LabelRelayTcp)).build();

    services_grid.attach(&svc_lbl_relay, 0, 0, 1, 1);
    services_grid.attach(&status_relay, 1, 0, 1, 1);
    services_grid.attach(&start_relay_btn, 2, 0, 1, 1);
    services_grid.attach(&stop_relay_btn, 3, 0, 1, 1);

    services_grid.attach(&svc_lbl_watch, 0, 1, 1, 1);
    services_grid.attach(&status_watch, 1, 1, 1, 1);
    services_grid.attach(&start_watch_btn, 2, 1, 1, 1);
    services_grid.attach(&stop_watch_btn, 3, 1, 1, 1);

    services_grid.attach(&svc_lbl_apply, 0, 2, 1, 1);
    services_grid.attach(&status_apply, 1, 2, 1, 1);
    services_grid.attach(&start_apply_btn, 2, 2, 1, 1);
    services_grid.attach(&stop_apply_btn, 3, 2, 1, 1);

    // Connection status (client -> relay TCP reachability)
    services_grid.attach(&svc_lbl_relay_tcp, 0, 3, 1, 1);
    // Span across remaining columns (no action buttons here)
    services_grid.attach(&status_relay_tcp, 1, 3, 3, 1);

    let services_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    services_actions.set_margin_top(10);
    services_actions.set_margin_start(10);
    services_actions.set_margin_end(10);
    services_actions.append(&start_all);
    services_actions.append(&stop_all);

    let services_box = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    services_box.append(&services_actions);
    services_box.append(&services_grid);
    services_frame.set_child(Some(&services_box));

    let test_frame = gtk4::Frame::builder().label(t(initial_lang, K::SectionTest)).build();
    let test_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    test_box.set_margin_top(10);
    test_box.set_margin_bottom(10);
    test_box.set_margin_start(10);
    test_box.set_margin_end(10);

    let send_test_text = gtk4::Button::with_label(t(initial_lang, K::BtnSendTestText));
    let send_test_image = gtk4::Button::with_label(t(initial_lang, K::BtnSendTestImage));
    let send_test_file = gtk4::Button::with_label(t(initial_lang, K::BtnSendTestFile));
    let show_clip_types = gtk4::Button::with_label(t(initial_lang, K::BtnShowClipTypes));
    test_box.append(&send_test_text);
    test_box.append(&send_test_image);
    test_box.append(&send_test_file);
    test_box.append(&show_clip_types);
    test_frame.set_child(Some(&test_box));

    control_box.append(&config_frame);
    control_box.append(&services_frame);
    control_box.append(&test_frame);

    // Make the Control page scrollable so the window can be resized smaller without
    // GTK repeatedly warning about measuring below minimum height.
    let control_scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&control_box)
        .build();
    control_scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
    control_scroll.set_margin_top(12);
    control_scroll.set_margin_bottom(12);
    control_scroll.set_margin_start(12);
    control_scroll.set_margin_end(12);

    // --- Log view ---
    let log_buf = gtk4::TextBuffer::new(None);
    let log_view = gtk4::TextView::builder()
        .buffer(&log_buf)
        .editable(false)
        .monospace(true)
        .build();
    let log_scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&log_view)
        .build();

    let clear_logs = gtk4::Button::with_label(t(initial_lang, K::BtnClearLogs));
    clear_logs.connect_clicked(clone!(@weak log_buf => move |_| {
        log_buf.set_text("");
    }));

    let logs_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    logs_box.set_margin_top(12);
    logs_box.set_margin_bottom(12);
    logs_box.set_margin_start(12);
    logs_box.set_margin_end(12);
    logs_box.append(&clear_logs);
    logs_box.append(&log_scroll);

    // --- History view ---
    let history_buf = gtk4::TextBuffer::new(None);
    let history_view = gtk4::TextView::builder()
        .buffer(&history_buf)
        .editable(false)
        .monospace(true)
        .build();
    let history_scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&history_view)
        .build();

    let clear_history = gtk4::Button::with_label(t(initial_lang, K::BtnClearHistory));
    let history_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    history_box.set_margin_top(12);
    history_box.set_margin_bottom(12);
    history_box.set_margin_start(12);
    history_box.set_margin_end(12);
    history_box.append(&clear_history);
    history_box.append(&history_scroll);

    // --- Help view ---
    let help_buf = gtk4::TextBuffer::new(None);
    help_buf.set_text(&help_text(initial_lang));
    let help_view = gtk4::TextView::builder()
        .buffer(&help_buf)
        .editable(false)
        .monospace(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .build();
    let help_scroll = gtk4::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&help_view)
        .build();
    help_scroll.set_margin_top(12);
    help_scroll.set_margin_bottom(12);
    help_scroll.set_margin_start(12);
    help_scroll.set_margin_end(12);

    // log receiver -> append to text buffer
    install_log_drain(log_rx, log_buf.clone());

    // Config save/reload wiring is handled in a dedicated module.

    // --- Service UI state sync (status + button sensitivity) ---
    let service_widgets = ServiceWidgets {
        start_all: start_all.clone(),
        stop_all: stop_all.clone(),
        start_relay_btn: start_relay_btn.clone(),
        stop_relay_btn: stop_relay_btn.clone(),
        start_watch_btn: start_watch_btn.clone(),
        stop_watch_btn: stop_watch_btn.clone(),
        start_apply_btn: start_apply_btn.clone(),
        stop_apply_btn: stop_apply_btn.clone(),
        status_relay: status_relay.clone(),
        status_watch: status_watch.clone(),
        status_apply: status_apply.clone(),
    };
    let update_services_ui: Rc<dyn Fn()> = make_update_services_ui(procs.clone(), lang_state.clone(), service_widgets.clone());

    // --- Apply language (runtime refresh) ---
    let apply_lang = make_apply_lang(ApplyLangCtx {
        window: window.clone(),
        stack: stack.clone(),
        language_combo: language_combo.clone(),
        image_mode_combo: image_mode_combo.clone(),
        suppress_lang_combo: suppress_lang_combo.clone(),
        suppress_mode_combo: suppress_mode_combo.clone(),
        mode_hint: mode_hint.clone(),
        help_buf: help_buf.clone(),
        clear_logs_btn: clear_logs.clone(),
        clear_history_btn: clear_history.clone(),
        reload_btn: reload_btn.clone(),
        lbl_relay: lbl_relay.clone(),
        lbl_room: lbl_room.clone(),
        lbl_max_text: lbl_max_text.clone(),
        lbl_max_img: lbl_max_img.clone(),
        lbl_max_file: lbl_max_file.clone(),
        lbl_img_mode: lbl_img_mode.clone(),
        lbl_lang: lbl_lang.clone(),
        lbl_relay_tcp: svc_lbl_relay_tcp.clone(),
        start_relay: start_relay_btn.clone(),
        stop_relay: stop_relay_btn.clone(),
        start_watch: start_watch_btn.clone(),
        stop_watch: stop_watch_btn.clone(),
        start_apply: start_apply_btn.clone(),
        stop_apply: stop_apply_btn.clone(),
        start_all: start_all.clone(),
        stop_all: stop_all.clone(),
        send_test_text: send_test_text.clone(),
        send_test_image: send_test_image.clone(),
        send_test_file: send_test_file.clone(),
        show_clip_types: show_clip_types.clone(),
        update_services_ui: update_services_ui.clone(),
    });

    // --- Relay reachability probe ---
    install_relay_probe(
        relay_entry.clone(),
        status_relay_tcp.clone(),
        log_tx.clone(),
        lang_state.clone(),
    );

    // --- History refresh ---
    install_history_refresh(
        history_buf.clone(),
        clear_history.clone(),
        log_tx.clone(),
        lang_state.clone(),
    );

    // --- Button handlers (services) ---
    connect_service_handlers(
        procs.clone(),
        log_tx.clone(),
        update_services_ui.clone(),
        service_widgets.clone(),
        ServiceConfigInputs {
            relay_entry: relay_entry.clone(),
            room_entry: room_entry.clone(),
            max_text_spin: max_text_spin.clone(),
            max_image_spin: max_image_spin.clone(),
            max_file_spin: max_file_spin.clone(),
            image_mode_combo: image_mode_combo.clone(),
        },
    );

    // --- Diagnostics handlers ---
    connect_diagnostics_handlers(
        diagnostics::DiagnosticsWidgets {
            send_test_text: send_test_text.clone(),
            send_test_image: send_test_image.clone(),
            send_test_file: send_test_file.clone(),
            show_clip_types: show_clip_types.clone(),
        },
        diagnostics::DiagnosticsInputs {
            window: window.clone(),
            relay_entry: relay_entry.clone(),
            room_entry: room_entry.clone(),
            max_image_spin: max_image_spin.clone(),
            max_file_spin: max_file_spin.clone(),
            image_mode_combo: image_mode_combo.clone(),
        },
        log_tx.clone(),
        lang_state.clone(),
    );

    // --- Config save/reload + change wiring ---
    connect_config_wiring(config_wiring::ConfigWiringCtx {
        cfg_path: cfg_path.clone(),
        log_tx: log_tx.clone(),
        lang_state: lang_state.clone(),
        apply_lang: apply_lang.clone(),
        update_services_ui: update_services_ui.clone(),
        ui: config_wiring::ConfigWidgets {
            relay_entry: relay_entry.clone(),
            room_entry: room_entry.clone(),
            max_text_spin: max_text_spin.clone(),
            max_image_spin: max_image_spin.clone(),
            max_file_spin: max_file_spin.clone(),
            language_combo: language_combo.clone(),
            image_mode_combo: image_mode_combo.clone(),
            mode_hint: mode_hint.clone(),
            reload_btn: reload_btn.clone(),
        },
        suppress_save_cfg: suppress_save_cfg.clone(),
        suppress_lang_combo: suppress_lang_combo.clone(),
        suppress_mode_combo: suppress_mode_combo.clone(),
    });

    // Kill child processes when window closes
    install_close_handler(&window, procs.clone(), log_tx.clone());

    // Stack pages
    stack.add_titled(&control_scroll, Some(PAGE_CONTROL), t(initial_lang, K::TabControl));
    stack.add_titled(&history_box, Some(PAGE_HISTORY), t(initial_lang, K::TabHistory));
    stack.add_titled(&logs_box, Some(PAGE_LOGS), t(initial_lang, K::TabLogs));
    stack.add_titled(&help_scroll, Some(PAGE_HELP), t(initial_lang, K::TabHelp));

    // Initialize all i18n texts for labels created without initial text.
    // Do this after pages are added, so stack page title updates are safe.
    apply_lang(initial_lang);

    // Initialize services UI state.
    update_services_ui();

    // Prune exited child processes and keep UI state correct.
    install_prune_timer(procs.clone(), log_tx.clone(), update_services_ui.clone());

    root.append(&stack);
    window.set_child(Some(&root));
    window.show();

    let _ = log_tx.send(format!("config: {}", cfg_path.display()));
}
