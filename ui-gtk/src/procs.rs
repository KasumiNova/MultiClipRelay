use anyhow::Context;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Default)]
pub struct Procs {
    pub relay: Option<Child>,
    pub watch: Option<Child>,
    pub apply: Option<Child>,
    pub x11: Option<Child>,
}

pub fn terminate_child(mut child: Child, label: &'static str, log_tx: mpsc::Sender<String>) {
    // Best-effort graceful shutdown so `node wl-watch` can clean up its `wl-paste --watch` children.
    thread::spawn(move || {
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

        // Hard kill fallback.
        let _ = child.kill();
        let _ = child.wait();
        let _ = log_tx.send(format!("{label} killed"));
    });
}

pub fn find_sibling_binary(name: &str) -> Option<PathBuf> {
    // When running via `cargo run -p ui-gtk`, current_exe usually is target/debug/ui-gtk.
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
    // Heuristic: when running from cargo build output, prefer sibling binaries.
    s.contains("/target/debug/") || s.contains("/target/release/")
}

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

    // 1) dev: sibling binaries next to current_exe (target/debug|release)
    if prefer_sibling {
        for n in names.iter() {
            if let Some(p) = find_sibling_binary(n) {
                return p;
            }
        }
    }

    // 2) gather all candidates (sibling, PATH, and common system locations)
    for n in names.iter() {
        if let Some(p) = find_sibling_binary(n) {
            candidates.push(p);
        }
        if let Ok(p) = which::which(n) {
            candidates.push(p);
        }

        // Common absolute locations (helpful when PATH has an older ~/.local/bin first)
        add_if_exists(&mut candidates, Path::new("/usr/bin").join(n));
        add_if_exists(&mut candidates, Path::new("/usr/local/bin").join(n));
    }

    // Deduplicate while keeping order.
    let mut uniq: Vec<PathBuf> = Vec::new();
    for p in candidates.into_iter() {
        if !uniq.iter().any(|u| u == &p) {
            uniq.push(p);
        }
    }

    // Pick the newest by mtime if possible.
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
                // If we can't stat mtime, keep it as a last-resort fallback.
                if best.is_none() {
                    best = Some((p, std::time::SystemTime::UNIX_EPOCH));
                }
            }
        }
    }

    best.map(|(p, _)| p)
        .unwrap_or_else(|| PathBuf::from(primary))
}

pub fn spawn_relay(log_tx: &mpsc::Sender<String>, bind_addr: &str) -> anyhow::Result<Child> {
    // Prefer dev sibling (target/*), otherwise pick the newest available binary across sibling/PATH.
    let relay_bin = resolve_binary("multicliprelay-relay", &["relay"]);
    let _ = log_tx.send(format!("starting relay: {}", relay_bin.display()));
    let mut cmd = Command::new(relay_bin);

    let bind_addr = bind_addr.trim();
    if !bind_addr.is_empty() {
        cmd.args(["--bind", bind_addr]);
    }

    spawn_with_logs(&mut cmd, log_tx, "relay")
}

pub fn spawn_node(log_tx: &mpsc::Sender<String>, args: &[&str]) -> anyhow::Result<Child> {
    // Prefer dev sibling (target/*), otherwise pick the newest available binary across sibling/PATH.
    let node_bin = resolve_binary("multicliprelay-node", &["node"]);
    let _ = log_tx.send(format!("starting node: {}", node_bin.display()));
    let mut cmd = Command::new(node_bin);
    cmd.args(args);
    spawn_with_logs(&mut cmd, log_tx, "node")
}

fn spawn_with_logs(
    cmd: &mut Command,
    log_tx: &mpsc::Sender<String>,
    tag: &str,
) -> anyhow::Result<Child> {
    // On Linux, if the UI process is killed abruptly (e.g. SIGKILL), child processes
    // would normally keep running and can leave behind wl-paste watchers.
    // PR_SET_PDEATHSIG makes the kernel deliver SIGTERM to the child when the parent dies.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }

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

fn pipe_lines<R: std::io::Read + Send + 'static>(
    reader: R,
    log_tx: mpsc::Sender<String>,
    prefix: String,
) {
    thread::spawn(move || {
        let br = BufReader::new(reader);
        for line in br.lines().flatten() {
            let _ = log_tx.send(format!("[{prefix}] {line}"));
        }
    });
}
