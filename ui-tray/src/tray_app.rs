use crate::config::{load_config, UiConfig};
use crate::i18n::{detect_lang_from_env, parse_lang_id, t, Lang, K};
use crate::procs::{find_sibling_binary, spawn_ui_gtk, terminate_child, Procs};
use crate::systemd;

use ksni::{menu::StandardItem, Handle, Status, ToolTip, Tray};

use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct MultiClipRelayTray {
    state: Arc<Mutex<AppState>>,
}

#[derive(Debug, Copy, Clone, Default)]
struct ServiceStatus {
    relay: bool,
    watch: bool,
    apply: bool,
    x11: bool,
}

impl MultiClipRelayTray {
    pub fn new(cfg: UiConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(AppState::new(cfg))),
        }
    }

    fn open_control_panel(&self) {
        {
            let mut st = self.state.lock().unwrap();
            st.opened_count = st.opened_count.saturating_add(1);
        }

        if let Err(e) = spawn_ui_gtk() {
            eprintln!("failed to spawn ui-gtk: {e:?}");
        }
    }

    fn reload_config(&self) {
        match load_config() {
            Ok(cfg) => {
                let mut st = self.state.lock().unwrap();
                st.cfg = cfg;
                if st.systemd {
                    let _ = systemd::write_env_from_ui_config(&st.cfg);
                }
            }
            Err(e) => {
                eprintln!("failed to reload config: {e:?}");
            }
        }
    }

    fn start_all(&self) {
        self.start_relay();
        self.start_apply();
        self.start_watch();
        self.start_x11_sync();
    }

    fn stop_all(&self) {
        self.stop_x11_sync();
        self.stop_watch();
        self.stop_apply();
        self.stop_relay();
    }

    fn start_relay(&self) {
        let mut st = self.state.lock().unwrap();
        if st.systemd {
            let _ = systemd::write_env_from_ui_config(&st.cfg);
            if let Err(e) = systemd::start(systemd::UNIT_RELAY) {
                eprintln!("failed to start relay (systemd): {e:?}");
            }
            return;
        }
        if st.procs.relay.is_some() {
            return;
        }

        let relay_bin = find_sibling_binary("multicliprelay-relay")
            .or_else(|| find_sibling_binary("relay"))
            .or_else(|| which::which("multicliprelay-relay").ok())
            .or_else(|| which::which("relay").ok())
            .unwrap_or_else(|| PathBuf::from("multicliprelay-relay"));

        let mut cmd = Command::new(relay_bin);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(bind) = st.cfg.relay_bind_hint() {
            cmd.arg("--bind").arg(bind);
        }

        match cmd.spawn() {
            Ok(child) => st.procs.relay = Some(child),
            Err(e) => eprintln!("failed to start relay: {e:?}"),
        }
    }

    fn stop_relay(&self) {
        let mut st = self.state.lock().unwrap();
        if st.systemd {
            if let Err(e) = systemd::stop(systemd::UNIT_RELAY) {
                eprintln!("failed to stop relay (systemd): {e:?}");
            }
            return;
        }
        if let Some(child) = st.procs.relay.take() {
            terminate_child(child, "relay");
        }
    }

    fn start_watch(&self) {
        let (cfg, systemd_mode, already_child) = {
            let st = self.state.lock().unwrap();
            (st.cfg.clone(), st.systemd, st.procs.watch.is_some())
        };

        if systemd_mode {
            if systemd::is_active(systemd::UNIT_WL_WATCH) {
                return;
            }
            let _ = systemd::write_env_from_ui_config(&cfg);
            if let Err(e) = systemd::start(systemd::UNIT_WL_WATCH) {
                eprintln!("failed to start wl-watch (systemd): {e:?}");
            }
            return;
        }

        if already_child {
            return;
        }

        let node_bin = find_sibling_binary("multicliprelay-node")
            .or_else(|| find_sibling_binary("node"))
            .or_else(|| which::which("multicliprelay-node").ok())
            .or_else(|| which::which("node").ok())
            .unwrap_or_else(|| PathBuf::from("multicliprelay-node"));

        let mut cmd = Command::new(node_bin);
        cmd.arg("wl-watch")
            .arg("--room")
            .arg(cfg.room)
            .arg("--relay")
            .arg(cfg.relay_addr)
            .arg("--mode")
            .arg("watch")
            .arg("--max-text-bytes")
            .arg(cfg.max_text_bytes.to_string())
            .arg("--max-image-bytes")
            .arg(cfg.max_image_bytes.to_string())
            .arg("--max-file-bytes")
            .arg(cfg.max_file_bytes.to_string())
            .arg("--image-mode")
            .arg(cfg.image_mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                let mut st = self.state.lock().unwrap();
                st.procs.watch = Some(child);
            }
            Err(e) => eprintln!("failed to start wl-watch: {e:?}"),
        }
    }

    fn stop_watch(&self) {
        let mut st = self.state.lock().unwrap();
        if st.systemd {
            if let Err(e) = systemd::stop(systemd::UNIT_WL_WATCH) {
                eprintln!("failed to stop wl-watch (systemd): {e:?}");
            }
            return;
        }
        if let Some(child) = st.procs.watch.take() {
            terminate_child(child, "node wl-watch");
        }
    }

    fn start_apply(&self) {
        let (cfg, systemd_mode, already_child) = {
            let st = self.state.lock().unwrap();
            (st.cfg.clone(), st.systemd, st.procs.apply.is_some())
        };

        if systemd_mode {
            if systemd::is_active(systemd::UNIT_WL_APPLY) {
                return;
            }
            let _ = systemd::write_env_from_ui_config(&cfg);
            if let Err(e) = systemd::start(systemd::UNIT_WL_APPLY) {
                eprintln!("failed to start wl-apply (systemd): {e:?}");
            }
            return;
        }

        if already_child {
            return;
        }

        let node_bin = find_sibling_binary("multicliprelay-node")
            .or_else(|| find_sibling_binary("node"))
            .or_else(|| which::which("multicliprelay-node").ok())
            .or_else(|| which::which("node").ok())
            .unwrap_or_else(|| PathBuf::from("multicliprelay-node"));

        let mut cmd = Command::new(node_bin);
        cmd.arg("wl-apply")
            .arg("--room")
            .arg(cfg.room)
            .arg("--relay")
            .arg(cfg.relay_addr)
            .arg("--image-mode")
            .arg(cfg.image_mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                let mut st = self.state.lock().unwrap();
                st.procs.apply = Some(child);
            }
            Err(e) => eprintln!("failed to start wl-apply: {e:?}"),
        }
    }

    fn stop_apply(&self) {
        let mut st = self.state.lock().unwrap();
        if st.systemd {
            if let Err(e) = systemd::stop(systemd::UNIT_WL_APPLY) {
                eprintln!("failed to stop wl-apply (systemd): {e:?}");
            }
            return;
        }
        if let Some(child) = st.procs.apply.take() {
            terminate_child(child, "node wl-apply");
        }
    }

    fn start_x11_sync(&self) {
        let st = self.state.lock().unwrap();
        if !st.systemd {
            // In non-systemd mode we don't manage x11-sync from tray.
            return;
        }
        if !systemd::node_supports_x11_sync() {
            eprintln!("multicliprelay-node does not support x11-sync; please upgrade/reinstall binaries (or adjust unit ExecStart)");
            return;
        }
        let _ = systemd::write_env_from_ui_config(&st.cfg);
        if let Err(e) = systemd::start(systemd::UNIT_X11_SYNC) {
            eprintln!("failed to start x11-sync (systemd): {e:?}");
        }
    }

    fn stop_x11_sync(&self) {
        let st = self.state.lock().unwrap();
        if !st.systemd {
            return;
        }
        if let Err(e) = systemd::stop(systemd::UNIT_X11_SYNC) {
            eprintln!("failed to stop x11-sync (systemd): {e:?}");
        }
    }

    fn quit_and_cleanup(&self) {
        self.stop_all();
        std::process::exit(0);
    }

    fn prune_exited(&self) {
        let mut st = self.state.lock().unwrap();
        if st.systemd {
            st.status = ServiceStatus {
                relay: systemd::is_active(systemd::UNIT_RELAY),
                watch: systemd::is_active(systemd::UNIT_WL_WATCH),
                apply: systemd::is_active(systemd::UNIT_WL_APPLY),
                x11: systemd::is_active(systemd::UNIT_X11_SYNC),
            };
            return;
        }
        if let Some(child) = st.procs.relay.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                st.procs.relay = None;
            }
        }
        if let Some(child) = st.procs.watch.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                st.procs.watch = None;
            }
        }
        if let Some(child) = st.procs.apply.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                st.procs.apply = None;
            }
        }
    }
}

pub struct AppState {
    cfg: UiConfig,
    procs: Procs,
    opened_count: u64,

    systemd: bool,
    status: ServiceStatus,
}

impl AppState {
    fn new(cfg: UiConfig) -> Self {
        let use_systemd = systemd::available();
        if use_systemd {
            let _ = systemd::write_env_from_ui_config(&cfg);
        }
        Self {
            cfg,
            procs: Procs::default(),
            opened_count: 0,
            systemd: use_systemd,
            status: ServiceStatus {
                relay: use_systemd && systemd::is_active(systemd::UNIT_RELAY),
                watch: use_systemd && systemd::is_active(systemd::UNIT_WL_WATCH),
                apply: use_systemd && systemd::is_active(systemd::UNIT_WL_APPLY),
                x11: use_systemd && systemd::is_active(systemd::UNIT_X11_SYNC),
            },
        }
    }

    fn lang(&self) -> Lang {
        if self.cfg.language == "auto" {
            detect_lang_from_env()
        } else {
            parse_lang_id(&self.cfg.language).unwrap_or_else(detect_lang_from_env)
        }
    }

    fn service_status(&self) -> ServiceStatus {
        if self.systemd {
            self.status
        } else {
            ServiceStatus {
                relay: self.procs.relay.is_some(),
                watch: self.procs.watch.is_some(),
                apply: self.procs.apply.is_some(),
                x11: false,
            }
        }
    }
}

impl Tray for MultiClipRelayTray {
    fn icon_name(&self) -> String {
        "edit-paste".to_string()
    }

    fn title(&self) -> String {
        "MultiClipRelay".to_string()
    }

    fn id(&self) -> String {
        "multicliprelay".to_string()
    }

    fn status(&self) -> Status {
        // Important: many SNI hosts (e.g. waybar) hide items with `Passive` status.
        // Users expect the tray icon to be visible even when MultiClipRelay is idle.
        Status::Active
    }

    fn tool_tip(&self) -> ToolTip {
        let st = self.state.lock().unwrap();
        let lang = st.lang();
        let ss = st.service_status();
        let relay_on = ss.relay;
        let watch_on = ss.watch;
        let apply_on = ss.apply;
        let x11_on = ss.x11;

        let (relay_s, watch_s, apply_s, x11_s) = match lang {
            Lang::ZhCn => (
                if relay_on { "开" } else { "关" },
                if watch_on { "开" } else { "关" },
                if apply_on { "开" } else { "关" },
                if x11_on { "开" } else { "关" },
            ),
            Lang::En => (
                if relay_on { "on" } else { "off" },
                if watch_on { "on" } else { "off" },
                if apply_on { "on" } else { "off" },
                if x11_on { "on" } else { "off" },
            ),
        };

        let desc = match lang {
            Lang::ZhCn => format!(
                "{}：\nrelay: {}\nwl-watch: {}\nwl-apply: {}\nx11-sync: {}\n\n{}\n",
                t(lang, K::TooltipStatusLine),
                relay_s,
                watch_s,
                apply_s,
                x11_s,
                t(lang, K::TooltipHint)
            ),
            Lang::En => format!(
                "{}:\nrelay: {}\nwl-watch: {}\nwl-apply: {}\nx11-sync: {}\n\n{}\n",
                t(lang, K::TooltipStatusLine),
                relay_s,
                watch_s,
                apply_s,
                x11_s,
                t(lang, K::TooltipHint)
            ),
        };

        ToolTip {
            icon_name: self.icon_name(),
            title: t(lang, K::TooltipTitle).to_string(),
            description: desc,
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::MenuItem;

        let (lang, relay_running, watch_running, apply_running, x11_running, has_systemd) = {
            let st = self.state.lock().unwrap();
            let ss = st.service_status();
            (
                st.lang(),
                ss.relay,
                ss.watch,
                ss.apply,
                ss.x11,
                st.systemd,
            )
        };

        let any_running = relay_running || watch_running || apply_running || x11_running;
        let all_running = relay_running && watch_running && apply_running && (!has_systemd || x11_running);

        vec![
            MenuItem::Standard(StandardItem {
                label: t(lang, K::OpenControlPanel).into(),
                activate: Box::new(|this: &mut Self| this.open_control_panel()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::ReloadConfig).into(),
                activate: Box::new(|this: &mut Self| this.reload_config()),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StartAll).into(),
                enabled: !all_running,
                activate: Box::new(|this: &mut Self| this.start_all()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StopAll).into(),
                enabled: any_running,
                activate: Box::new(|this: &mut Self| this.stop_all()),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StartRelay).into(),
                enabled: !relay_running,
                activate: Box::new(|this: &mut Self| this.start_relay()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StopRelay).into(),
                enabled: relay_running,
                activate: Box::new(|this: &mut Self| this.stop_relay()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StartWatch).into(),
                enabled: !watch_running,
                activate: Box::new(|this: &mut Self| this.start_watch()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StopWatch).into(),
                enabled: watch_running,
                activate: Box::new(|this: &mut Self| this.stop_watch()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StartApply).into(),
                enabled: !apply_running,
                activate: Box::new(|this: &mut Self| this.start_apply()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StopApply).into(),
                enabled: apply_running,
                activate: Box::new(|this: &mut Self| this.stop_apply()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StartX11Sync).into(),
                enabled: has_systemd && !x11_running,
                activate: Box::new(|this: &mut Self| this.start_x11_sync()),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: t(lang, K::StopX11Sync).into(),
                enabled: has_systemd && x11_running,
                activate: Box::new(|this: &mut Self| this.stop_x11_sync()),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: t(lang, K::Quit).into(),
                activate: Box::new(|this: &mut Self| this.quit_and_cleanup()),
                ..Default::default()
            }),
        ]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.open_control_panel();
    }
}

pub fn spawn_refresh_thread(handle: Handle<MultiClipRelayTray>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(600));
        let _ = handle.update(|tray| {
            tray.prune_exited();
        });
    });
}
