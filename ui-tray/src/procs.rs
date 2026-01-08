use std::path::PathBuf;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

#[derive(Default)]
pub struct Procs {
    pub relay: Option<Child>,
    pub watch: Option<Child>,
    pub apply: Option<Child>,
}

pub fn terminate_child(mut child: Child, label: &'static str) {
    // Best-effort graceful shutdown.
    thread::spawn(move || {
        let pid = child.id() as i32;
        if pid > 0 {
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }

        let deadline = std::time::Instant::now() + Duration::from_millis(900);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(30));
                }
                Err(e) => {
                    eprintln!("{label} wait failed: {e:?}");
                    return;
                }
            }
        }

        let _ = child.kill();
        let _ = child.wait();
    });
}

pub fn find_sibling_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join(name);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

pub fn spawn_ui_gtk() -> anyhow::Result<()> {
    // Prefer a sibling binary (when running from target/debug), fallback to PATH.
    let ui = find_sibling_binary("multicliprelay-ui-gtk")
        .or_else(|| find_sibling_binary("ui-gtk"))
        .or_else(|| which::which("multicliprelay-ui-gtk").ok())
        .or_else(|| which::which("ui-gtk").ok())
        .unwrap_or_else(|| PathBuf::from("multicliprelay-ui-gtk"));

    Command::new(ui)
        .env("MULTICLIPRELAY_USE_SYSTEMD", "1")
        .spawn()?;
    Ok(())
}
