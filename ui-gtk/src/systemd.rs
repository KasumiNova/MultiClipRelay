use anyhow::Context;
use std::path::PathBuf;
use std::process::Command;

use crate::config::UiConfig;

pub const UNIT_RELAY: &str = "multicliprelay-relay.service";
pub const UNIT_WL_WATCH: &str = "multicliprelay-wl-watch.service";
pub const UNIT_WL_APPLY: &str = "multicliprelay-wl-apply.service";
pub const UNIT_X11_SYNC: &str = "multicliprelay-x11-sync.service";

#[derive(Debug, Copy, Clone, Default)]
pub struct ServiceStatus {
    pub relay: bool,
    pub watch: bool,
    pub apply: bool,
    pub x11: bool,
}

pub fn enabled_from_env_or_auto() -> bool {
    match std::env::var("MULTICLIPRELAY_USE_SYSTEMD").ok().as_deref() {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    available()
}

pub fn available() -> bool {
    // Require working user bus.
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", unit])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn status_snapshot() -> ServiceStatus {
    ServiceStatus {
        relay: is_active(UNIT_RELAY),
        watch: is_active(UNIT_WL_WATCH),
        apply: is_active(UNIT_WL_APPLY),
        x11: is_active(UNIT_X11_SYNC),
    }
}

pub fn start(unit: &str) -> anyhow::Result<()> {
    Command::new("systemctl")
        .args(["--user", "start", unit])
        .status()
        .with_context(|| format!("systemctl start {unit}"))?
        .success()
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("systemctl start failed: {unit}"))
}

pub fn stop(unit: &str) -> anyhow::Result<()> {
    Command::new("systemctl")
        .args(["--user", "stop", unit])
        .status()
        .with_context(|| format!("systemctl stop {unit}"))?
        .success()
        .then_some(())
        .ok_or_else(|| anyhow::anyhow!("systemctl stop failed: {unit}"))
}

pub fn node_supports_x11_sync() -> bool {
    // In packaged installs, `node` is renamed to `multicliprelay-node`.
    // If the user has an older binary in PATH, starting the unit will fail with
    // "unrecognized subcommand 'x11-sync'" and systemd will restart-loop.
    Command::new("multicliprelay-node")
        .args(["x11-sync", "--help"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn env_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config"));
    base.join("multicliprelay").join("multicliprelay.env")
}

pub fn write_env_from_ui_config(cfg: &UiConfig) -> anyhow::Result<()> {
    let path = env_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("mkdir env dir")?;
    }

    let mut lines: Vec<String> = Vec::new();

    let relay = cfg.relay_addr.trim();
    if !relay.is_empty() {
        lines.push(format!("MULTICLIPRELAY_RELAY={relay}"));
    }

    // Bind hint for local relay.
    // If we can't infer one, do NOT write an empty MULTICLIPRELAY_BIND (would override unit default).
    if relay.starts_with("127.")
        || relay.starts_with("0.0.0.0")
        || relay.starts_with("[::1]")
        || relay.starts_with("localhost")
    {
        lines.push(format!("MULTICLIPRELAY_BIND={relay}"));
    }

    lines.push(format!("MULTICLIPRELAY_ROOM={}", cfg.room.trim()));
    lines.push(format!("MULTICLIPRELAY_MAX_TEXT_BYTES={}", cfg.max_text_bytes));
    lines.push(format!("MULTICLIPRELAY_MAX_IMAGE_BYTES={}", cfg.max_image_bytes));
    lines.push(format!("MULTICLIPRELAY_MAX_FILE_BYTES={}", cfg.max_file_bytes));
    lines.push(format!("MULTICLIPRELAY_IMAGE_MODE={}", cfg.image_mode.trim()));

    lines.push("MULTICLIPRELAY_WATCH_MODE=watch".to_string());
    lines.push("MULTICLIPRELAY_POLL_INTERVAL_MS=200".to_string());
    lines.push(format!(
        "MULTICLIPRELAY_X11_POLL_INTERVAL_MS={}",
        cfg.x11_poll_interval_ms
    ));

    std::fs::write(&path, lines.join("\n") + "\n").context("write env")?;
    Ok(())
}
