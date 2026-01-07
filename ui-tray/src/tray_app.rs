use crate::config::{load_config, UiConfig};
use crate::i18n::{detect_lang_from_env, parse_lang_id, t, Lang, K};
use crate::procs::{find_sibling_binary, spawn_ui_gtk, terminate_child, Procs};

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
    }

    fn stop_all(&self) {
        self.stop_watch();
        self.stop_apply();
        self.stop_relay();
    }

    fn start_relay(&self) {
        let mut st = self.state.lock().unwrap();
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
        let child = {
            let mut st = self.state.lock().unwrap();
            st.procs.relay.take()
        };
        if let Some(child) = child {
            terminate_child(child, "relay");
        }
    }

    fn start_watch(&self) {
        let (cfg, already) = {
            let st = self.state.lock().unwrap();
            (st.cfg.clone(), st.procs.watch.is_some())
        };
        if already {
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
        let child = {
            let mut st = self.state.lock().unwrap();
            st.procs.watch.take()
        };
        if let Some(child) = child {
            terminate_child(child, "node wl-watch");
        }
    }

    fn start_apply(&self) {
        let (cfg, already) = {
            let st = self.state.lock().unwrap();
            (st.cfg.clone(), st.procs.apply.is_some())
        };
        if already {
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
        let child = {
            let mut st = self.state.lock().unwrap();
            st.procs.apply.take()
        };
        if let Some(child) = child {
            terminate_child(child, "node wl-apply");
        }
    }

    fn quit_and_cleanup(&self) {
        self.stop_all();
        std::process::exit(0);
    }

    fn prune_exited(&self) {
        let mut st = self.state.lock().unwrap();
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
}

impl AppState {
    fn new(cfg: UiConfig) -> Self {
        Self {
            cfg,
            procs: Procs::default(),
            opened_count: 0,
        }
    }

    fn lang(&self) -> Lang {
        if self.cfg.language == "auto" {
            detect_lang_from_env()
        } else {
            parse_lang_id(&self.cfg.language).unwrap_or_else(detect_lang_from_env)
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
        let relay_on = st.procs.relay.is_some();
        let watch_on = st.procs.watch.is_some();
        let apply_on = st.procs.apply.is_some();

        let (relay_s, watch_s, apply_s) = match lang {
            Lang::ZhCn => (
                if relay_on { "开" } else { "关" },
                if watch_on { "开" } else { "关" },
                if apply_on { "开" } else { "关" },
            ),
            Lang::En => (
                if relay_on { "on" } else { "off" },
                if watch_on { "on" } else { "off" },
                if apply_on { "on" } else { "off" },
            ),
        };

        let desc = match lang {
            Lang::ZhCn => format!(
                "{}：\nrelay: {}\nwl-watch: {}\nwl-apply: {}\n\n{}\n",
                t(lang, K::TooltipStatusLine),
                relay_s,
                watch_s,
                apply_s,
                t(lang, K::TooltipHint)
            ),
            Lang::En => format!(
                "{}:\nrelay: {}\nwl-watch: {}\nwl-apply: {}\n\n{}\n",
                t(lang, K::TooltipStatusLine),
                relay_s,
                watch_s,
                apply_s,
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

        let (lang, relay_running, watch_running, apply_running) = {
            let st = self.state.lock().unwrap();
            (
                st.lang(),
                st.procs.relay.is_some(),
                st.procs.watch.is_some(),
                st.procs.apply.is_some(),
            )
        };

        let any_running = relay_running || watch_running || apply_running;
        let all_running = relay_running && watch_running && apply_running;

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
