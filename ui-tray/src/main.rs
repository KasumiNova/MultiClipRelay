mod config;
mod i18n;
mod procs;
mod tray_app;

use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    // Single-instance guard:
    // - tray has no main window, so launching it twice is almost always a mistake.
    // - if already running, we "redirect" the user action to opening the control panel.
    let _instance_lock = {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let lock_path = dir.join("multicliprelay-ui-tray.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)?;

        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            // Already running.
            let _ = crate::procs::spawn_ui_gtk();
            return Ok(());
        }

        file
    };

    // Minimal StatusNotifierItem (SNI) tray.
    // Notes:
    // - Works best on KDE / bars that support SNI (e.g. waybar's tray module).
    // - GNOME may require an extension to show AppIndicators.

    let cfg = crate::config::load_config().unwrap_or_default();
    let tray = crate::tray_app::MultiClipRelayTray::new(cfg);
    let service = ksni::TrayService::new(tray);

    // Periodic refresh:
    // - prune exited child processes
    // - update tooltip/status
    let handle = service.handle();
    crate::tray_app::spawn_refresh_thread(handle.clone());

    // Blocks forever until the tray is closed (or the process is killed).
    if let Err(e) = service.run() {
        // If the host doesn't support SNI / DBus isn't available, we surface it.
        eprintln!("tray service exited with error: {e:?}");
    }

    Ok(())
}
