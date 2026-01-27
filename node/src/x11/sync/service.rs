use anyhow::Context;
use log::{debug, info, warn};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::net::UnixDatagram;

use crate::consts::X11_SYNC_MARKER_MIME;
use crate::hash::sha256_hex;

use super::state;
use super::wl_to_x11::apply_wayland_to_x11_full;
use super::x11_watch::{x11_watch_clipboard_loop, X11Snapshot};

pub struct X11SyncOpts {
    pub state_dir: PathBuf,
    pub poll_interval: Duration,
    pub max_text_bytes: usize,
    pub max_image_bytes: usize,
}

struct RateLimiter {
    window: Duration,
    max: usize,
    hits: VecDeque<Instant>,
}

impl RateLimiter {
    fn new(window: Duration, max: usize) -> Self {
        Self {
            window,
            max,
            hits: VecDeque::new(),
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        while let Some(&t) = self.hits.front() {
            if now.duration_since(t) >= self.window {
                self.hits.pop_front();
            } else {
                break;
            }
        }
        if self.hits.len() >= self.max {
            return false;
        }
        self.hits.push_back(now);
        true
    }
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse::<usize>().ok())
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse::<u64>().ok())
}

pub async fn x11_sync_service(opts: X11SyncOpts) -> anyhow::Result<()> {
    state::ensure_state_dir(&opts.state_dir).await;

    // Safety guard:
    // - rate limit: at most 3 sync tasks per 1s (default)
    // - timeout: abort long-running single task to avoid freezing other programs
    //
    // Env overrides (optional):
    // - MCR_X11_SYNC_MAX_TASKS (default 3)
    // - MCR_X11_SYNC_WINDOW_MS (default 1000)
    // - MCR_X11_SYNC_TASK_TIMEOUT_MS (default 1500)
    let max_tasks = env_usize("MCR_X11_SYNC_MAX_TASKS").unwrap_or(3);
    let window_ms = env_u64("MCR_X11_SYNC_WINDOW_MS").unwrap_or(1000);
    let task_timeout_ms = env_u64("MCR_X11_SYNC_TASK_TIMEOUT_MS").unwrap_or(1500);
    let mut limiter = RateLimiter::new(Duration::from_millis(window_ms), max_tasks);
    let task_timeout = Duration::from_millis(task_timeout_ms);

    // IPC for Wayland -> X11 triggers.
    // x11-hook sends datagrams here; we do the actual X11 clipboard write in this process.
    let sock_path = state::wl_notify_socket_path_for_bind(&opts.state_dir);
    let _ = tokio::fs::remove_file(&sock_path).await;
    let wl_sock = UnixDatagram::bind(&sock_path).context("bind wl notify socket")?;

    // Event-driven X11 -> Wayland.
    // We watch XFixes selection notifications and only sync when the selection changes.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<X11Snapshot>(8);
    let max_text = opts.max_text_bytes;
    let max_img = opts.max_image_bytes;

    tokio::task::spawn_blocking(move || {
        if let Err(e) = x11_watch_clipboard_loop(tx, max_text, max_img) {
            eprintln!("x11-sync: x11 watch loop failed: {:#}", e);
        }
    });

    let mut last_hash: Option<String> = None;
    let mut wl_buf = vec![0u8; 128];

    // Coalesce storms:
    // - X11 -> WL: keep only the latest snapshot
    // - WL -> X11: boolean "pending" is enough because the worker does a full wl scan
    let mut pending_x11_to_wl: Option<X11Snapshot> = None;
    let mut pending_wl_to_x11: bool = false;

    // Background tick to retry pending tasks when rate-limited.
    // Use opts.poll_interval as a reasonable cadence (user-tunable), but cap to avoid too-fast spinning.
    let tick_every = std::cmp::max(Duration::from_millis(50), opts.poll_interval);
    let mut tick = tokio::time::interval(tick_every);
    tick.tick().await;

    async fn run_x11_to_wl_once(
        mut snap: X11Snapshot,
        last_hash: &mut Option<String>,
    ) {
        // Skip echo: if X11 clipboard was produced by us from Wayland, it will contain our marker with payload from=wl.
        if snap.marked_from_wayland {
            debug!("x11->wl skip: x11 clipboard marked from wl");
            return;
        }

        // If X11 has no meaningful data (e.g. no owner / selection cleared), do not override Wayland.
        if snap.items.is_empty() {
            debug!("x11->wl skip: empty snapshot");
            return;
        }

        // Construct Wayland multi-mime set, and tag it as originating from X11.
        let mut items: Vec<(String, Vec<u8>)> = Vec::new();
        items.push((X11_SYNC_MARKER_MIME.to_string(), b"from=x11".to_vec()));

        let mut payload_count = 0usize;
        for (mime, bytes) in snap.items.drain(..) {
            if bytes.is_empty() {
                continue;
            }
            if mime == X11_SYNC_MARKER_MIME {
                continue;
            }
            payload_count += 1;
            items.push((mime, bytes));
        }
        if payload_count == 0 {
            debug!("x11->wl skip: marker-only payload");
            return;
        }

        // Hash guard.
        let hash_material = items
            .iter()
            .map(|(m, b)| format!("{}:{}", m, sha256_hex(b)))
            .collect::<Vec<_>>()
            .join("\n");
        let sha = sha256_hex(hash_material.as_bytes());
        if last_hash.as_deref() == Some(&sha) {
            debug!("x11->wl skip: same hash {sha}");
            return;
        }

        match crate::clipboard::wl_copy_multi(items).await {
            Ok(()) => info!("x11->wl applied (hash={sha})"),
            Err(e) => warn!("x11->wl failed to write wl clipboard: {e:?}"),
        }
        *last_hash = Some(sha);
    }

    loop {
        tokio::select! {
            _ = tick.tick() => {
                // Retry pending tasks when allowed.
                let now = Instant::now();

                if let Some(snap) = pending_x11_to_wl.take() {
                    if limiter.allow(now) {
                        match tokio::time::timeout(task_timeout, run_x11_to_wl_once(snap, &mut last_hash)).await {
                            Ok(()) => {}
                            Err(_) => warn!("x11-sync guard: x11->wl task timed out after {:?}", task_timeout),
                        }
                    } else {
                        pending_x11_to_wl = Some(snap);
                    }
                    continue;
                }

                if pending_wl_to_x11 {
                    if limiter.allow(now) {
                        pending_wl_to_x11 = false;
                        match tokio::time::timeout(task_timeout, apply_wayland_to_x11_full(&opts.state_dir)).await {
                            Ok(()) => {}
                            Err(_) => warn!("x11-sync guard: wl->x11 task timed out after {:?}", task_timeout),
                        }
                    }
                }
            }
            maybe = rx.recv() => {
                let Some(snap) = maybe else { break; };

                // Coalesce to latest snapshot. We either run immediately (if allowed), or defer.
                let now = Instant::now();
                if limiter.allow(now) {
                    match tokio::time::timeout(task_timeout, run_x11_to_wl_once(snap, &mut last_hash)).await {
                        Ok(()) => {}
                        Err(_) => warn!("x11-sync guard: x11->wl task timed out after {:?}", task_timeout),
                    }
                } else {
                    pending_x11_to_wl = Some(snap);
                }
            }
            recv = wl_sock.recv_from(&mut wl_buf) => {
                if recv.is_ok() {
                    debug!("wl notify received -> wl->x11 scan/apply");
                    let now = Instant::now();
                    if limiter.allow(now) {
                        match tokio::time::timeout(task_timeout, apply_wayland_to_x11_full(&opts.state_dir)).await {
                            Ok(()) => {}
                            Err(_) => warn!("x11-sync guard: wl->x11 task timed out after {:?}", task_timeout),
                        }
                    } else {
                        // apply_wayland_to_x11_full does a full scan; just mark pending.
                        pending_wl_to_x11 = true;
                    }
                }
            }
        }
    }

    Ok(())
}
