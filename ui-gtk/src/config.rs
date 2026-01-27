use anyhow::Context;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UiConfig {
    pub relay_addr: String,
    pub room: String,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: usize,
    #[serde(default = "default_image_mode")]
    pub image_mode: String,

    #[serde(default = "default_x11_poll_interval_ms")]
    pub x11_poll_interval_ms: u64,

    #[serde(default = "default_language")]
    pub language: String,

    /// History table column visibility map.
    /// Key = column id (e.g. "peer"), value = visible.
    /// Empty map means "use built-in defaults".
    #[serde(default)]
    pub history_columns: BTreeMap<String, bool>,

    // Legacy field (v0): existed as `force_png = true/false`.
    // Keep it for backward-compatible loading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_png: Option<bool>,
}

fn default_image_mode() -> String {
    "force-png".to_string()
}

fn default_language() -> String {
    // auto: follow system LANG.
    "auto".to_string()
}

fn default_max_file_bytes() -> usize {
    20 * 1024 * 1024
}

fn default_x11_poll_interval_ms() -> u64 {
    200
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            relay_addr: "127.0.0.1:8080".to_string(),
            room: "default".to_string(),
            max_text_bytes: 1 * 1024 * 1024,
            max_image_bytes: 20 * 1024 * 1024,
            max_file_bytes: default_max_file_bytes(),
            image_mode: default_image_mode(),
            x11_poll_interval_ms: default_x11_poll_interval_ms(),
            language: default_language(),
            history_columns: BTreeMap::new(),
            force_png: None,
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config"));
    base.join("multicliprelay").join("ui.toml")
}

pub fn load_config(path: &Path) -> anyhow::Result<UiConfig> {
    let s = fs::read_to_string(path).context("read config")?;
    let mut cfg: UiConfig = toml::from_str(&s).context("parse config")?;

    // Backward compat: old configs had `force_png = true/false`.
    if let Some(force_png) = cfg.force_png.take() {
        cfg.image_mode = if force_png { "force-png" } else { "multi" }.to_string();
    }

    Ok(cfg)
}

pub fn save_config(path: &Path, cfg: &UiConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("mkdir config")?;
    }
    let s = toml::to_string_pretty(cfg).context("serialize config")?;
    fs::write(path, s).context("write config")?;
    Ok(())
}
