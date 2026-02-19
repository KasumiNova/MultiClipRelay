use anyhow::Context;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

#[derive(Default)]
pub struct Procs {
    pub relay: Option<Child>,
    pub watch: Option<Child>,
    pub apply: Option<Child>,
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

fn is_dev_exe_location() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let Some(s) = exe.to_str() else {
        return false;
    };
    s.contains("/target/debug/")
        || s.contains("/target/release/")
        || s.contains("\\target\\debug\\")
        || s.contains("\\target\\release\\")
}

fn resolve_binary(primary: &str, fallbacks: &[&str]) -> PathBuf {
    let mut names: Vec<&str> = Vec::with_capacity(1 + fallbacks.len());
    names.push(primary);
    names.extend_from_slice(fallbacks);

    let prefer_sibling = is_dev_exe_location();

    if prefer_sibling {
        for n in names.iter() {
            if let Some(p) = find_sibling_binary(n) {
                return p;
            }
        }
    }

    for n in names.iter() {
        if let Some(p) = find_sibling_binary(n) {
            return p;
        }
        if let Ok(p) = which::which(n) {
            return p;
        }
    }

    PathBuf::from(primary)
}

pub fn ui_egui_exe() -> PathBuf {
    resolve_binary("multicliprelay-ui-egui", &["ui-egui", "multicliprelay-ui-egui.exe", "ui-egui.exe"])
}

pub fn spawn_ui_egui() -> anyhow::Result<()> {
    let ui = ui_egui_exe();
    Command::new(ui).spawn().context("spawn ui-egui")?;
    Ok(())
}

pub fn spawn_relay(bind_addr: Option<&str>) -> anyhow::Result<Child> {
    let relay_bin = resolve_binary("multicliprelay-relay", &["relay", "multicliprelay-relay.exe", "relay.exe"]);
    let mut cmd = Command::new(relay_bin);
    if let Some(bind) = bind_addr {
        let bind = bind.trim();
        if !bind.is_empty() {
            cmd.args(["--bind", bind]);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().context("spawn relay")
}

pub fn spawn_node(args: &[&str]) -> anyhow::Result<Child> {
    let node_bin = resolve_binary("multicliprelay-node", &["node", "multicliprelay-node.exe", "node.exe"]);
    let mut cmd = Command::new(node_bin);
    cmd.args(args);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().with_context(|| format!("spawn node args={:?}", args))
}

pub fn terminate_best_effort(child: &mut Option<Child>) {
    let Some(mut c) = child.take() else { return; };
    let _ = c.kill();
    let _ = c.wait();
}

pub fn prune_exited(child: &mut Option<Child>) {
    let Some(c) = child.as_mut() else { return; };
    if let Ok(Some(_)) = c.try_wait() {
        *child = None;
    }
}
