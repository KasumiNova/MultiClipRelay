mod config;
mod procs;

use crate::config::{config_path, load_config, save_config, UiConfig};
use crate::procs::{prune_exited, spawn_node, spawn_relay, terminate_child, Procs};

use eframe::egui;

use std::collections::VecDeque;
use std::sync::mpsc;

const LOG_CAP: usize = 800;

struct UiApp {
    cfg_path: std::path::PathBuf,
    cfg: UiConfig,

    procs: Procs,

    // logs are fed by background threads reading child stdout/stderr.
    log_rx: mpsc::Receiver<String>,
    log_tx: mpsc::Sender<String>,
    logs: VecDeque<String>,

    // UI inputs (editable copies)
    relay_addr: String,
    room: String,
    max_text_bytes: String,
    max_image_bytes: String,
    max_file_bytes: String,
    image_mode: String,
}

impl UiApp {
    fn new() -> Self {
        let cfg_path = config_path();
        let cfg = load_config(&cfg_path).unwrap_or_default();

        let (tx, rx) = mpsc::channel::<String>();

        let mut app = Self {
            cfg_path,
            cfg: cfg.clone(),
            procs: Procs::default(),
            log_rx: rx,
            log_tx: tx,
            logs: VecDeque::new(),
            relay_addr: cfg.relay_addr.clone(),
            room: cfg.room.clone(),
            max_text_bytes: cfg.max_text_bytes.to_string(),
            max_image_bytes: cfg.max_image_bytes.to_string(),
            max_file_bytes: cfg.max_file_bytes.to_string(),
            image_mode: cfg.image_mode.clone(),
        };

        app.log(format!(
            "config: {} (loaded={})",
            app.cfg_path.display(),
            app.cfg_path.exists()
        ));

        app
    }

    fn log(&mut self, s: impl Into<String>) {
        self.logs.push_back(s.into());
        while self.logs.len() > LOG_CAP {
            self.logs.pop_front();
        }
    }

    fn drain_logs(&mut self) {
        while let Ok(line) = self.log_rx.try_recv() {
            self.log(line);
        }
    }

    fn refresh_procs(&mut self) {
        prune_exited(&mut self.procs.relay, "relay", &self.log_tx);
        prune_exited(&mut self.procs.watch, "watch", &self.log_tx);
        prune_exited(&mut self.procs.apply, "apply", &self.log_tx);
    }

    fn apply_inputs_to_cfg(&mut self) {
        self.cfg.relay_addr = self.relay_addr.trim().to_string();
        self.cfg.room = self.room.trim().to_string();
        self.cfg.image_mode = self.image_mode.trim().to_string();

        self.cfg.max_text_bytes = self.max_text_bytes.trim().parse().unwrap_or(self.cfg.max_text_bytes);
        self.cfg.max_image_bytes = self.max_image_bytes.trim().parse().unwrap_or(self.cfg.max_image_bytes);
        self.cfg.max_file_bytes = self.max_file_bytes.trim().parse().unwrap_or(self.cfg.max_file_bytes);
    }

    fn reload_cfg(&mut self) {
        match load_config(&self.cfg_path) {
            Ok(cfg) => {
                self.cfg = cfg.clone();
                self.relay_addr = cfg.relay_addr;
                self.room = cfg.room;
                self.max_text_bytes = cfg.max_text_bytes.to_string();
                self.max_image_bytes = cfg.max_image_bytes.to_string();
                self.max_file_bytes = cfg.max_file_bytes.to_string();
                self.image_mode = cfg.image_mode;
                self.log("reloaded config".to_string());
            }
            Err(e) => self.log(format!("reload config failed: {e:#}")),
        }
    }

    fn save_cfg(&mut self) {
        self.apply_inputs_to_cfg();
        match save_config(&self.cfg_path, &self.cfg) {
            Ok(()) => self.log("saved config".to_string()),
            Err(e) => self.log(format!("save config failed: {e:#}")),
        }
    }

    fn start_relay(&mut self) {
        if self.procs.relay.is_some() {
            return;
        }
        self.apply_inputs_to_cfg();
        let bind = self.cfg.relay_bind_hint();
        match spawn_relay(&self.log_tx, bind.as_deref()) {
            Ok(child) => self.procs.relay = Some(child),
            Err(e) => self.log(format!("start relay failed: {e:#}")),
        }
    }

    fn stop_relay(&mut self) {
        if let Some(child) = self.procs.relay.take() {
            terminate_child(child, "relay", self.log_tx.clone());
        }
    }

    fn start_watch(&mut self) {
        if self.procs.watch.is_some() {
            return;
        }
        self.apply_inputs_to_cfg();

        let mut args: Vec<String> = Vec::new();

        #[cfg(windows)]
        {
            args.extend([
                "win-watch".to_string(),
                "--room".to_string(),
                self.cfg.room.clone(),
                "--relay".to_string(),
                self.cfg.relay_addr.clone(),
                "--max-text-bytes".to_string(),
                self.cfg.max_text_bytes.to_string(),
                "--max-image-bytes".to_string(),
                self.cfg.max_image_bytes.to_string(),
                "--max-file-bytes".to_string(),
                self.cfg.max_file_bytes.to_string(),
            ]);
        }

        #[cfg(unix)]
        {
            args.extend([
                "wl-watch".to_string(),
                "--room".to_string(),
                self.cfg.room.clone(),
                "--relay".to_string(),
                self.cfg.relay_addr.clone(),
                "--mode".to_string(),
                "watch".to_string(),
                "--max-text-bytes".to_string(),
                self.cfg.max_text_bytes.to_string(),
                "--max-image-bytes".to_string(),
                self.cfg.max_image_bytes.to_string(),
                "--max-file-bytes".to_string(),
                self.cfg.max_file_bytes.to_string(),
                "--image-mode".to_string(),
                self.cfg.image_mode.clone(),
            ]);
        }

        #[cfg(not(any(windows, unix)))]
        {
            self.log("watch not supported on this platform".to_string());
            return;
        }

        match spawn_node(&self.log_tx, &args) {
            Ok(child) => self.procs.watch = Some(child),
            Err(e) => self.log(format!("start watch failed: {e:#}")),
        }
    }

    fn stop_watch(&mut self) {
        if let Some(child) = self.procs.watch.take() {
            terminate_child(child, "watch", self.log_tx.clone());
        }
    }

    fn start_apply(&mut self) {
        if self.procs.apply.is_some() {
            return;
        }
        self.apply_inputs_to_cfg();

        let mut args: Vec<String> = Vec::new();

        #[cfg(windows)]
        {
            args.extend([
                "win-apply".to_string(),
                "--room".to_string(),
                self.cfg.room.clone(),
                "--relay".to_string(),
                self.cfg.relay_addr.clone(),
            ]);
        }

        #[cfg(unix)]
        {
            args.extend([
                "wl-apply".to_string(),
                "--room".to_string(),
                self.cfg.room.clone(),
                "--relay".to_string(),
                self.cfg.relay_addr.clone(),
                "--image-mode".to_string(),
                self.cfg.image_mode.clone(),
            ]);
        }

        #[cfg(not(any(windows, unix)))]
        {
            self.log("apply not supported on this platform".to_string());
            return;
        }

        match spawn_node(&self.log_tx, &args) {
            Ok(child) => self.procs.apply = Some(child),
            Err(e) => self.log(format!("start apply failed: {e:#}")),
        }
    }

    fn stop_apply(&mut self) {
        if let Some(child) = self.procs.apply.take() {
            terminate_child(child, "apply", self.log_tx.clone());
        }
    }

    fn start_all(&mut self) {
        self.start_relay();
        self.start_apply();
        self.start_watch();
    }

    fn stop_all(&mut self) {
        self.stop_watch();
        self.stop_apply();
        self.stop_relay();
    }
}

impl eframe::App for UiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_logs();
        self.refresh_procs();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("MultiClipRelay 控制面板 (egui)");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Reload").clicked() {
                        self.reload_cfg();
                    }
                    if ui.button("Save").clicked() {
                        self.save_cfg();
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.group(|ui| {
                ui.label("配置");
                ui.horizontal(|ui| {
                    ui.label("Relay:");
                    ui.text_edit_singleline(&mut self.relay_addr);
                });
                ui.horizontal(|ui| {
                    ui.label("Room:");
                    ui.text_edit_singleline(&mut self.room);
                });
                ui.horizontal(|ui| {
                    ui.label("Max text bytes:");
                    ui.text_edit_singleline(&mut self.max_text_bytes);
                });
                ui.horizontal(|ui| {
                    ui.label("Max image bytes:");
                    ui.text_edit_singleline(&mut self.max_image_bytes);
                });
                ui.horizontal(|ui| {
                    ui.label("Max file bytes:");
                    ui.text_edit_singleline(&mut self.max_file_bytes);
                });

                #[cfg(unix)]
                {
                    ui.horizontal(|ui| {
                        ui.label("Image mode:");
                        egui::ComboBox::from_id_salt("image_mode")
                            .selected_text(self.image_mode.clone())
                            .show_ui(ui, |ui| {
                                for m in ["force-png", "multi", "passthrough", "spoof-png"] {
                                    ui.selectable_value(&mut self.image_mode, m.to_string(), m);
                                }
                            });
                    });
                }

                #[cfg(windows)]
                {
                    ui.horizontal(|ui| {
                        ui.label("Image mode:");
                        ui.label("(Windows: always DIBV5 on apply / PNG on send)");
                    });
                }
            });

            ui.add_space(8.0);

            ui.group(|ui| {
                ui.label("服务");
                ui.horizontal_wrapped(|ui| {
                    let relay_on = self.procs.relay.is_some();
                    let watch_on = self.procs.watch.is_some();
                    let apply_on = self.procs.apply.is_some();

                    ui.label(format!("relay: {}", if relay_on { "Running" } else { "Stopped" }));
                    ui.separator();
                    ui.label(format!("watch: {}", if watch_on { "Running" } else { "Stopped" }));
                    ui.separator();
                    ui.label(format!("apply: {}", if apply_on { "Running" } else { "Stopped" }));
                });

                ui.horizontal(|ui| {
                    if ui.button("Start all").clicked() {
                        self.start_all();
                    }
                    if ui.button("Stop all").clicked() {
                        self.stop_all();
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    if ui.button("Start relay").clicked() {
                        self.start_relay();
                    }
                    if ui.button("Stop relay").clicked() {
                        self.stop_relay();
                    }
                    if ui.button("Start watch").clicked() {
                        self.start_watch();
                    }
                    if ui.button("Stop watch").clicked() {
                        self.stop_watch();
                    }
                    if ui.button("Start apply").clicked() {
                        self.start_apply();
                    }
                    if ui.button("Stop apply").clicked() {
                        self.stop_apply();
                    }
                });
            });

            ui.add_space(8.0);

            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label("日志");
                    if ui.button("Clear").clicked() {
                        self.logs.clear();
                    }
                });

                let mut text = String::new();
                for l in self.logs.iter() {
                    text.push_str(l);
                    text.push('\n');
                }
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(360.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .desired_rows(18)
                                .code_editor()
                                .interactive(false),
                        );
                    });
            });
        });

        // keep UI responsive even when idle
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

fn main() -> anyhow::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 560.0])
            .with_title("MultiClipRelay (egui)"),
        ..Default::default()
    };

    eframe::run_native(
        "multicliprelay-ui-egui",
        native_options,
        Box::new(|_cc| Ok(Box::new(UiApp::new()))),
    )
    .map_err(|e| anyhow::anyhow!("eframe run_native failed: {e}"))?;

    Ok(())
}
