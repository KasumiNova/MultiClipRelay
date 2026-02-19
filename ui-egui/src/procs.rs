use anyhow::Context;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[derive(Default)]
pub struct Procs {
    pub relay: Option<Child>,
    pub watch: Option<Child>,
    pub apply: Option<Child>,
}

pub fn terminate_child(mut child: Child, label: &'static str, log_tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        // Best-effort graceful shutdown.
        #[cfg(unix)]
        {
            let pid = child.id() as i32;
            if pid > 0 {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }

            let deadline = std::time::Instant::now() + Duration::from_millis(800);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        let _ = log_tx.send(format!("{label} exited"));
                        return;
                    }
                    Ok(None) => {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        thread::sleep(Duration::from_millis(30));
                    }
                    Err(e) => {
                        let _ = log_tx.send(format!("{label} wait failed: {e:?}"));
                        return;
                    }
                }
            }
        }

        // Fallback: kill.
        let _ = child.kill();
        let _ = child.wait();
        let _ = log_tx.send(format!("{label} killed"));
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

fn is_dev_exe_location() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let Some(s) = exe.to_str() else {
        return false;
    };
    s.contains("/target/debug/") || s.contains("/target/release/") || s.contains("\\target\\debug\\") || s.contains("\\target\\release\\")
}

#[cfg(unix)]
fn add_if_exists(out: &mut Vec<PathBuf>, p: PathBuf) {
    if p.exists() {
        out.push(p);
    }
}

fn resolve_binary(primary: &str, fallbacks: &[&str]) -> PathBuf {
    let mut names: Vec<&str> = Vec::with_capacity(1 + fallbacks.len());
    names.push(primary);
    names.extend_from_slice(fallbacks);

    let prefer_sibling = is_dev_exe_location();
    let mut candidates: Vec<PathBuf> = Vec::new();

    if prefer_sibling {
        for n in names.iter() {
            if let Some(p) = find_sibling_binary(n) {
                return p;
            }
        }
    }

    for n in names.iter() {
        if let Some(p) = find_sibling_binary(n) {
            candidates.push(p);
        }
        if let Ok(p) = which::which(n) {
            candidates.push(p);
        }

        #[cfg(unix)]
        {
            use std::path::Path;
            add_if_exists(&mut candidates, Path::new("/usr/bin").join(n));
            add_if_exists(&mut candidates, Path::new("/usr/local/bin").join(n));
        }
    }

    let mut uniq: Vec<PathBuf> = Vec::new();
    for p in candidates.into_iter() {
        if !uniq.iter().any(|u| u == &p) {
            uniq.push(p);
        }
    }

    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    for p in uniq.into_iter() {
        match std::fs::metadata(&p).and_then(|m| m.modified()) {
            Ok(t) => match &best {
                None => best = Some((p, t)),
                Some((_, bt)) => {
                    if t > *bt {
                        best = Some((p, t));
                    }
                }
            },
            Err(_) => {
                if best.is_none() {
                    best = Some((p, std::time::SystemTime::UNIX_EPOCH));
                }
            }
        }
    }

    best.map(|(p, _)| p)
        .unwrap_or_else(|| PathBuf::from(primary))
}

pub fn spawn_relay(log_tx: &mpsc::Sender<String>, bind_addr: Option<&str>) -> anyhow::Result<Child> {
    let relay_bin = resolve_binary("multicliprelay-relay", &["relay"]);
    let _ = log_tx.send(format!("starting relay: {}", relay_bin.display()));
    let mut cmd = Command::new(relay_bin);

    if let Some(bind_addr) = bind_addr {
        let bind_addr = bind_addr.trim();
        if !bind_addr.is_empty() {
            cmd.args(["--bind", bind_addr]);
        }
    }

    spawn_with_logs(&mut cmd, log_tx, "relay")
}

pub fn spawn_node(log_tx: &mpsc::Sender<String>, args: &[String]) -> anyhow::Result<Child> {
    let node_bin = resolve_binary("multicliprelay-node", &["node"]);
    let _ = log_tx.send(format!("starting node: {}", node_bin.display()));
    let mut cmd = Command::new(node_bin);
    cmd.args(args);
    spawn_with_logs(&mut cmd, log_tx, "node")
}

fn spawn_with_logs(cmd: &mut Command, log_tx: &mpsc::Sender<String>, tag: &str) -> anyhow::Result<Child> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn {tag}"))?;

    if let Some(out) = child.stdout.take() {
        pipe_lines(out, log_tx.clone(), format!("{tag}:stdout"));
    }
    if let Some(err) = child.stderr.take() {
        pipe_lines(err, log_tx.clone(), format!("{tag}:stderr"));
    }

    Ok(child)
}

fn pipe_lines<R: std::io::Read + Send + 'static>(reader: R, log_tx: mpsc::Sender<String>, prefix: String) {
    thread::spawn(move || {
        let br = BufReader::new(reader);
        for line in br.lines().flatten() {
            let _ = log_tx.send(format!("[{prefix}] {line}"));
        }
    });
}

pub fn prune_exited(p: &mut Option<Child>, label: &'static str, log_tx: &mpsc::Sender<String>) {
    let Some(child) = p.as_mut() else { return; };
    match child.try_wait() {
        Ok(Some(status)) => {
            let _ = log_tx.send(format!("{label} exited: {status}"));
            *p = None;
        }
        Ok(None) => {}
        Err(e) => {
            let _ = log_tx.send(format!("{label} wait failed: {e:?}"));
        }
    }
}
