use anyhow::Context;

use std::path::PathBuf;

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

    #[serde(default = "default_language")]
    pub language: String,

    // Legacy field in early ui versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_png: Option<bool>,
}

fn default_image_mode() -> String {
    "force-png".to_string()
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_max_file_bytes() -> usize {
    20 * 1024 * 1024
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
            language: default_language(),
            force_png: None,
        }
    }
}

impl UiConfig {
    pub fn relay_bind_hint(&self) -> Option<String> {
        // If relay_addr looks like a loopback address, we can bind relay to it.
        // Otherwise (e.g. a remote relay), binding locally doesn't make sense.
        let s = self.relay_addr.trim();
        if s.starts_with("127.")
            || s.starts_with("0.0.0.0")
            || s.starts_with("[::1]")
            || s.starts_with("localhost")
        {
            Some(s.to_string())
        } else {
            None
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".config"));
    base.join("multicliprelay").join("ui.toml")
}

pub fn load_config() -> anyhow::Result<UiConfig> {
    let path = config_path();
    let s = std::fs::read_to_string(&path).with_context(|| format!("read config {}", path.display()))?;
    let mut cfg: UiConfig = toml::from_str(&s).context("parse config")?;

    // Backward compat: old configs had `force_png = true/false`.
    if let Some(force_png) = cfg.force_png.take() {
        cfg.image_mode = if force_png { "force-png" } else { "multi" }.to_string();
    }

    Ok(cfg)
}
