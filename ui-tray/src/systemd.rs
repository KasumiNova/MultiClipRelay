use anyhow::Context;
use std::path::Path;
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
    let _ = repair_unit_execstart_if_missing(unit);
    let _ = Command::new("systemctl")
        .args(["--user", "reset-failed", unit])
        .status();
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

fn unit_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config"));
    base.join("systemd").join("user")
}

fn unit_file_path(unit: &str) -> PathBuf {
    unit_dir().join(unit)
}

fn prefer_installed_bin(names: &[&str]) -> Option<PathBuf> {
    for n in names {
        let p = Path::new("/usr/bin").join(n);
        if p.exists() {
            return Some(p);
        }
        let p = Path::new("/usr/local/bin").join(n);
        if p.exists() {
            return Some(p);
        }
    }
    for n in names {
        if let Ok(p) = which::which(n) {
            return Some(p);
        }
    }
    None
}

fn maybe_rewrite_execstart_line(
    line: &str,
    new_bin: &Path,
    expected_basenames: &[&str],
) -> Option<String> {
    let rest = line.strip_prefix("ExecStart=")?;
    let trimmed = rest.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }
    let cut = trimmed
        .find(|c: char| c.is_whitespace())
        .unwrap_or(trimmed.len());
    let old_bin = &trimmed[..cut];
    if Path::new(old_bin).exists() {
        return None;
    }
    if let Some(base) = Path::new(old_bin).file_name().and_then(|s| s.to_str()) {
        if !expected_basenames.iter().any(|b| b == &base) {
            return None;
        }
    } else {
        return None;
    }

    let suffix = &trimmed[cut..];
    Some(format!("ExecStart={}{}", new_bin.display(), suffix))
}

fn repair_unit_execstart_if_missing(unit: &str) -> anyhow::Result<()> {
    let path = unit_file_path(unit);
    if !path.exists() {
        return Ok(());
    }

    let (new_bin, expected): (Option<PathBuf>, Vec<&str>) = if unit == UNIT_RELAY {
        (
            prefer_installed_bin(&["multicliprelay-relay", "relay"]),
            vec!["multicliprelay-relay", "relay"],
        )
    } else {
        (
            prefer_installed_bin(&["multicliprelay-node", "node"]),
            vec!["multicliprelay-node", "node"],
        )
    };

    let Some(new_bin) = new_bin else {
        return Ok(());
    };

    let s = std::fs::read_to_string(&path)
        .with_context(|| format!("read unit file: {}", path.display()))?;
    let mut changed = false;
    let mut out: Vec<String> = Vec::new();
    for line in s.lines() {
        if let Some(new_line) = maybe_rewrite_execstart_line(line, &new_bin, &expected) {
            out.push(new_line);
            changed = true;
        } else {
            out.push(line.to_string());
        }
    }

    if changed {
        std::fs::create_dir_all(unit_dir()).ok();
        std::fs::write(&path, out.join("\n") + "\n")
            .with_context(|| format!("write unit file: {}", path.display()))?;
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }

    Ok(())
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
