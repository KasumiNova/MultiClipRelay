use std::path::PathBuf;

use crate::consts::{APP_DIR_NAME, TAR_MIME};

pub fn default_state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        let base = PathBuf::from(d);
        return base.join(APP_DIR_NAME);
    }
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/tmp/{}-{}", APP_DIR_NAME, uid))
}

pub fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        let base = PathBuf::from(d);
        return base.join(APP_DIR_NAME);
    }
    if let Ok(home) = std::env::var("HOME") {
        let base = PathBuf::from(home).join(".local/share");
        return base.join(APP_DIR_NAME);
    }
    // Last resort: state_dir should exist; fall back to /tmp.
    let base = PathBuf::from("/tmp");
    base.join(APP_DIR_NAME)
}

pub fn received_dir() -> PathBuf {
    default_data_dir().join("received")
}

pub fn history_path() -> PathBuf {
    default_data_dir().join("history.jsonl")
}

pub fn safe_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

pub fn is_tar_payload(name: &str, mime: Option<&str>) -> bool {
    mime == Some(TAR_MIME) || name.to_ascii_lowercase().ends_with(".tar")
}

pub fn first_8(s: &str) -> &str {
    if s.len() >= 8 { &s[..8] } else { s }
}
