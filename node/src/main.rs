use anyhow::Context;
use clap::{Parser, Subcommand};
use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use std::io;

use utils::{Kind, Message};
use node::consts::{
    GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME, URI_LIST_MIME,
};
use node::hash::sha256_hex;
use node::history::record_send;
use node::image_mode::parse_image_mode;
use node::net::{connect, send_frame, send_join};
use node::paths::{default_state_dir, safe_for_filename};
use node::transfer_file::send_file;
use node::transfer_image::send_image;
use node::x11_sync::{x11_hook_apply_wayland_to_x11, x11_sync_service, X11SyncOpts};

#[path = "cmd/wl_apply.rs"]
mod cmd_wl_apply;

#[path = "cmd/wl_watch.rs"]
mod cmd_wl_watch;

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
        return cmd_wl_watch::wl_watch_hook().await;
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
            cmd_wl_watch::run_wl_watch(
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
            cmd_wl_apply::run_wl_apply(&ctx, &room, &relay, im).await?
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
            cmd_wl_watch::wl_publish_current(
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
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true);

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

            // Watch a set of common MIME types to robustly trigger on clipboard changes.
            // The hook does a full type scan, so duplicates are deduped via hashing.
            let _wl_text_u8 = spawn_watch("text/plain;charset=utf-8", "full")
                .context("spawn wl-paste text/plain;charset=utf-8 watch")?;
            let _wl_text = spawn_watch("text/plain", "full")
                .context("spawn wl-paste text/plain watch")?;
            let _wl_uri = spawn_watch(URI_LIST_MIME, "full")
                .context("spawn wl-paste text/uri-list watch")?;
            let _wl_kde = spawn_watch(KDE_URI_LIST_MIME, "full")
                .context("spawn wl-paste kde urilist watch")?;
            let _wl_gnome = spawn_watch(GNOME_COPIED_FILES_MIME, "full")
                .context("spawn wl-paste gnome copied-files watch")?;
            let _wl_png = spawn_watch("image/png", "full")
                .context("spawn wl-paste image/png watch")?;
            let _wl_jpg = spawn_watch("image/jpeg", "full")
                .context("spawn wl-paste image/jpeg watch")?;
            let _wl_webp = spawn_watch("image/webp", "full")
                .context("spawn wl-paste image/webp watch")?;
            let _wl_gif = spawn_watch("image/gif", "full")
                .context("spawn wl-paste image/gif watch")?;

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
            // IMPORTANT:
            // 这里不要阻塞式 read_to_end 等待 EOF。
            // wl-paste 在某些 clipboard provider / 大 payload 场景下可能会卡住，
            // 进而导致 x11-hook 长时间挂起并堆积多个进程，最后出现互相抢剪贴板/桌面假死。
            //
            // 对我们来说 x11-hook 只是“触发信号”，真正读取 Wayland 剪贴板发生在 x11-sync service 内。
            // 因此这里做一次“带超时的小读”即可（也能让 wl-paste 早点触发 SIGPIPE/结束本次传输）。

            let _cap = std::cmp::min(max_bytes, 64 * 1024);
            let mut tmp = [0u8; 4096];
            let mut sample: Vec<u8> = Vec::new();
            let read_res = tokio::time::timeout(Duration::from_millis(50), async {
                tokio::io::stdin().read(&mut tmp).await
            })
            .await;
            if let Ok(Ok(n)) = read_res {
                sample.extend_from_slice(&tmp[..n]);
            }

            x11_hook_apply_wayland_to_x11(&ctx.state_dir, &kind, sample).await;
        }
    }
    Ok(())
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
// Tests live in the dedicated modules (e.g. transfer_file).
