use anyhow::Context;

use std::path::{Path, PathBuf};

pub fn startup_dir() -> Option<PathBuf> {
    // Per-user Startup folder.
    // Typically: %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("Startup"),
    )
}

pub fn autostart_script_path() -> Option<PathBuf> {
    startup_dir().map(|d| d.join("multicliprelay-ui-tray.cmd"))
}

pub fn is_enabled() -> bool {
    autostart_script_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn enable(ui_exe: &Path) -> anyhow::Result<()> {
    let Some(p) = autostart_script_path() else {
        anyhow::bail!("APPDATA not set; cannot locate Startup folder");
    };
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).context("mkdir startup dir")?;
    }

    // Use absolute path to the UI executable.
    let exe = ui_exe
        .canonicalize()
        .unwrap_or_else(|_| ui_exe.to_path_buf());

    // /min avoids stealing focus too aggressively.
    let content = format!(
        "@echo off\r\nstart \"\" /min \"{}\"\r\n",
        exe.display()
    );

    std::fs::write(&p, content).with_context(|| format!("write {}", p.display()))?;
    Ok(())
}

pub fn disable() -> anyhow::Result<()> {
    let Some(p) = autostart_script_path() else {
        return Ok(());
    };
    if p.exists() {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}
