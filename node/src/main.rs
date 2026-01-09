use anyhow::Context;
use clap::{Parser, Subcommand};
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use std::io;
use tokio::sync::watch;

use utils::{Kind, Message};

use node::clipboard::{wl_copy, wl_copy_multi, wl_paste};
use node::consts::{
    APPLIED_MARKER_MIME, FILE_SUPPRESS_KEY, GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME,
    URI_LIST_MIME,
};
use node::hash::sha256_hex;
use node::history::{record_recv, record_send};
use node::image_mode::{parse_image_mode, ImageMode};
use node::net::{connect, send_frame, send_join};
use node::paths::{default_state_dir, first_8, is_tar_payload, received_dir, safe_for_filename};
use node::suppress::{is_file_suppressed, is_suppressed, set_file_suppress, set_suppress};
use node::transfer_file::{
    build_uri_list, collect_clipboard_paths, send_file, send_paths_as_file, unpack_tar_bytes,
};
use node::transfer_image::{image_mimes, send_image, to_png};
use node::x11_sync::{pause_x11_text_sync, x11_hook_apply_wayland_to_x11, x11_sync_service, X11SyncOpts};

// (ImageMode + parsing are in node::image_mode)

#[derive(Clone, Debug)]
struct Ctx {
    state_dir: PathBuf,
    device_id: String,
}

#[derive(Parser)]
#[command(name = "multicliprelay-node")]
struct Cli {
    /// Directory for local state (device id, suppress markers).
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,

    /// Override device id (otherwise generated and persisted under state_dir).
    #[arg(long, global = true)]
    device_id: Option<String>,

    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Listen {
        #[arg(long, default_value = "default")]
        room: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
    },
    SendText {
        #[arg(long, default_value = "default")]
        room: String,
        #[arg(long)]
        text: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
    },
    SendImage {
        #[arg(long, default_value = "default")]
        room: String,
        /// Path to an image file (png/jpeg/webp/gif recommended)
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
        /// Max bytes allowed to send
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_bytes: usize,
        /// Image mode: passthrough keeps original mime; force-png converts and sends image/png.
        #[arg(long, default_value = "force-png")]
        image_mode: String,
    },

    SendFile {
        #[arg(long, default_value = "default")]
        room: String,
        /// Path to any file
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
        /// Max bytes allowed to send
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_file_bytes: usize,
    },

    /// Watch local Wayland clipboard (text + image/png) and publish to relay.
    WlWatch {
        #[arg(long, default_value = "default")]
        room: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
        /// Watch mode: "watch" uses wl-paste --watch (event-driven), "poll" uses polling.
        #[arg(long, default_value = "watch")]
        mode: String,
        /// Poll interval (ms), only used when mode=poll.
        #[arg(long, default_value_t = 200)]
        interval_ms: u64,
        #[arg(long, default_value_t = 1 * 1024 * 1024)]
        max_text_bytes: usize,
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_image_bytes: usize,
        /// Max bytes allowed to send for file clipboard (text/uri-list)
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_file_bytes: usize,
        /// Image mode: passthrough keeps original mime; force-png converts and sends image/png.
        #[arg(long, default_value = "force-png")]
        image_mode: String,
    },

    /// Apply incoming events to local Wayland clipboard (text + image/png).
    WlApply {
        #[arg(long, default_value = "default")]
        room: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
        /// Image mode: passthrough writes original mime; force-png converts and writes image/png.
        #[arg(long, default_value = "force-png")]
        image_mode: String,
    },

    /// Internal: invoked by wl-paste --watch to publish current clipboard content.
    #[command(hide = true)]
    WlPublishCurrent {
        #[arg(long, default_value = "default")]
        room: String,
        #[arg(long, default_value = "127.0.0.1:8080")]
        relay: String,
        /// "text" or "image/png"
        #[arg(long)]
        mime: String,
        #[arg(long, default_value_t = 1 * 1024 * 1024)]
        max_text_bytes: usize,
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_image_bytes: usize,
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_file_bytes: usize,
        /// Image mode: passthrough keeps original mime; force-png converts and sends image/png.
        #[arg(long, default_value = "force-png")]
        image_mode: String,
    },

    /// Sync clipboard between X11 and Wayland (replaces legacy xclip_sync.sh).
    ///
    /// - X11 -> Wayland: polling (small interval)
    /// - Wayland -> X11: event-driven via wl-paste --watch
    X11Sync {
        /// X11 poll interval (ms)
        #[arg(long, default_value_t = 200)]
        x11_poll_interval_ms: u64,
        #[arg(long, default_value_t = 1 * 1024 * 1024)]
        max_text_bytes: usize,
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_image_bytes: usize,
    },

    /// Internal: invoked by wl-paste --watch for Wayland -> X11 sync.
    #[command(hide = true)]
    X11Hook {
        /// text | image
        #[arg(long)]
        kind: String,
        /// Max stdin bytes allowed
        #[arg(long, default_value_t = 20 * 1024 * 1024)]
        max_bytes: usize,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Enable logging when the user sets RUST_LOG (kept quiet by default).
    // Useful for diagnosing clipboard edge cases.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();

    // Internal hook mode: wl-paste --watch can only execute a single command (no extra args).
    // We use env vars to pass parameters and run a hidden publish step when invoked without args.
    if std::env::var_os("MCR_WL_WATCH_HOOK").is_some() && std::env::args_os().len() <= 1 {
        return wl_watch_hook().await;
    }

    let cli = Cli::parse();

    let state_dir = cli.state_dir.unwrap_or_else(default_state_dir);
    tokio::fs::create_dir_all(&state_dir)
        .await
        .context("create state_dir")?;
    let device_id = match cli.device_id {
        Some(id) => id,
        None => get_or_create_device_id(&state_dir).await?,
    };
    let ctx = Ctx {
        state_dir,
        device_id,
    };

    match cli.cmd {
        Commands::Listen { room, relay } => listen_mode(&ctx, &room, &relay).await?,
        Commands::SendText { room, text, relay } => send_text(&ctx, &room, &text, &relay).await?,
        Commands::SendImage {
            room,
            file,
            relay,
            max_bytes,
            image_mode,
        } => {
            let im = parse_image_mode(&image_mode)?;
            send_image(&ctx.device_id, &room, &file, &relay, max_bytes, im).await?;
            println!("sent image to room {}", room);
        }
        Commands::SendFile {
            room,
            file,
            relay,
            max_file_bytes,
        } => send_file(&ctx.device_id, &room, &file, &relay, max_file_bytes).await?,
        Commands::WlWatch {
            room,
            relay,
            mode,
            interval_ms,
            max_text_bytes,
            max_image_bytes,
            max_file_bytes,
            image_mode,
        } => {
            let im = parse_image_mode(&image_mode)?;
            wl_watch(
                &ctx,
                &room,
                &relay,
                &mode,
                interval_ms,
                max_text_bytes,
                max_image_bytes,
                max_file_bytes,
                im,
            )
            .await?
        }
        Commands::WlApply {
            room,
            relay,
            image_mode,
        } => {
            let im = parse_image_mode(&image_mode)?;
            wl_apply(&ctx, &room, &relay, im).await?
        }
        Commands::WlPublishCurrent {
            room,
            relay,
            mime,
            max_text_bytes,
            max_image_bytes,
            max_file_bytes,
            image_mode,
        } => {
            let im = parse_image_mode(&image_mode)?;
            wl_publish_current(
                &ctx,
                &room,
                &relay,
                &mime,
                max_text_bytes,
                max_image_bytes,
                max_file_bytes,
                im,
            )
            .await?
        }

        Commands::X11Sync {
            x11_poll_interval_ms,
            max_text_bytes,
            max_image_bytes,
        } => {
            async fn ensure_bin(name: &str) -> anyhow::Result<()> {
                match Command::new(name)
                    .arg("--help")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output()
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        anyhow::bail!("required program not found in PATH: {name}")
                    }
                    Err(e) => Err(anyhow::anyhow!(e)).context(format!("check program: {name}")),
                }
            }

            ensure_bin("xclip").await?;
            ensure_bin("wl-paste").await?;

            // Guard against multiple instances (spawns background wl-paste watchers).
            let _lock = acquire_instance_lock(&ctx.state_dir, "x11-sync", "local", "x11")?;

            // Spawn event-driven watchers (Wayland -> X11).
            let exe = std::env::current_exe().context("current_exe")?;
            let state_dir = ctx.state_dir.clone();
            let spawn_watch = |mime: &str, kind: &str| {
                let mut cmd = Command::new("wl-paste");
                cmd.arg("--type").arg(mime)
                    .arg("--watch")
                    .arg(exe.clone())
                    .arg("--state-dir")
                    .arg(state_dir.clone())
                    .arg("x11-hook")
                    .arg("--kind")
                    .arg(kind)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());

                // Ensure watcher dies with parent.
                #[cfg(unix)]
                unsafe {
                    cmd.pre_exec(|| {
                        let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                        Ok(())
                    });
                }

                cmd.spawn()
            };

            let _wl_text = spawn_watch("text", "text").context("spawn wl-paste text watch")?;
            let _wl_img = spawn_watch("image", "image").context("spawn wl-paste image watch")?;

            // Main loop: X11 -> Wayland.
            x11_sync_service(X11SyncOpts {
                state_dir: ctx.state_dir.clone(),
                poll_interval: Duration::from_millis(x11_poll_interval_ms),
                max_text_bytes,
                max_image_bytes,
            })
            .await?;
        }

        Commands::X11Hook { kind, max_bytes } => {
            // Read stdin fully (wl-paste provides the selection data).
            let mut buf = Vec::new();
            tokio::io::stdin()
                .take(max_bytes as u64 + 1)
                .read_to_end(&mut buf)
                .await
                .ok();
            if buf.len() > max_bytes {
                return Ok(());
            }
            x11_hook_apply_wayland_to_x11(&ctx.state_dir, &kind, buf).await;
        }
    }
    Ok(())
}

async fn wl_watch_hook() -> anyhow::Result<()> {
    let debug_path = std::env::var("MCR_HOOK_DEBUG_PATH").ok();
    let debug = |line: &str| {
        if let Some(p) = debug_path.as_deref() {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "{}", line)
                });
        }
    };

    let state_dir = std::env::var_os("MCR_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_state_dir);
    tokio::fs::create_dir_all(&state_dir).await.ok();

    let device_id = std::env::var("MCR_DEVICE_ID")
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let room = std::env::var("MCR_ROOM").unwrap_or_else(|_| "default".to_string());
    let relay = std::env::var("MCR_RELAY").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    let max_text_bytes = std::env::var("MCR_MAX_TEXT_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1 * 1024 * 1024);
    let max_image_bytes = std::env::var("MCR_MAX_IMAGE_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20 * 1024 * 1024);
    let max_file_bytes = std::env::var("MCR_MAX_FILE_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(20 * 1024 * 1024);

    let image_mode = std::env::var("MCR_IMAGE_MODE").unwrap_or_else(|_| "force-png".to_string());
    let im = parse_image_mode(&image_mode)?;

    let ctx = Ctx {
        state_dir,
        device_id,
    };

    // If we were triggered by a specific watcher, wl-paste pipes that type to stdin.
    // Prefer using stdin bytes directly (avoids nested wl-paste calls which can be flaky).
    let candidate = std::env::var("MCR_WATCH_CANDIDATE_MIME").ok();
    if let Some(candidate) = candidate {
        debug(&format!("hook: candidate={}", candidate));
        let cap = if candidate.starts_with("image/") {
            max_image_bytes
        } else {
            // text + uri-list/gnome are tiny in practice, but keep a generous cap.
            std::cmp::max(max_text_bytes, max_file_bytes)
        };

        let mut stdin = tokio::io::stdin();
        let mut stored: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        let mut too_big = false;
        loop {
            let n = match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if !too_big {
                if stored.len() + n > cap {
                    too_big = true;
                } else {
                    stored.extend_from_slice(&buf[..n]);
                }
            }
            // If too_big, continue draining without storing to avoid blocking wl-paste.
        }
        if too_big {
            debug(&format!("hook: stdin too big (cap={})", cap));
            return Ok(());
        }

        debug(&format!("hook: stdin_bytes={}", stored.len()));

        // Determine best MIME for this selection.
        let out = match Command::new("wl-paste").arg("--list-types").output().await {
            Ok(o) => o,
            Err(_) => return Ok(()),
        };
        let types = String::from_utf8_lossy(&out.stdout);
        let has = |m: &str| types.lines().any(|l| l.trim() == m);

        // If the clipboard contains our "applied" marker, it was written by wl-apply.
        // Ignore to prevent feedback loops (apply -> watch -> re-send).
        if has(APPLIED_MARKER_MIME) {
            debug("hook: applied marker present; ignore");
            return Ok(());
        }

        let chosen = if has(URI_LIST_MIME) {
            URI_LIST_MIME
        } else if has(KDE_URI_LIST_MIME) {
            KDE_URI_LIST_MIME
        } else if has(GNOME_COPIED_FILES_MIME) {
            GNOME_COPIED_FILES_MIME
        } else {
            let choose_image = || {
                if im == ImageMode::MultiMime {
                    for m in ["image/jpeg", "image/webp", "image/gif", "image/png"] {
                        if has(m) {
                            return Some(m);
                        }
                    }
                } else {
                    if has("image/png") {
                        return Some("image/png");
                    }
                    for m in ["image/jpeg", "image/webp", "image/gif"] {
                        if has(m) {
                            return Some(m);
                        }
                    }
                }
                None
            };

            if let Some(m) = choose_image() {
                m
            } else if has("text/plain;charset=utf-8") {
                "text/plain;charset=utf-8"
            } else if has("text/plain") {
                "text/plain"
            } else {
                return Ok(());
            }
        };

        if candidate != chosen {
            debug(&format!("hook: chosen={} candidate_mismatch", chosen));
            return Ok(());
        }

        debug(&format!("hook: chosen={}", chosen));

        // Publish using the stdin bytes for the chosen type.
        if chosen == URI_LIST_MIME || chosen == KDE_URI_LIST_MIME || chosen == GNOME_COPIED_FILES_MIME {
            // Multiple supervised wl-paste watchers can trigger nearly at the same time.
            // Use a short-lived non-blocking lock to ensure we only process one file event
            // per clipboard change, preventing duplicate sends and feedback-loop amplification.
            #[cfg(unix)]
            let _hook_lock = match acquire_instance_lock(&ctx.state_dir, "wl-watch-hook-file", &room, &relay) {
                Ok(f) => Some(f),
                Err(e) => {
                    debug(&format!("hook: file lock busy or error: {:#}", e));
                    return Ok(());
                }
            };

            let paths = collect_clipboard_paths(&stored);
            let mut uniq: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
            for p in paths {
                uniq.insert(p);
            }
            let paths: Vec<PathBuf> = uniq.into_iter().collect();
            if paths.is_empty() {
                debug("hook: no paths in uri-list");
                return Ok(());
            }
            let _ = send_paths_as_file(
                &ctx.state_dir,
                &ctx.device_id,
                &room,
                &relay,
                paths,
                max_file_bytes,
            )
            .await?;

            // Pause x11-sync text synchronization to prevent it from overriding the file clipboard.
            pause_x11_text_sync(&ctx.state_dir, Duration::from_millis(2000)).await;

            // File clipboards may also provide a text/plain `file:///...` representation.
            // Suppress text sends briefly to avoid overriding receiver clipboard with host paths.
            set_suppress(
                &ctx.state_dir,
                &room,
                "text/plain;charset=utf-8",
                "*",
                Duration::from_millis(1500),
            )
            .await;
            set_suppress(
                &ctx.state_dir,
                &room,
                "text/plain",
                "*",
                Duration::from_millis(1500),
            )
            .await;
            debug("hook: sent file bundle");
            return Ok(());
        }

        // Some clipboard producers may expose file copies as plain text containing `file:///...`
        // without offering `text/uri-list` consistently. Treat such payloads as file clipboard.
        if chosen.starts_with("text/plain") {
            let mut paths = collect_clipboard_paths(&stored);
            if !paths.is_empty() {
                let mut existing: Vec<PathBuf> = Vec::new();
                for p in paths.drain(..) {
                    if tokio::fs::metadata(&p).await.is_ok() {
                        existing.push(p);
                    }
                }

                let mut uniq: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
                for p in existing {
                    uniq.insert(p);
                }
                let existing: Vec<PathBuf> = uniq.into_iter().collect();

                if !existing.is_empty() {
                    let _ = send_paths_as_file(
                        &ctx.state_dir,
                        &ctx.device_id,
                        &room,
                        &relay,
                        existing,
                        max_file_bytes,
                    )
                    .await?;

                    // Pause x11-sync text synchronization.
                    pause_x11_text_sync(&ctx.state_dir, Duration::from_millis(2000)).await;

                    // Suppress follow-up text/plain `file:///...` updates.
                    set_suppress(
                        &ctx.state_dir,
                        &room,
                        "text/plain;charset=utf-8",
                        "*",
                        Duration::from_millis(1500),
                    )
                    .await;
                    set_suppress(
                        &ctx.state_dir,
                        &room,
                        "text/plain",
                        "*",
                        Duration::from_millis(1500),
                    )
                    .await;
                    debug("hook: text looked like file URIs; sent as file bundle");
                    return Ok(());
                }
            }
        }

        let (send_mime, send_bytes) = if chosen.starts_with("image/") {
            match im {
                ImageMode::ForcePng => match to_png(&stored) {
                    Ok(png) => ("image/png", png),
                    Err(e) => {
                        debug(&format!("hook: to_png failed: {:#}", e));
                        return Ok(());
                    }
                },
                ImageMode::Passthrough | ImageMode::MultiMime | ImageMode::SpoofPng => {
                    (chosen, stored)
                }
            }
        } else {
            (chosen, stored)
        };

        let sha = sha256_hex(&send_bytes);
        if is_suppressed(&ctx.state_dir, &room, send_mime, &sha).await {
            debug(&format!("hook: suppressed mime={} sha={}", send_mime, sha));
            return Ok(());
        }

        debug(&format!("hook: sending mime={} bytes={}", send_mime, send_bytes.len()));

        let stream = match connect(&relay).await {
            Ok(s) => s,
            Err(e) => {
                debug(&format!("hook: connect failed: {:#}", e));
                return Ok(());
            }
        };
        let mut msg = if send_mime.starts_with("text/") {
            let mut m = Message::new_text(&ctx.device_id, &room, "");
            m.payload = Some(send_bytes);
            m.size = m.payload.as_ref().map(|p| p.len()).unwrap_or(0);
            m
        } else {
            Message::new_image(&ctx.device_id, &room, send_mime, send_bytes)
        };
        msg.sha256 = Some(sha);
        if let Err(e) = send_frame(stream, msg.to_bytes()).await {
            debug(&format!("hook: send_frame failed: {:#}", e));
            return Ok(());
        }
        record_send(
            &ctx.device_id,
            &room,
            &relay,
            msg.kind,
            Some(send_mime.to_string()),
            msg.name.clone(),
            msg.size,
            msg.sha256.clone(),
        )
        .await;
        debug("hook: send done");
        return Ok(());
    }

    // Fallback: if invoked without a candidate watcher MIME, use the original logic.
    wl_publish_current(
        &ctx,
        &room,
        &relay,
        "auto",
        max_text_bytes,
        max_image_bytes,
        max_file_bytes,
        im,
    )
    .await
}

#[cfg(unix)]
fn acquire_instance_lock(
    state_dir: &PathBuf,
    name: &str,
    room: &str,
    relay: &str,
) -> anyhow::Result<File> {
    use std::os::unix::io::AsRawFd;

    let lock_name = format!(
        "{}_room={}_relay={}.lock",
        name,
        safe_for_filename(room),
        safe_for_filename(relay)
    );
    let lock_path = state_dir.join(lock_name);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;

    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        // EWOULDBLOCK means another instance holds the lock.
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            anyhow::bail!(
                "{} already running (lock busy): {}",
                name,
                lock_path.display()
            );
        }
        return Err(anyhow::Error::new(e)).context("flock");
    }
    Ok(f)
}

#[cfg(not(unix))]
fn acquire_instance_lock(
    _state_dir: &PathBuf,
    _name: &str,
    _room: &str,
    _relay: &str,
) -> anyhow::Result<()> {
    Ok(())
}

async fn get_or_create_device_id(state_dir: &PathBuf) -> anyhow::Result<String> {
    let p = state_dir.join("device_id");
    if let Ok(s) = tokio::fs::read_to_string(&p).await {
        let id = s.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    tokio::fs::write(&p, &id).await.context("write device_id")?;
    Ok(id)
}

async fn read_loop(mut reader: tokio::net::tcp::OwnedReadHalf) -> anyhow::Result<()> {
    loop {
        let len = match reader.read_u32().await {
            Ok(l) => l as usize,
            Err(_) => break,
        };
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await.context("read payload")?;
        let msg = Message::from_bytes(&buf);
        match msg.kind {
            Kind::Text => {
                let text = msg
                    .payload
                    .as_ref()
                    .map(|p| String::from_utf8_lossy(p).to_string())
                    .unwrap_or_default();
                println!(
                    "RECV from {} kind={:?} text={}",
                    msg.device_id, msg.kind, text
                );
            }
            Kind::Image => {
                println!(
                    "RECV from {} kind={:?} mime={} bytes={} sha256={}",
                    msg.device_id,
                    msg.kind,
                    msg.mime.clone().unwrap_or_default(),
                    msg.size,
                    msg.sha256.clone().unwrap_or_default()
                );
            }
            Kind::File => {
                println!(
                    "RECV from {} kind={:?} name={} mime={} bytes={} sha256={}",
                    msg.device_id,
                    msg.kind,
                    msg.name.clone().unwrap_or_else(|| "(no-name)".into()),
                    msg.mime.clone().unwrap_or_default(),
                    msg.size,
                    msg.sha256.clone().unwrap_or_default()
                );
            }
            Kind::Join => {
                println!("RECV from {} kind=Join", msg.device_id);
            }
        }
    }
    Ok(())
}

async fn listen_mode(ctx: &Ctx, room: &str, relay: &str) -> anyhow::Result<()> {
    let stream = connect(relay).await?;
    let (reader, mut writer) = stream.into_split();

    // Send a Join message so the relay can register us into the room.
    send_join(&mut writer, &ctx.device_id, room).await?;

    // spawn reader
    tokio::spawn(async move {
        let _ = read_loop(reader).await;
    });
    println!("Listening in room '{}' on {}", room, relay);
    // keep alive
    // keep writer alive as well, otherwise the TCP write half may close and server may disconnect.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        let _ = &writer;
    }
}

async fn send_text(ctx: &Ctx, room: &str, text: &str, relay: &str) -> anyhow::Result<()> {
    let stream = connect(relay).await?;
    let mut msg = Message::new_text(&ctx.device_id, room, text);
    let sha = sha256_hex(msg.payload.as_deref().unwrap_or_default());
    msg.sha256 = Some(sha.clone());
    let buf = msg.to_bytes();
    send_frame(stream, buf).await?;
    record_send(
        &ctx.device_id,
        room,
        relay,
        Kind::Text,
        Some("text/plain;charset=utf-8".to_string()),
        None,
        msg.size,
        Some(sha),
    )
    .await;
    println!("sent text to room {}", room);
    Ok(())
}

async fn wl_watch(
    ctx: &Ctx,
    room: &str,
    relay: &str,
    mode: &str,
    interval_ms: u64,
    max_text_bytes: usize,
    max_image_bytes: usize,
    max_file_bytes: usize,
    image_mode: ImageMode,
) -> anyhow::Result<()> {
    // Guard against accidentally starting multiple watchers (which spawns multiple `wl-paste --watch` processes).
    // Holding this lock for the duration of the command keeps the system tidy.
    let _lock = acquire_instance_lock(&ctx.state_dir, "wl-watch", room, relay)?;

    match mode {
        "watch" => {
            wl_watch_evented(
                ctx,
                room,
                relay,
                max_text_bytes,
                max_image_bytes,
                max_file_bytes,
                image_mode,
            )
            .await
        }
        "poll" => {
            wl_watch_poll(
                ctx,
                room,
                relay,
                interval_ms,
                max_text_bytes,
                max_image_bytes,
                max_file_bytes,
                image_mode,
            )
            .await
        }
        other => anyhow::bail!("invalid --mode {}, expected watch|poll", other),
    }
}

async fn wl_watch_poll(
    ctx: &Ctx,
    room: &str,
    relay: &str,
    interval_ms: u64,
    max_text_bytes: usize,
    max_image_bytes: usize,
    max_file_bytes: usize,
    image_mode: ImageMode,
) -> anyhow::Result<()> {
    let stream = connect(relay).await?;
    let (_reader, mut writer) = stream.into_split();
    send_join(&mut writer, &ctx.device_id, room).await?;
    println!("wl-watch(poll): room='{}' relay='{}'", room, relay);

    let mut last_text_hash: Option<String> = None;
    let mut last_img_hash: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut last_file_hash: Option<String> = None;

    loop {
        // If wl-apply recently wrote the clipboard, it will include our marker MIME.
        // Avoid polling and re-sending during that window.
        if let Ok(out) = Command::new("wl-paste").arg("--list-types").output().await {
            let types = String::from_utf8_lossy(&out.stdout);
            if types.lines().any(|l| l.trim() == APPLIED_MARKER_MIME) {
                tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
                continue;
            }
        }

        // files (uri-list / KDE / gnome)
        let mut list_bytes: Option<Vec<u8>> = None;
        if let Ok(b) = wl_paste(URI_LIST_MIME).await {
            if !b.is_empty() {
                list_bytes = Some(b);
            }
        }
        if list_bytes.is_none() {
            if let Ok(b) = wl_paste(KDE_URI_LIST_MIME).await {
                if !b.is_empty() {
                    list_bytes = Some(b);
                }
            }
        }
        if list_bytes.is_none() {
            if let Ok(b) = wl_paste(GNOME_COPIED_FILES_MIME).await {
                if !b.is_empty() {
                    list_bytes = Some(b);
                }
            }
        }
        if let Some(list_bytes) = list_bytes {
            let paths = collect_clipboard_paths(&list_bytes);
            // quick de-dupe
            let mut uniq: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
            for p in paths {
                uniq.insert(p);
            }
            let paths: Vec<PathBuf> = uniq.into_iter().collect();

            let maybe_sha = if paths.is_empty() {
                None
            } else {
                // Cheap pre-check: for single file we can compute sha after reading; for bundles, sha is from tar.
                // Loop prevention uses the sha of the transmitted payload.
                if last_file_hash.as_deref().is_some() {
                    // keep as-is
                }
                if let Some(sha) = send_paths_as_file(
                    &ctx.state_dir,
                    &ctx.device_id,
                    room,
                    relay,
                    paths,
                    max_file_bytes,
                )
                .await?
                {
                    Some(sha)
                } else {
                    None
                }
            };

            // Pause x11-sync text synchronization whenever we handle file clipboard.
            pause_x11_text_sync(&ctx.state_dir, Duration::from_millis(2000)).await;

            if let Some(sha) = maybe_sha {
                if last_file_hash.as_deref() != Some(&sha)
                    && !is_file_suppressed(&ctx.state_dir, room, &sha).await
                {
                    last_file_hash = Some(sha);
                }
            }

            // File clipboards may also (briefly) provide text/plain with `file:///...`.
            // Suppress text sends briefly to avoid overriding receiver clipboard with host paths.
            set_suppress(
                &ctx.state_dir,
                room,
                "text/plain;charset=utf-8",
                "*",
                Duration::from_millis(1500),
            )
            .await;
            set_suppress(
                &ctx.state_dir,
                room,
                "text/plain",
                "*",
                Duration::from_millis(1500),
            )
            .await;

            // Treat file clipboard as dominant for this tick.
            tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
            continue;
        }

        // text/plain
        if let Ok(text_bytes) = wl_paste("text/plain;charset=utf-8").await {
            if !text_bytes.is_empty() && text_bytes.len() <= max_text_bytes {
                let mut paths = collect_clipboard_paths(&text_bytes);
                if !paths.is_empty() {
                    let mut existing: Vec<PathBuf> = Vec::new();
                    for p in paths.drain(..) {
                        if tokio::fs::metadata(&p).await.is_ok() {
                            existing.push(p);
                        }
                    }
                    let mut uniq: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
                    for p in existing {
                        uniq.insert(p);
                    }
                    let existing: Vec<PathBuf> = uniq.into_iter().collect();

                    if !existing.is_empty() {
                        if let Some(sha) = send_paths_as_file(
                            &ctx.state_dir,
                            &ctx.device_id,
                            room,
                            relay,
                            existing,
                            max_file_bytes,
                        )
                        .await?
                        {
                            last_file_hash = Some(sha);
                        }

                        // Pause x11-sync text synchronization.
                        pause_x11_text_sync(&ctx.state_dir, Duration::from_millis(2000)).await;

                        set_suppress(
                            &ctx.state_dir,
                            room,
                            "text/plain;charset=utf-8",
                            "*",
                            Duration::from_millis(1500),
                        )
                        .await;
                        set_suppress(
                            &ctx.state_dir,
                            room,
                            "text/plain",
                            "*",
                            Duration::from_millis(1500),
                        )
                        .await;

                        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
                        continue;
                    }
                }

                let h = sha256_hex(&text_bytes);
                if last_text_hash.as_deref() != Some(&h)
                    && !is_suppressed(&ctx.state_dir, room, "text/plain;charset=utf-8", &h).await
                {
                    let mut msg = Message::new_text(&ctx.device_id, room, "");
                    msg.payload = Some(text_bytes);
                    msg.size = msg.payload.as_ref().map(|p| p.len()).unwrap_or(0);
                    msg.sha256 = Some(h.clone());
                    let buf = msg.to_bytes();
                    writer.write_u32(buf.len() as u32).await?;
                    writer.write_all(&buf).await?;
                    record_send(
                        &ctx.device_id,
                        room,
                        relay,
                        Kind::Text,
                        Some("text/plain;charset=utf-8".to_string()),
                        None,
                        msg.size,
                        Some(h.clone()),
                    )
                    .await;
                    last_text_hash = Some(h);
                }
            }
        }

        // images
        let mut sent_non_png = false;
        for &mime in image_mimes().iter() {
            if let Ok(img_bytes) = wl_paste(mime).await {
                if img_bytes.is_empty() || img_bytes.len() > max_image_bytes {
                    continue;
                }
                if image_mode == ImageMode::MultiMime && mime == "image/png" && sent_non_png {
                    // In multi-mime mode, prefer publishing the non-png representation to the relay
                    // to preserve the original format across devices.
                    continue;
                }

                let mut send_mime = mime;
                let mut send_bytes: Vec<u8> = img_bytes;
                if image_mode == ImageMode::ForcePng {
                    if let Ok(png) = to_png(&send_bytes) {
                        send_mime = "image/png";
                        send_bytes = png;
                    } else {
                        continue;
                    }
                }
                let h = sha256_hex(&send_bytes);
                if last_img_hash.get(send_mime).map(|s| s.as_str()) != Some(&h)
                    && !is_suppressed(&ctx.state_dir, room, send_mime, &h).await
                {
                    let mut msg = Message::new_image(&ctx.device_id, room, send_mime, send_bytes);
                    msg.sha256 = Some(h.clone());
                    let buf = msg.to_bytes();
                    writer.write_u32(buf.len() as u32).await?;
                    writer.write_all(&buf).await?;
                    record_send(
                        &ctx.device_id,
                        room,
                        relay,
                        Kind::Image,
                        Some(send_mime.to_string()),
                        None,
                        msg.size,
                        Some(h.clone()),
                    )
                    .await;
                    last_img_hash.insert(send_mime.to_string(), h);
                    if send_mime != "image/png" {
                        sent_non_png = true;
                    }
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
    }
}

#[cfg(unix)]
async fn wl_watch_evented(
    ctx: &Ctx,
    room: &str,
    relay: &str,
    max_text_bytes: usize,
    max_image_bytes: usize,
    max_file_bytes: usize,
    image_mode: ImageMode,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    println!("wl-watch(watch): room='{}' relay='{}'", room, relay);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("install SIGTERM handler")?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .context("install SIGINT handler")?;

    // wl-paste --watch takes a single command (no args), and its triggering behavior depends on
    // being able to paste a type. Some clipboards offer ONLY image/* or ONLY text/uri-list.
    //
    // To reliably trigger on images + files + text, we supervise multiple watchers, one per MIME.
    // Each watcher passes its MIME as a *candidate*; the hook will only publish when that
    // candidate equals the current best MIME (auto-detected), preventing duplicates.
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    let debug_hook_path = if std::env::var_os("MCR_WL_WATCH_DEBUG").is_some() {
        Some(ctx.state_dir.join("wl_watch_hook.log").to_string_lossy().to_string())
    } else {
        None
    };

    let mut watch_mimes: Vec<String> = Vec::new();
    watch_mimes.push(URI_LIST_MIME.to_string());
    watch_mimes.push(GNOME_COPIED_FILES_MIME.to_string());
    watch_mimes.push(KDE_URI_LIST_MIME.to_string());
    watch_mimes.push("text/plain;charset=utf-8".to_string());
    watch_mimes.push("text/plain".to_string());
    for &m in image_mimes().iter() {
        watch_mimes.push(m.to_string());
    }

    for mime in watch_mimes {
        let mut stop_rx = stop_rx.clone();
        let exe = exe.clone();
        let state_dir = ctx.state_dir.clone();
        let device_id = ctx.device_id.clone();
        let room = room.to_string();
        let relay = relay.to_string();
        let im = image_mode;
        let debug_hook_path = debug_hook_path.clone();

        let handle = tokio::spawn(async move {
            // Backoff to avoid hot loops when the mime isn't currently offered.
            let backoff = Duration::from_millis(300);
            loop {
                if *stop_rx.borrow() {
                    break;
                }

                let mut cmd = Command::new("wl-paste");
                cmd.arg("--no-newline")
                    .arg("--type")
                    .arg(&mime)
                    .arg("--watch")
                    .arg(&exe);

                // Ensure wl-paste doesn't outlive us if our process dies abruptly (e.g. SIGKILL).
                // This prevents accumulating orphaned wl-paste processes in the background.
                unsafe {
                    cmd.pre_exec(|| {
                        // Best-effort: if unsupported, ignore.
                        // PR_SET_PDEATHSIG makes the kernel send SIGTERM to the child when the parent dies.
                        let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                        Ok(())
                    });
                }

                cmd.env("MCR_WL_WATCH_HOOK", "1")
                    .env("MCR_WATCH_CANDIDATE_MIME", &mime)
                    .env("MCR_STATE_DIR", state_dir.to_string_lossy().to_string())
                    .env("MCR_DEVICE_ID", device_id.clone())
                    .env("MCR_ROOM", room.clone())
                    .env("MCR_RELAY", relay.clone())
                    .env("MCR_MAX_TEXT_BYTES", max_text_bytes.to_string())
                    .env("MCR_MAX_IMAGE_BYTES", max_image_bytes.to_string())
                    .env("MCR_MAX_FILE_BYTES", max_file_bytes.to_string())
                    .envs(
                        debug_hook_path
                            .as_ref()
                            .map(|p| [("MCR_HOOK_DEBUG_PATH", p.as_str())])
                            .into_iter()
                            .flatten(),
                    )
                    .env(
                        "MCR_IMAGE_MODE",
                        match im {
                            ImageMode::Passthrough => "passthrough",
                            ImageMode::ForcePng => "force-png",
                            ImageMode::MultiMime => "multi",
                            ImageMode::SpoofPng => "spoof-png",
                        },
                    )
                    .kill_on_drop(true);

                let child = cmd.spawn();

                let mut child = match child {
                    Ok(c) => c,
                    Err(_) => {
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                };

                tokio::select! {
                    _ = stop_rx.changed() => {
                        let _ = child.kill().await;
                        break;
                    }
                    _ = child.wait() => {
                        // wl-paste exits if the requested type is not currently offered.
                        // We'll restart after a short backoff.
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                }
            }
        });
        handles.push(handle);
    }

    tokio::select! {
        _ = sigterm.recv() => {
            let _ = stop_tx.send(true);
        }
        _ = sigint.recv() => {
            let _ = stop_tx.send(true);
        }
        _ = tokio::signal::ctrl_c() => {
            let _ = stop_tx.send(true);
        }
    }

    // Best-effort: let tasks observe stop and exit.
    for h in handles {
        let _ = h.await;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn wl_watch_evented(
    ctx: &Ctx,
    room: &str,
    relay: &str,
    max_text_bytes: usize,
    max_image_bytes: usize,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    println!("wl-watch(watch): room='{}' relay='{}'", room, relay);

    let mut text_child = Command::new("wl-paste")
        .arg("--type")
        .arg("text/plain;charset=utf-8")
        .arg("--watch")
        .arg(&exe)
        .arg("--state-dir")
        .arg(&ctx.state_dir)
        .arg("--device-id")
        .arg(&ctx.device_id)
        .arg("wl-publish-current")
        .arg("--room")
        .arg(room)
        .arg("--relay")
        .arg(relay)
        .arg("--mime")
        .arg("text/plain;charset=utf-8")
        .arg("--max-text-bytes")
        .arg(max_text_bytes.to_string())
        .arg("--max-image-bytes")
        .arg(max_image_bytes.to_string())
        .kill_on_drop(true)
        .spawn()
        .context("spawn wl-paste watch text")?;

    let mut img_child = Command::new("wl-paste")
        .arg("--type")
        .arg("image/png")
        .arg("--watch")
        .arg(&exe)
        .arg("--state-dir")
        .arg(&ctx.state_dir)
        .arg("--device-id")
        .arg(&ctx.device_id)
        .arg("wl-publish-current")
        .arg("--room")
        .arg(room)
        .arg("--relay")
        .arg(relay)
        .arg("--mime")
        .arg("image/png")
        .arg("--max-text-bytes")
        .arg(max_text_bytes.to_string())
        .arg("--max-image-bytes")
        .arg(max_image_bytes.to_string())
        .kill_on_drop(true)
        .spawn()
        .context("spawn wl-paste watch image")?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            let _ = text_child.kill().await;
            let _ = img_child.kill().await;
        }
        status = text_child.wait() => {
            eprintln!("text watcher exited: {:?}", status);
            let _ = img_child.kill().await;
        }
        status = img_child.wait() => {
            eprintln!("image watcher exited: {:?}", status);
            let _ = text_child.kill().await;
        }
    }
    Ok(())
}

async fn wl_publish_current(
    ctx: &Ctx,
    room: &str,
    relay: &str,
    mime: &str,
    max_text_bytes: usize,
    max_image_bytes: usize,
    max_file_bytes: usize,
    image_mode: ImageMode,
) -> anyhow::Result<()> {
    // Auto mode: determine the best MIME to publish based on current offers.
    // This is used by wl-watch(watch) to stay robust even when the clipboard
    // does not currently offer a particular MIME type at startup.
    let mime = if mime == "auto" {
        let out = match Command::new("wl-paste").arg("--list-types").output().await {
            Ok(o) => o,
            Err(_) => return Ok(()),
        };
        let types = String::from_utf8_lossy(&out.stdout);
        let has = |m: &str| types.lines().any(|l| l.trim() == m);

        let chosen = if has(URI_LIST_MIME) {
            URI_LIST_MIME
        } else if has(KDE_URI_LIST_MIME) {
            KDE_URI_LIST_MIME
        } else if has(GNOME_COPIED_FILES_MIME) {
            GNOME_COPIED_FILES_MIME
        } else {
            // Prefer images when available.
            let choose_image = || {
                if image_mode == ImageMode::MultiMime {
                    // Prefer original formats over PNG when we can offer a PNG fallback.
                    for m in ["image/jpeg", "image/webp", "image/gif", "image/png"] {
                        if has(m) {
                            return Some(m);
                        }
                    }
                } else {
                    // Prefer PNG if present.
                    if has("image/png") {
                        return Some("image/png");
                    }
                    for m in ["image/jpeg", "image/webp", "image/gif"] {
                        if has(m) {
                            return Some(m);
                        }
                    }
                }
                None
            };

            if let Some(m) = choose_image() {
                m
            } else if has("text/plain;charset=utf-8") {
                "text/plain;charset=utf-8"
            } else if has("text/plain") {
                "text/plain"
            } else {
                return Ok(());
            }
        };

        // When invoked by wl-watch(watch), multiple MIME-specific watchers may fire.
        // Only allow the watcher whose candidate MIME matches our chosen best MIME.
        if let Ok(candidate) = std::env::var("MCR_WATCH_CANDIDATE_MIME") {
            if candidate != chosen {
                return Ok(());
            }
        }

        chosen
    } else {
        mime
    };

    // File selection: read uri-list and send file bytes.
    if mime == URI_LIST_MIME || mime == KDE_URI_LIST_MIME || mime == GNOME_COPIED_FILES_MIME {
        let list_bytes = match wl_paste(mime).await {
            Ok(b) => b,
            Err(_) => return Ok(()),
        };
        if list_bytes.is_empty() {
            return Ok(());
        }
        let paths = collect_clipboard_paths(&list_bytes);
        let mut uniq: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        for p in paths {
            uniq.insert(p);
        }
        let paths: Vec<PathBuf> = uniq.into_iter().collect();
        if paths.is_empty() {
            return Ok(());
        }

        // send (single file raw / multi or dir tar)
        let _ = send_paths_as_file(
            &ctx.state_dir,
            &ctx.device_id,
            room,
            relay,
            paths,
            max_file_bytes,
        )
        .await?;

        // Pause x11-sync text synchronization.
        pause_x11_text_sync(&ctx.state_dir, Duration::from_millis(2000)).await;

        // Same as hook/poll: avoid a follow-up text/plain `file:///...` overriding the receiver.
        set_suppress(
            &ctx.state_dir,
            room,
            "text/plain;charset=utf-8",
            "*",
            Duration::from_millis(1500),
        )
        .await;
        set_suppress(
            &ctx.state_dir,
            room,
            "text/plain",
            "*",
            Duration::from_millis(1500),
        )
        .await;
        return Ok(());
    }

    if mime == "image/png" && image_mode == ImageMode::MultiMime {
        // If the clipboard currently offers a non-png image type too, prefer publishing that one.
        // This avoids publishing both png and the original format when we locally set multi-mime.
        if let Ok(out) = Command::new("wl-paste").arg("--list-types").output().await {
            let types = String::from_utf8_lossy(&out.stdout);
            let has_other = image_mimes()
                .iter()
                .any(|m| *m != "image/png" && types.lines().any(|l| l.trim() == *m));
            if has_other {
                return Ok(());
            }
        }
    }

    let bytes = wl_paste(mime).await?;
    if bytes.is_empty() {
        return Ok(());
    }
    if mime.starts_with("text/") && bytes.len() > max_text_bytes {
        return Ok(());
    }
    if mime.starts_with("image/") && bytes.len() > max_image_bytes {
        return Ok(());
    }

    if mime.starts_with("text/plain") {
        let mut paths = collect_clipboard_paths(&bytes);
        if !paths.is_empty() {
            let mut existing: Vec<PathBuf> = Vec::new();
            for p in paths.drain(..) {
                if tokio::fs::metadata(&p).await.is_ok() {
                    existing.push(p);
                }
            }
            let mut uniq: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
            for p in existing {
                uniq.insert(p);
            }
            let existing: Vec<PathBuf> = uniq.into_iter().collect();
            if !existing.is_empty() {
                let _ = send_paths_as_file(
                    &ctx.state_dir,
                    &ctx.device_id,
                    room,
                    relay,
                    existing,
                    max_file_bytes,
                )
                .await?;

                // Pause x11-sync text synchronization.
                pause_x11_text_sync(&ctx.state_dir, Duration::from_millis(2000)).await;

                set_suppress(
                    &ctx.state_dir,
                    room,
                    "text/plain;charset=utf-8",
                    "*",
                    Duration::from_millis(1500),
                )
                .await;
                set_suppress(
                    &ctx.state_dir,
                    room,
                    "text/plain",
                    "*",
                    Duration::from_millis(1500),
                )
                .await;
                return Ok(());
            }
        }
    }

    let (send_mime, send_bytes) = if mime.starts_with("image/") {
        match image_mode {
            ImageMode::ForcePng => match to_png(&bytes) {
                Ok(png) => ("image/png", png),
                Err(_) => return Ok(()),
            },
            ImageMode::Passthrough | ImageMode::MultiMime | ImageMode::SpoofPng => (mime, bytes),
        }
    } else {
        (mime, bytes)
    };

    let sha = sha256_hex(&send_bytes);
    if is_suppressed(&ctx.state_dir, room, send_mime, &sha).await {
        return Ok(());
    }

    let stream = connect(relay).await?;
    let mut msg = if send_mime.starts_with("text/") {
        let mut m = Message::new_text(&ctx.device_id, room, "");
        m.payload = Some(send_bytes);
        m.size = m.payload.as_ref().map(|p| p.len()).unwrap_or(0);
        m
    } else {
        Message::new_image(&ctx.device_id, room, send_mime, send_bytes)
    };
    msg.sha256 = Some(sha);
    send_frame(stream, msg.to_bytes()).await?;
    record_send(
        &ctx.device_id,
        room,
        relay,
        msg.kind,
        Some(send_mime.to_string()),
        msg.name.clone(),
        msg.size,
        msg.sha256.clone(),
    )
    .await;
    Ok(())
}

async fn wl_apply(ctx: &Ctx, room: &str, relay: &str, image_mode: ImageMode) -> anyhow::Result<()> {
    // Guard against accidentally starting multiple appliers (which can cause confusing race-y clipboard behavior).
    let _lock = acquire_instance_lock(&ctx.state_dir, "wl-apply", room, relay)?;

    let stream = connect(relay).await?;
    let (mut reader, mut writer) = stream.into_split();

    send_join(&mut writer, &ctx.device_id, room).await?;
    println!("wl-apply: room='{}' relay='{}'", room, relay);

    // Simple loop-prevention: skip if we applied same sha recently.
    let mut last_applied_sha: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    loop {
        let len = match reader.read_u32().await {
            Ok(l) => l as usize,
            Err(_) => break,
        };
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await.context("read payload")?;
        let msg = Message::from_bytes(&buf);

        // don't apply our own
        if msg.device_id == ctx.device_id {
            continue;
        }
        if let Some(sha) = msg.sha256.as_deref() {
            let key = msg.mime.clone().unwrap_or_else(|| "(no-mime)".to_string());
            if last_applied_sha.get(&key).map(|s| s.as_str()) == Some(sha) {
                continue;
            }
        }

        match msg.kind {
            Kind::Text => {
                if let Some(payload) = msg.payload.as_deref() {
                    wl_copy("text/plain;charset=utf-8", payload).await.ok();
                    record_recv(&ctx.device_id, room, relay, &msg).await;
                    if let Some(sha) = msg.sha256.as_deref() {
                        set_suppress(
                            &ctx.state_dir,
                            room,
                            "text/plain;charset=utf-8",
                            sha,
                            Duration::from_secs(2),
                        )
                        .await;
                        last_applied_sha
                            .insert("text/plain;charset=utf-8".to_string(), sha.to_string());
                    }
                    println!("applied text ({} bytes)", payload.len());
                }
            }
            Kind::Image => {
                if let Some(payload) = msg.payload.as_deref() {
                    record_recv(&ctx.device_id, room, relay, &msg).await;
                    let mime = msg.mime.clone().unwrap_or_else(|| "image/png".to_string());
                    match image_mode {
                        ImageMode::ForcePng => {
                            let (apply_mime, apply_bytes) = match to_png(payload) {
                                Ok(png) => ("image/png".to_string(), png),
                                Err(_) => (mime.clone(), payload.to_vec()),
                            };
                            let _ = wl_copy(&apply_mime, &apply_bytes).await;
                            if let Some(sha) = msg.sha256.as_deref() {
                                set_suppress(
                                    &ctx.state_dir,
                                    room,
                                    &apply_mime,
                                    sha,
                                    Duration::from_secs(2),
                                )
                                .await;
                                last_applied_sha.insert(apply_mime.clone(), sha.to_string());
                            }
                            println!("applied {} ({} bytes)", apply_mime, apply_bytes.len());
                        }
                        ImageMode::Passthrough => {
                            let apply_mime = mime.clone();
                            let apply_bytes = payload.to_vec();
                            let _ = wl_copy(&apply_mime, &apply_bytes).await;
                            if let Some(sha) = msg.sha256.as_deref() {
                                set_suppress(
                                    &ctx.state_dir,
                                    room,
                                    &apply_mime,
                                    sha,
                                    Duration::from_secs(2),
                                )
                                .await;
                                last_applied_sha.insert(apply_mime.clone(), sha.to_string());
                            }
                            println!("applied {} ({} bytes)", apply_mime, apply_bytes.len());
                        }
                        ImageMode::MultiMime => {
                            // Offer both the original format and a PNG fallback (when possible).
                            if mime == "image/png" {
                                let apply_mime = mime.clone();
                                let apply_bytes = payload.to_vec();
                                let _ = wl_copy(&apply_mime, &apply_bytes).await;
                                if let Some(sha) = msg.sha256.as_deref() {
                                    set_suppress(
                                        &ctx.state_dir,
                                        room,
                                        &apply_mime,
                                        sha,
                                        Duration::from_secs(2),
                                    )
                                    .await;
                                    last_applied_sha.insert(apply_mime.clone(), sha.to_string());
                                }
                                println!("applied {} ({} bytes)", apply_mime, apply_bytes.len());
                            } else {
                                let orig_bytes = payload.to_vec();
                                let mut items = vec![(mime.clone(), orig_bytes.clone())];
                                let mut suppress_items: Vec<(String, String)> = Vec::new();

                                // Original sha
                                if let Some(sha) = msg.sha256.as_deref() {
                                    suppress_items.push((mime.clone(), sha.to_string()));
                                }

                                if let Ok(png) = to_png(&orig_bytes) {
                                    let png_sha = sha256_hex(&png);
                                    items.push(("image/png".to_string(), png));
                                    suppress_items.push(("image/png".to_string(), png_sha));
                                }

                                let _ = wl_copy_multi(items).await;
                                for (m, sha) in suppress_items {
                                    set_suppress(
                                        &ctx.state_dir,
                                        room,
                                        &m,
                                        &sha,
                                        Duration::from_secs(2),
                                    )
                                    .await;
                                    last_applied_sha.insert(m, sha);
                                }
                                println!("applied multi-mime {} (+png fallback)", mime);
                            }
                        }
                        ImageMode::SpoofPng => {
                            // Experimental / high-risk mode: declare image/png but serve the original bytes.
                            // Some applications may crash or hang if they trust the MIME type.
                            log::warn!(
                                "spoof-png: offering image/png with original payload mime={}",
                                mime
                            );

                            let apply_mime = "image/png".to_string();
                            let apply_bytes = payload.to_vec();
                            let _ = wl_copy(&apply_mime, &apply_bytes).await;

                            if let Some(sha) = msg.sha256.as_deref() {
                                set_suppress(
                                    &ctx.state_dir,
                                    room,
                                    &apply_mime,
                                    sha,
                                    Duration::from_secs(2),
                                )
                                .await;
                                last_applied_sha.insert(apply_mime.clone(), sha.to_string());
                            }
                            println!(
                                "applied spoof-png (orig {} bytes as image/png)",
                                apply_bytes.len()
                            );
                        }
                    }
                }
            }
            Kind::File => {
                let Some(payload) = msg.payload.as_deref() else {
                    continue;
                };
                record_recv(&ctx.device_id, room, relay, &msg).await;
                let sha = msg.sha256.clone().unwrap_or_else(|| sha256_hex(payload));
                if last_applied_sha.get(FILE_SUPPRESS_KEY).map(|s| s.as_str()) == Some(sha.as_str())
                {
                    continue;
                }

                let name = msg
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("multicliprelay-{}", &sha[..8]));
                let safe = safe_for_filename(&name);

                let dir = received_dir();
                tokio::fs::create_dir_all(&dir).await.ok();
                let sha8 = first_8(&sha).to_string();

                // If this is a tar bundle, extract into a directory and put that directory into the clipboard.
                if is_tar_payload(&name, msg.mime.as_deref()) {
                    // Prevent immediate feedback-loop: wl-apply writes file clipboard formats,
                    // which can trigger wl-watch almost instantly on the same machine.
                    // Use a short wildcard suppress window to ignore any file/text changes.
                    set_file_suppress(&ctx.state_dir, room, "*", Duration::from_millis(1500)).await;
                    set_suppress(
                        &ctx.state_dir,
                        room,
                        "text/plain;charset=utf-8",
                        "*",
                        Duration::from_millis(1500),
                    )
                    .await;
                    set_suppress(
                        &ctx.state_dir,
                        room,
                        "text/plain",
                        "*",
                        Duration::from_millis(1500),
                    )
                    .await;

                    let stem = safe
                        .trim_end_matches(".tar")
                        .trim_end_matches(".TAR")
                        .to_string();
                    let out_dir = dir.join(format!("{}_{}", sha8, stem));
                    tokio::fs::create_dir_all(&out_dir).await.ok();

                    // unpack in a blocking task
                    let out_dir2 = out_dir.clone();
                    let payload2 = payload.to_vec();
                    let _ =
                        tokio::task::spawn_blocking(move || unpack_tar_bytes(&payload2, &out_dir2))
                            .await;

                    // Ensure "copy folder" semantics across file managers:
                    // expose a *single root directory* in the clipboard.
                    // Some environments may copy a folder as its children (multiple top-level entries).
                    // In that case, we synthesize a wrapper folder and move entries into it.
                    let mut entries = node::transfer_file::list_top_level_items(&out_dir, 5000);

                    let sanitize_component = |s: &str| -> String {
                        let mut out: String = s
                            .chars()
                            .map(|c| match c {
                                '/' | '\\' | '\0' => '_',
                                _ => c,
                            })
                            .collect();
                        if out.is_empty() {
                            out = "multicliprelay".to_string();
                        }
                        if out == "." || out == ".." {
                            out = format!("_{}", out);
                        }
                        out
                    };

                    // Prefer the raw tar stem (preserves unicode) rather than `safe_for_filename`.
                    let stem_raw = name
                        .trim_end_matches(".tar")
                        .trim_end_matches(".TAR")
                        .to_string();
                    let wrapper_name = sanitize_component(&stem_raw);

                    // Generic names come from multi-selection without a clear folder intent.
                    // In that case we should preserve multi-item paste semantics (no wrapper).
                    let is_generic_bundle_name = stem_raw.starts_with("multicliprelay-bundle-");

                    async fn move_entry_best_effort(src: &std::path::PathBuf, dst: &std::path::PathBuf) {
                        if tokio::fs::rename(src, dst).await.is_ok() {
                            return;
                        }

                        let md = tokio::fs::metadata(src).await;
                        let Ok(md) = md else {
                            return;
                        };

                        if md.is_file() {
                            if tokio::fs::copy(src, dst).await.is_ok() {
                                let _ = tokio::fs::remove_file(src).await;
                            }
                            return;
                        }

                        if md.is_dir() {
                            let src2 = src.clone();
                            let dst2 = dst.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                use walkdir::WalkDir;
                                std::fs::create_dir_all(&dst2).ok();
                                for e in WalkDir::new(&src2).follow_links(false).into_iter().filter_map(|e| e.ok()) {
                                    let p = e.path();
                                    let Ok(rel) = p.strip_prefix(&src2) else { continue; };
                                    let target = dst2.join(rel);
                                    if e.file_type().is_dir() {
                                        std::fs::create_dir_all(&target).ok();
                                    } else if e.file_type().is_file() {
                                        if let Some(parent) = target.parent() {
                                            std::fs::create_dir_all(parent).ok();
                                        }
                                        std::fs::copy(p, &target).ok();
                                    }
                                }
                                std::fs::remove_dir_all(&src2).ok();
                            })
                            .await;
                        }
                    }

                    // Decide how to expose extracted content:
                    // - If it looks like a generic multi-item bundle, keep multi-item semantics.
                    // - Otherwise, prefer a single root directory (wrapper) for folder-copy semantics.
                    let (root_paths, root_name_for_plain) = if entries.is_empty() {
                        (vec![out_dir.clone()], wrapper_name.clone())
                    } else if entries.len() == 1 {
                        let p = entries[0].clone();
                        let n = p
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| wrapper_name.clone());
                        (vec![p], n)
                    } else if is_generic_bundle_name {
                        // Preserve multi-item paste (e.g. user copied multiple files).
                        let first_name = entries[0]
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("items")
                            .to_string();
                        (entries.clone(), first_name)
                    } else {
                        // Synthesize a wrapper folder and move top-level entries into it.
                        let wrapper = out_dir.join(&wrapper_name);
                        tokio::fs::create_dir_all(&wrapper).await.ok();

                        for src in entries.drain(..) {
                            if src == wrapper {
                                continue;
                            }
                            let Some(base) = src.file_name().map(|s| s.to_os_string()) else {
                                continue;
                            };
                            let mut dst = wrapper.join(&base);
                            if tokio::fs::metadata(&dst).await.is_ok() {
                                // Best-effort collision avoidance.
                                let b = base.to_string_lossy();
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis();
                                dst = wrapper.join(format!("{}_{}", b, ts));
                            }
                            move_entry_best_effort(&src, &dst).await;
                        }

                        (vec![wrapper], wrapper_name.clone())
                    };

                    // Clipboard payloads.
                    // - Always expose paths via uri-list.
                    // - Avoid providing large/ambiguous text/plain for file bundles to reduce
                    //   file manager paste oddities (esp. KDE/Dolphin). Keep marker mime.
                    let uri_list = build_uri_list(&root_paths);
                    let gnome_list = format!("copy\n{}", uri_list);

                    let items = vec![
                        (
                            "text/plain;charset=utf-8".to_string(),
                            root_name_for_plain.as_bytes().to_vec(),
                        ),
                        (
                            "text/plain".to_string(),
                            root_name_for_plain.as_bytes().to_vec(),
                        ),
                        (URI_LIST_MIME.to_string(), uri_list.as_bytes().to_vec()),
                        (
                            GNOME_COPIED_FILES_MIME.to_string(),
                            gnome_list.as_bytes().to_vec(),
                        ),
                        (
                            APPLIED_MARKER_MIME.to_string(),
                            format!(
                                "applied\nkind=tar\nsha={}\nname={}\nroot_hint={}\n",
                                sha,
                                name,
                                root_name_for_plain
                            )
                            .as_bytes()
                            .to_vec(),
                        ),
                    ];

                    let _ = wl_copy_multi(items).await;
                    println!(
                        "received bundle -> {} item(s) ({} bytes)",
                        root_paths.len(),
                        payload.len(),
                    );
                } else {
                    // Same feedback-loop guard for single-file payloads.
                    set_file_suppress(&ctx.state_dir, room, "*", Duration::from_millis(1500)).await;
                    set_suppress(
                        &ctx.state_dir,
                        room,
                        "text/plain;charset=utf-8",
                        "*",
                        Duration::from_millis(1500),
                    )
                    .await;
                    set_suppress(
                        &ctx.state_dir,
                        room,
                        "text/plain",
                        "*",
                        Duration::from_millis(1500),
                    )
                    .await;

                    // Store under a stable hash directory, but keep the original filename.
                    // This avoids "sha prefix" polluting the visible filename on the receiving side.
                    let out_dir = dir.join(&sha8);
                    tokio::fs::create_dir_all(&out_dir).await.ok();
                    let out_path = out_dir.join(&safe);
                    tokio::fs::write(&out_path, payload).await.ok();

                    // Write clipboard as file URI + plain path.
                    // NOTE: We can't preserve original remote paths; we point to the local received file.
                    let uri = build_uri_list(&vec![out_path.clone()]);
                    let plain = out_path.to_string_lossy().to_string();
                    let _ = wl_copy_multi(vec![
                        (
                            "text/plain;charset=utf-8".to_string(),
                            plain.as_bytes().to_vec(),
                        ),
                        (URI_LIST_MIME.to_string(), uri.as_bytes().to_vec()),
                        (
                            APPLIED_MARKER_MIME.to_string(),
                            format!("applied\nkind=file\nsha={}\nname={}\n", sha, name)
                                .as_bytes()
                                .to_vec(),
                        ),
                    ])
                    .await;
                    println!(
                        "received file -> {} ({} bytes)",
                        out_path.display(),
                        payload.len()
                    );
                }

                set_file_suppress(&ctx.state_dir, room, &sha, Duration::from_secs(2)).await;
                last_applied_sha.insert(FILE_SUPPRESS_KEY.to_string(), sha.clone());
            }
            Kind::Join => {}
        }
    }
    Ok(())
}

// Tests live in the dedicated modules (e.g. transfer_file).
