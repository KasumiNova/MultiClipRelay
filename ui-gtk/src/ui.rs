mod wl_clipboard_logs;
mod table;
use glib::clone;
use gtk4::prelude::*;
use gtk4::gdk;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};

use crate::config::{config_path, load_config, save_config};
use crate::i18n::{
    detect_lang_from_env, help_text, image_mode_hint_text, parse_lang_id,
    populate_image_mode_combo, t, Lang, K,
};
use crate::procs::Procs;
use crate::systemd;

mod apply_lang;
mod config_wiring;
mod connection;
mod constants;
mod diagnostics;
mod helpers;
mod history;
mod services;
mod timers;

use self::apply_lang::{make_apply_lang, ApplyLangCtx};
use self::config_wiring::connect_config_wiring;
use self::connection::install_relay_probe;
use self::constants::{LANG_AUTO_ID, PAGE_ACTIVITY, PAGE_CONTROL, PAGE_HELP};
use self::diagnostics::connect_diagnostics_handlers;
use self::history::{install_history_refresh, make_history_table};
use self::services::{
    connect_service_handlers, make_update_services_ui, ServiceConfigInputs, ServiceWidgets,
};
use self::timers::{install_close_handler, install_log_drain, install_prune_timer};
use self::table::{make_tabbed_table, ColumnSpec};
use self::wl_clipboard_logs::build_wl_clipboard_logs_widget;

pub fn build_ui(app: &gtk4::Application) {
    if let Some(win) = app
        .active_window()
        .or_else(|| app.windows().into_iter().next())
    {
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

    let use_systemd = systemd::enabled_from_env_or_auto();
    if use_systemd {
        // Keep systemd env in sync with current config on startup.
        let _ = systemd::write_env_from_ui_config(&cfg);
    }

    let svc_status: Arc<Mutex<systemd::ServiceStatus>> = Arc::new(Mutex::new(systemd::ServiceStatus::default()));
    if use_systemd {
        let status = svc_status.clone();
        std::thread::spawn(move || loop {
            *status.lock().unwrap() = systemd::status_snapshot();
            std::thread::sleep(std::time::Duration::from_millis(700));
        });
    }

    let (log_tx, log_rx) = mpsc::channel::<String>();

        // App CSS (row zebra stripes for ColumnView-based tables).
        if let Some(display) = gdk::Display::default() {
                let provider = gtk4::CssProvider::new();
                // Keep it subtle so selection highlight still stands out.
                // Use theme colors so it works in both light and dark themes.
                let css = r#"
.mcr-cell {
    padding: 4px 10px;
}

/* Make small in-cell action buttons not inflate row height. */
.mcr-compact-btn {
    padding: 1px 8px;
    min-height: 0;
    min-width: 0;
}

/* Force consistent row height across all columns. */
columnview row,
columnview listview row {
    min-height: 28px;
}

/* Zebra stripes: color the whole row (not individual cells) to avoid a chopped look. */
columnview row:nth-child(odd),
columnview listview row:nth-child(odd) {
    background-color: transparent;
}

columnview row:nth-child(even),
columnview listview row:nth-child(even) {
    background-color: alpha(@theme_fg_color, 0.028);
}

columnview row:hover:not(:selected),
columnview listview row:hover:not(:selected) {
    background-color: alpha(@theme_fg_color, 0.050);
}
"#;
                provider.load_from_data(css);
                gtk4::style_context_add_provider_for_display(
                        &display,
                        &provider,
                        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
        }

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
    let x11_poll_adj = gtk4::Adjustment::new(
        cfg.x11_poll_interval_ms as f64,
        10.0,
        10_000.0,
        10.0,
        50.0,
        0.0,
    );
    let max_text_spin = gtk4::SpinButton::new(Some(&max_text_adj), 1024.0, 0);
    let max_image_spin = gtk4::SpinButton::new(Some(&max_image_adj), 1024.0, 0);
    let max_file_spin = gtk4::SpinButton::new(Some(&max_file_adj), 1024.0, 0);
    let x11_poll_spin = gtk4::SpinButton::new(Some(&x11_poll_adj), 10.0, 0);

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

    let config_frame = gtk4::Frame::builder()
        .label(t(initial_lang, K::SectionConfig))
        .build();
    config_frame.set_margin_bottom(6);
    let config_grid = gtk4::Grid::builder()
        .row_spacing(6)
        .column_spacing(8)
        .build();
    config_grid.set_margin_top(10);
    config_grid.set_margin_bottom(10);
    config_grid.set_margin_start(10);
    config_grid.set_margin_end(10);

    let lbl_relay = gtk4::Label::builder().xalign(0.0).build();
    let lbl_room = gtk4::Label::builder().xalign(0.0).build();
    let lbl_max_text = gtk4::Label::builder().xalign(0.0).build();
    let lbl_max_img = gtk4::Label::builder().xalign(0.0).build();
    let lbl_max_file = gtk4::Label::builder().xalign(0.0).build();
    let lbl_x11_poll = gtk4::Label::builder().xalign(0.0).build();
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

    config_grid.attach(&lbl_x11_poll, 0, 3, 1, 1);
    config_grid.attach(&x11_poll_spin, 1, 3, 3, 1);

    config_grid.attach(&lbl_img_mode, 0, 4, 1, 1);
    config_grid.attach(&image_mode_combo, 1, 4, 3, 1);
    config_grid.attach(&mode_hint, 1, 5, 3, 1);

    config_grid.attach(&lbl_lang, 0, 6, 1, 1);
    config_grid.attach(&language_combo, 1, 6, 3, 1);

    config_frame.set_child(Some(&config_grid));

    let services_frame = gtk4::Frame::builder()
        .label(t(initial_lang, K::SectionServices))
        .build();
    services_frame.set_margin_bottom(6);
    let services_grid = gtk4::Grid::builder()
        .row_spacing(6)
        .column_spacing(10)
        .build();
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
    let start_x11_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStartX11Sync));
    let stop_x11_btn = gtk4::Button::with_label(t(initial_lang, K::BtnStopX11Sync));

    let status_relay = gtk4::Label::builder().xalign(0.0).build();
    let status_watch = gtk4::Label::builder().xalign(0.0).build();
    let status_apply = gtk4::Label::builder().xalign(0.0).build();
    let status_x11 = gtk4::Label::builder().xalign(0.0).build();
    let status_relay_tcp = gtk4::Label::builder().xalign(0.0).build();
    status_relay.add_css_class("dim-label");
    status_watch.add_css_class("dim-label");
    status_apply.add_css_class("dim-label");
    status_x11.add_css_class("dim-label");
    status_relay_tcp.add_css_class("dim-label");
    status_relay_tcp.set_text(t(initial_lang, K::StatusChecking));

    let svc_lbl_relay = gtk4::Label::builder().xalign(0.0).label("relay").build();
    let svc_lbl_watch = gtk4::Label::builder()
        .xalign(0.0)
        .label("node wl-watch")
        .build();
    let svc_lbl_apply = gtk4::Label::builder()
        .xalign(0.0)
        .label("node wl-apply")
        .build();
    let svc_lbl_x11 = gtk4::Label::builder()
        .xalign(0.0)
        .label("node x11-sync")
        .build();
    let svc_lbl_relay_tcp = gtk4::Label::builder()
        .xalign(0.0)
        .label(t(initial_lang, K::LabelRelayTcp))
        .build();

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

    services_grid.attach(&svc_lbl_x11, 0, 3, 1, 1);
    services_grid.attach(&status_x11, 1, 3, 1, 1);
    services_grid.attach(&start_x11_btn, 2, 3, 1, 1);
    services_grid.attach(&stop_x11_btn, 3, 3, 1, 1);

    // Connection status (client -> relay TCP reachability)
    services_grid.attach(&svc_lbl_relay_tcp, 0, 4, 1, 1);
    // Span across remaining columns (no action buttons here)
    services_grid.attach(&status_relay_tcp, 1, 4, 3, 1);

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

    let test_frame = gtk4::Frame::builder()
        .label(t(initial_lang, K::SectionTest))
        .build();
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

    // --- Activity view (sub-tabs) ---
    // 1) Sync history
    let history_table = make_history_table(initial_lang, &cfg.history_columns);
    let clear_history = gtk4::Button::with_label(t(initial_lang, K::BtnClearHistory));

    // Column visibility settings (persisted in ui.toml).
    let columns_btn = gtk4::MenuButton::builder()
        .label(match initial_lang {
            Lang::ZhCn => "列",
            Lang::En => "Columns",
        })
        .build();
    columns_btn.add_css_class("flat");

    let columns_pop = gtk4::Popover::new();
    let columns_box = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    columns_box.set_margin_top(0);
    columns_box.set_margin_bottom(0);
    columns_box.set_margin_start(10);
    columns_box.set_margin_end(10);

    // Friendly labels for column ids.
    let col_label = move |id: &str| -> String {
        match (initial_lang, id) {
            (Lang::ZhCn, "time") => "时间".into(),
            (Lang::ZhCn, "dir") => "方向".into(),
            (Lang::ZhCn, "name") => "名称".into(),
            (Lang::ZhCn, "peer") => "peer(id)".into(),
            (Lang::ZhCn, "kind") => "类型".into(),
            (Lang::ZhCn, "bytes") => "大小".into(),
            (Lang::ZhCn, "extra") => "详情".into(),
            (Lang::ZhCn, "preview") => "预览".into(),
            (_, other) => other.to_string(),
        }
    };

    for (id, col) in history_table.columns.iter() {
        let id2 = id.clone();
        let col2 = col.clone();
        let cfg_path2 = cfg_path.clone();

        let cb = gtk4::CheckButton::with_label(&col_label(&id2));
        cb.set_active(col2.is_visible());
        cb.connect_toggled(move |c| {
            let v = c.is_active();
            col2.set_visible(v);

            // Best-effort persistence.
            let mut cfg2 = load_config(&cfg_path2).unwrap_or_default();
            cfg2.history_columns.insert(id2.clone(), v);
            let _ = save_config(&cfg_path2, &cfg2);
        });
        columns_box.append(&cb);
    }

    columns_pop.set_child(Some(&columns_box));
    columns_btn.set_popover(Some(&columns_pop));

    let history_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    history_actions.set_margin_top(2);
    history_actions.set_margin_bottom(2);
    history_actions.set_margin_start(4);
    history_actions.set_margin_end(4);
    history_actions.append(&clear_history);
    let history_actions_spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    history_actions_spacer.set_hexpand(true);
    history_actions.append(&history_actions_spacer);
    history_actions.append(&columns_btn);

    let history_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    history_box.append(&history_actions);
    history_box.append(&history_table.scroll);

    // 2) App logs
    let app_logs = make_tabbed_table(&[
        ColumnSpec { title: "time", fixed_width: Some(140), expand: false, resizable: true, ellipsize: true },
        ColumnSpec { title: "src", fixed_width: Some(170), expand: false, resizable: true, ellipsize: true },
        ColumnSpec { title: "message", fixed_width: None, expand: true, resizable: true, ellipsize: false },
    ]);

    let clear_logs = gtk4::Button::with_label(t(initial_lang, K::BtnClearLogs));
    clear_logs.connect_clicked(clone!(@strong app_logs => move |_| {
        app_logs.store.remove_all();
    }));

    let logs_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    logs_box.append(&clear_logs);
    logs_box.append(&app_logs.scroll);

    // 3) System clipboard logs (systemd journal)
    let (clip_logs_widget, clip_logs_alive) = build_wl_clipboard_logs_widget(initial_lang);
    window.connect_close_request(clone!(@strong clip_logs_alive => @default-return glib::Propagation::Proceed, move |_| {
        clip_logs_alive.store(false, std::sync::atomic::Ordering::Relaxed);
        glib::Propagation::Proceed
    }));

    let clip_logs_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    clip_logs_box.append(&clip_logs_widget);

    let activity_notebook = gtk4::Notebook::new();
    activity_notebook.set_vexpand(true);
    activity_notebook.set_hexpand(true);

    let tab_history_lbl = gtk4::Label::new(Some(t(initial_lang, K::SubTabHistory)));
    let tab_app_logs_lbl = gtk4::Label::new(Some(t(initial_lang, K::SubTabAppLogs)));
    let tab_clip_logs_lbl = gtk4::Label::new(Some(t(initial_lang, K::SubTabClipboardLogs)));

    activity_notebook.append_page(&history_box, Some(&tab_history_lbl));
    activity_notebook.append_page(&logs_box, Some(&tab_app_logs_lbl));
    activity_notebook.append_page(&clip_logs_box, Some(&tab_clip_logs_lbl));

    let activity_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    activity_box.set_margin_top(12);
    activity_box.set_margin_bottom(12);
    activity_box.set_margin_start(12);
    activity_box.set_margin_end(12);
    activity_box.append(&activity_notebook);

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

    // log receiver -> append to app logs table
    install_log_drain(log_rx, app_logs.store.clone(), app_logs.scroll.clone());

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
        start_x11_btn: start_x11_btn.clone(),
        stop_x11_btn: stop_x11_btn.clone(),
        status_relay: status_relay.clone(),
        status_watch: status_watch.clone(),
        status_apply: status_apply.clone(),
        status_x11: status_x11.clone(),
    };
    let update_services_ui: Rc<dyn Fn()> =
        make_update_services_ui(procs.clone(), svc_status.clone(), use_systemd, lang_state.clone(), service_widgets.clone());

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

        tab_history_lbl: tab_history_lbl.clone(),
        tab_app_logs_lbl: tab_app_logs_lbl.clone(),
        tab_clipboard_logs_lbl: tab_clip_logs_lbl.clone(),
        lbl_relay: lbl_relay.clone(),
        lbl_room: lbl_room.clone(),
        lbl_max_text: lbl_max_text.clone(),
        lbl_max_img: lbl_max_img.clone(),
        lbl_max_file: lbl_max_file.clone(),
            lbl_x11_poll: lbl_x11_poll.clone(),
        lbl_img_mode: lbl_img_mode.clone(),
        lbl_lang: lbl_lang.clone(),
        lbl_relay_tcp: svc_lbl_relay_tcp.clone(),
        start_relay: start_relay_btn.clone(),
        stop_relay: stop_relay_btn.clone(),
        start_watch: start_watch_btn.clone(),
        stop_watch: stop_watch_btn.clone(),
        start_apply: start_apply_btn.clone(),
        stop_apply: stop_apply_btn.clone(),
        start_x11_sync: start_x11_btn.clone(),
        stop_x11_sync: stop_x11_btn.clone(),
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
        history_table.store.clone(),
        history_table.scroll.clone(),
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
                        x11_poll_spin: x11_poll_spin.clone(),
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
                        x11_poll_spin: x11_poll_spin.clone(),
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

    // Kill child processes when window closes (non-systemd mode only)
    install_close_handler(&window, use_systemd, procs.clone(), log_tx.clone());

    // Stack pages
    stack.add_titled(
        &control_scroll,
        Some(PAGE_CONTROL),
        t(initial_lang, K::TabControl),
    );
    stack.add_titled(
        &activity_box,
        Some(PAGE_ACTIVITY),
        t(initial_lang, K::TabActivity),
    );
    stack.add_titled(&help_scroll, Some(PAGE_HELP), t(initial_lang, K::TabHelp));

    // Initialize all i18n texts for labels created without initial text.
    // Do this after pages are added, so stack page title updates are safe.
    apply_lang(initial_lang);

    // Initialize services UI state.
    update_services_ui();

    // Prune exited child processes and keep UI state correct.
    install_prune_timer(procs.clone(), use_systemd, log_tx.clone(), update_services_ui.clone());

    root.append(&stack);
    window.set_child(Some(&root));
    window.show();

    let _ = log_tx.send(format!("config: {}", cfg_path.display()));
}
