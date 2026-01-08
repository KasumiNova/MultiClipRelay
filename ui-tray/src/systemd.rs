use anyhow::Context;
use std::path::PathBuf;
use std::process::Command;

use crate::config::UiConfig;

pub const UNIT_RELAY: &str = "multicliprelay-relay.service";
pub const UNIT_WL_WATCH: &str = "multicliprelay-wl-watch.service";
pub const UNIT_WL_APPLY: &str = "multicliprelay-wl-apply.service";
pub const UNIT_X11_SYNC: &str = "multicliprelay-x11-sync.service";

pub fn available() -> bool {
    if std::env::var_os("MULTICLIPRELAY_USE_SYSTEMD").as_deref() == Some("0".as_ref()) {
        return false;
    }
    // Hard require a working user bus; otherwise systemctl will fail and block UX.
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

    // Keep it minimal and compatible with systemd EnvironmentFile.
    // NOTE: don't write empty values for required vars.
    let mut lines: Vec<String> = Vec::new();

    // Relay address used by node (connect target)
    let relay = cfg.relay_addr.trim();
    if !relay.is_empty() {
        lines.push(format!("MULTICLIPRELAY_RELAY={relay}"));
    }

    // Suggest a bind target if relay_addr looks local.
    if let Some(bind) = cfg.relay_bind_hint() {
        let bind = bind.trim();
        if !bind.is_empty() {
            lines.push(format!("MULTICLIPRELAY_BIND={bind}"));
        }
    }

    lines.push(format!("MULTICLIPRELAY_ROOM={}", cfg.room.trim()));
    lines.push(format!("MULTICLIPRELAY_MAX_TEXT_BYTES={}", cfg.max_text_bytes));
    lines.push(format!("MULTICLIPRELAY_MAX_IMAGE_BYTES={}", cfg.max_image_bytes));
    lines.push(format!("MULTICLIPRELAY_MAX_FILE_BYTES={}", cfg.max_file_bytes));
    lines.push(format!("MULTICLIPRELAY_IMAGE_MODE={}", cfg.image_mode.trim()));

    // Watch mode/interval defaults.
    lines.push("MULTICLIPRELAY_WATCH_MODE=watch".to_string());
    lines.push("MULTICLIPRELAY_POLL_INTERVAL_MS=200".to_string());

    // X11 sync default.
    lines.push(format!(
        "MULTICLIPRELAY_X11_POLL_INTERVAL_MS={}",
        cfg.x11_poll_interval_ms
    ));

    std::fs::write(&path, lines.join("\n") + "\n").context("write env")?;
    Ok(())
}
