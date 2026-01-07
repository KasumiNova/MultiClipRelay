use anyhow::Context;
use clap::{Parser, Subcommand};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use url::Url;
use walkdir::WalkDir;

use utils::{Kind, Message};

const FILE_SUPPRESS_KEY: &str = "application/x-cliprelay-file";
const URI_LIST_MIME: &str = "text/uri-list";
const GNOME_COPIED_FILES_MIME: &str = "x-special/gnome-copied-files";
const TAR_MIME: &str = "application/x-tar";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageMode {
    Passthrough,
    ForcePng,
    MultiMime,
    SpoofPng,
}

fn parse_image_mode(s: &str) -> anyhow::Result<ImageMode> {
    match s {
        "passthrough" => Ok(ImageMode::Passthrough),
        "force-png" => Ok(ImageMode::ForcePng),
        "multi" | "multi-mime" => Ok(ImageMode::MultiMime),
        "spoof-png" | "fake-png" => Ok(ImageMode::SpoofPng),
        other => anyhow::bail!(
            "invalid --image-mode {}, expected force-png|multi|passthrough|spoof-png",
            other
        ),
    }
}

#[derive(Clone, Debug)]
struct Ctx {
    state_dir: PathBuf,
    device_id: String,
}

#[derive(Parser)]
#[command(name = "clip-node")]
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Enable logging when the user sets RUST_LOG (kept quiet by default).
    // Useful for diagnosing clipboard edge cases.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();

    let cli = Cli::parse();

    let state_dir = cli
        .state_dir
        .unwrap_or_else(default_state_dir);
    tokio::fs::create_dir_all(&state_dir)
        .await
        .context("create state_dir")?;
    let device_id = match cli.device_id {
        Some(id) => id,
        None => get_or_create_device_id(&state_dir).await?,
    };
    let ctx = Ctx { state_dir, device_id };

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
            send_image(&ctx, &room, &file, &relay, max_bytes, im).await?
        }
        Commands::SendFile {
            room,
            file,
            relay,
            max_file_bytes,
        } => send_file(&ctx, &room, &file, &relay, max_file_bytes).await?,
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
        Commands::WlApply { room, relay, image_mode } => {
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
    }
    Ok(())
}

async fn connect(relay: &str) -> anyhow::Result<TcpStream> {
    let s = TcpStream::connect(relay).await.context("connect")?;
    Ok(s)
}

async fn send_frame(mut stream: TcpStream, buf: Vec<u8>) -> anyhow::Result<()> {
    stream.write_u32(buf.len() as u32).await.context("write len")?;
    stream.write_all(&buf).await.context("write payload")?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn default_state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("cliprelay");
    }
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/tmp/cliprelay-{}", uid))
}

fn safe_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

fn is_tar_payload(name: &str, mime: Option<&str>) -> bool {
    mime == Some(TAR_MIME) || name.to_ascii_lowercase().ends_with(".tar")
}

fn first_8(s: &str) -> &str {
    if s.len() >= 8 {
        &s[..8]
    } else {
        s
    }
}

fn default_data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(d).join("cliprelay");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/share/cliprelay");
    }
    // Last resort: state_dir should exist; fall back to /tmp.
    PathBuf::from("/tmp").join("cliprelay")
}

fn received_dir() -> PathBuf {
    default_data_dir().join("received")
}

fn history_path() -> PathBuf {
    default_data_dir().join("history.jsonl")
}

#[derive(Debug, Clone, Serialize)]
struct HistoryEvent {
    ts_ms: u64,
    dir: String,
    room: String,
    relay: String,
    local_device_id: String,
    remote_device_id: Option<String>,
    kind: String,
    mime: Option<String>,
    name: Option<String>,
    bytes: usize,
    sha256: Option<String>,
}

async fn append_history(event: HistoryEvent) {
    // Best-effort; never fail the main flow.
    let p = history_path();
    if let Some(parent) = p.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut f = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .await
    {
        Ok(v) => v,
        Err(_) => return,
    };

    let _ = f.write_all(line.as_bytes()).await;
    let _ = f.write_all(b"\n").await;
}

fn kind_to_string(k: &Kind) -> String {
    match k {
        Kind::Text => "text",
        Kind::Image => "image",
        Kind::File => "file",
        Kind::Join => "join",
    }
    .to_string()
}

async fn record_send(
    ctx: &Ctx,
    room: &str,
    relay: &str,
    kind: Kind,
    mime: Option<String>,
    name: Option<String>,
    bytes: usize,
    sha256: Option<String>,
) {
    append_history(HistoryEvent {
        ts_ms: utils::now_ms(),
        dir: "send".to_string(),
        room: room.to_string(),
        relay: relay.to_string(),
        local_device_id: ctx.device_id.clone(),
        remote_device_id: None,
        kind: kind_to_string(&kind),
        mime,
        name,
        bytes,
        sha256,
    })
    .await;
}

async fn record_recv(ctx: &Ctx, room: &str, relay: &str, msg: &Message) {
    append_history(HistoryEvent {
        ts_ms: utils::now_ms(),
        dir: "recv".to_string(),
        room: room.to_string(),
        relay: relay.to_string(),
        local_device_id: ctx.device_id.clone(),
        remote_device_id: Some(msg.device_id.clone()),
        kind: kind_to_string(&msg.kind),
        mime: msg.mime.clone(),
        name: msg.name.clone(),
        bytes: msg.payload.as_ref().map(|p| p.len()).unwrap_or(0),
        sha256: msg.sha256.clone(),
    })
    .await;
}

fn detect_file_mime(bytes: &[u8], file: &PathBuf) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_string();
    }
    // Extension-based minimal hints for common cases.
    let ext = file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" => "text/plain;charset=utf-8".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn parse_uri_list(bytes: &[u8]) -> Vec<Url> {
    let s = String::from_utf8_lossy(bytes);
    s.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with('#'))
        // gnome format starts with: "copy" or "cut"
        .filter(|l| *l != "copy" && *l != "cut")
        .filter_map(|l| Url::parse(l).ok())
        .collect()
}

fn collect_clipboard_paths(bytes: &[u8]) -> Vec<PathBuf> {
    parse_uri_list(bytes)
        .into_iter()
        .filter_map(|u| u.to_file_path().ok())
        .collect()
}

fn bundle_name_for(paths: &[PathBuf]) -> String {
    if paths.len() == 1 {
        if let Some(n) = paths[0].file_name().and_then(|s| s.to_str()) {
            return format!("{}.tar", n);
        }
    }
    format!("cliprelay-bundle-{}.tar", utils::now_ms())
}

fn build_tar_bundle(paths: &[PathBuf]) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());

    for p in paths {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item")
            .to_string();

        let md = std::fs::metadata(p)
            .with_context(|| format!("metadata {}", p.display()))?;
        if md.is_dir() {
            // Preserve the directory as a top-level folder in the archive.
            builder
                .append_dir_all(&name, p)
                .with_context(|| format!("append dir {}", p.display()))?;
        } else if md.is_file() {
            builder
                .append_path_with_name(p, &name)
                .with_context(|| format!("append file {}", p.display()))?;
        } else {
            // Skip symlinks/special files for safety.
            continue;
        }
    }

    let out = builder.into_inner().context("finish tar")?;
    Ok(out)
}

fn unpack_tar_bytes(bytes: &[u8], dest: &PathBuf) -> anyhow::Result<()> {
    let mut ar = tar::Archive::new(Cursor::new(bytes));
    for e in ar.entries().context("tar entries")? {
        let mut e = e.context("tar entry")?;
        // `unpack_in` defends against path traversal.
        e.unpack_in(dest).context("unpack_in")?;
    }
    Ok(())
}

fn build_uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        if let Ok(u) = Url::from_file_path(p) {
            out.push_str(u.as_str());
            out.push('\n');
        }
    }
    out
}

fn list_files_recursively(dir: &PathBuf, max_items: usize) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    if files.len() > max_items {
        files.truncate(max_items);
    }
    files
}

async fn send_file(ctx: &Ctx, room: &str, file: &PathBuf, relay: &str, max_file_bytes: usize) -> anyhow::Result<()> {
    let bytes = tokio::fs::read(file).await.context("read file")?;
    if bytes.len() > max_file_bytes {
        anyhow::bail!("file too large: {} bytes > {}", bytes.len(), max_file_bytes);
    }
    let name = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let mime = detect_file_mime(&bytes, file);
    let sha = sha256_hex(&bytes);

    let stream = connect(relay).await?;
    let mut msg = Message::new_file(&ctx.device_id, room, &name, &mime, bytes);
    msg.sha256 = Some(sha.clone());
    send_frame(stream, msg.to_bytes()).await?;
    record_send(
        ctx,
        room,
        relay,
        Kind::File,
        Some(mime),
        Some(name.clone()),
        msg.size,
        Some(sha),
    )
    .await;
    println!("sent file '{}' to room {}", name, room);
    Ok(())
}

async fn send_paths_as_file(ctx: &Ctx, room: &str, relay: &str, paths: Vec<PathBuf>, max_file_bytes: usize) -> anyhow::Result<Option<String>> {
    if paths.is_empty() {
        return Ok(None);
    }

    // Single regular file: send raw bytes (best compatibility).
    if paths.len() == 1 {
        let md = tokio::fs::metadata(&paths[0]).await;
        if let Ok(md) = md {
            if md.is_file() {
                let bytes = tokio::fs::read(&paths[0]).await.context("read file")?;
                if bytes.is_empty() || bytes.len() > max_file_bytes {
                    return Ok(None);
                }
                let sha = sha256_hex(&bytes);
                if is_suppressed(&ctx.state_dir, room, FILE_SUPPRESS_KEY, &sha).await {
                    return Ok(None);
                }
                let name = paths[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string();
                let mime = detect_file_mime(&bytes, &paths[0]);

                let stream = connect(relay).await?;
                let mut msg = Message::new_file(&ctx.device_id, room, &name, &mime, bytes);
                msg.sha256 = Some(sha.clone());
                send_frame(stream, msg.to_bytes()).await?;

                record_send(
                    ctx,
                    room,
                    relay,
                    Kind::File,
                    Some(mime),
                    Some(name),
                    msg.size,
                    Some(sha.clone()),
                )
                .await;
                return Ok(Some(sha));
            }
        }
    }

    // Multiple items or a directory: bundle into a tar.
    // Build tar in a blocking task (std::fs + tar builder).
    let paths2 = paths.clone();
    let tar_bytes = tokio::task::spawn_blocking(move || build_tar_bundle(&paths2))
        .await
        .context("tar build join")??;
    if tar_bytes.is_empty() || tar_bytes.len() > max_file_bytes {
        return Ok(None);
    }
    let sha = sha256_hex(&tar_bytes);
    if is_suppressed(&ctx.state_dir, room, FILE_SUPPRESS_KEY, &sha).await {
        return Ok(None);
    }
    let name = bundle_name_for(&paths);

    let stream = connect(relay).await?;
    let mut msg = Message::new_file(&ctx.device_id, room, &name, TAR_MIME, tar_bytes);
    msg.sha256 = Some(sha.clone());
    send_frame(stream, msg.to_bytes()).await?;

    record_send(
        ctx,
        room,
        relay,
        Kind::File,
        Some(TAR_MIME.to_string()),
        Some(name),
        msg.size,
        Some(sha.clone()),
    )
    .await;
    Ok(Some(sha))
}

#[cfg(unix)]
fn acquire_instance_lock(state_dir: &PathBuf, name: &str, room: &str, relay: &str) -> anyhow::Result<File> {
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
fn acquire_instance_lock(_state_dir: &PathBuf, _name: &str, _room: &str, _relay: &str) -> anyhow::Result<()> {
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

fn suppress_path(state_dir: &PathBuf, room: &str, mime: &str) -> PathBuf {
    // include room to allow multiple rooms on same machine
    let safe_room = room.replace('/', "_");
    let safe_mime = mime.replace('/', "_").replace(';', "_").replace('=', "_");
    state_dir.join(format!("suppress_{}_{}", safe_room, safe_mime))
}

async fn set_suppress(state_dir: &PathBuf, room: &str, mime: &str, sha: &str, ttl: Duration) {
    let expires = utils::now_ms().saturating_add(ttl.as_millis() as u64);
    let p = suppress_path(state_dir, room, mime);
    let _ = tokio::fs::write(p, format!("{}\n{}\n", sha, expires)).await;
}

async fn is_suppressed(state_dir: &PathBuf, room: &str, mime: &str, sha: &str) -> bool {
    let p = suppress_path(state_dir, room, mime);
    let s = match tokio::fs::read_to_string(p).await {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut it = s.lines();
    let ssha = it.next().unwrap_or("").trim();
    let sexp = it.next().unwrap_or("0").trim();
    if ssha != sha {
        return false;
    }
    let exp: u64 = sexp.parse().unwrap_or(0);
    utils::now_ms() <= exp
}

async fn wl_paste(mime: &str) -> anyhow::Result<Vec<u8>> {
    // wl-paste exits non-zero if the requested type is unavailable.
    let out = Command::new("wl-paste")
        .arg("--no-newline")
        .arg("--type")
        .arg(mime)
        .output()
        .await
        .context("spawn wl-paste")?;
    if !out.status.success() {
        anyhow::bail!("wl-paste unavailable: {}", mime);
    }
    Ok(out.stdout)
}

async fn wl_copy(mime: &str, bytes: &[u8]) -> anyhow::Result<()> {
    wl_copy_multi(vec![(mime.to_string(), bytes.to_vec())]).await
}

async fn wl_copy_multi(items: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        use wl_clipboard_rs::copy::{ClipboardType, Error as WlCopyError, MimeSource, MimeType, Options, Seat, Source};

        let mk_sources = |items: &[(String, Vec<u8>)]| -> Vec<MimeSource> {
            items
                .iter()
                .map(|(mime, bytes)| MimeSource {
                    source: Source::Bytes(bytes.clone().into_boxed_slice()),
                    mime_type: MimeType::Specific(mime.clone()),
                })
                .collect()
        };

        let sources = mk_sources(&items);

        // Practical note:
        // - Setting images to PRIMARY can confuse some toolchains / bridges.
        // - Many apps only use the regular clipboard for paste.
        // So we only set BOTH for text, and use Regular-only for non-text payloads.
        let want_both = items.iter().any(|(mime, _)| mime.starts_with("text/"));
        let clipboard = if want_both { ClipboardType::Both } else { ClipboardType::Regular };

        let mut opts = Options::new();
        opts.clipboard(clipboard).seat(Seat::All);

        match opts.copy_multi(sources.clone()) {
            Ok(()) => Ok(()),
            Err(WlCopyError::PrimarySelectionUnsupported) if want_both => {
                // Fallback: regular clipboard only.
                let mut opts = Options::new();
                opts.clipboard(ClipboardType::Regular).seat(Seat::All);
                opts.copy_multi(sources).map_err(|e| anyhow::anyhow!(e))
            }
            Err(e) => Err(anyhow::anyhow!(e)),
        }
    })
    .await
    .context("wl_copy_multi join")??;
    Ok(())
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
    tokio::spawn(async move { let _ = read_loop(reader).await; });
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
        ctx,
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

fn image_mimes() -> &'static [&'static str] {
    &[
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/gif",
    ]
}

fn detect_image_mime(bytes: &[u8], file: &PathBuf) -> anyhow::Result<String> {
    // Prefer content sniffing.
    if let Some(kind) = infer::get(bytes) {
        let mime = kind.mime_type();
        if mime.starts_with("image/") {
            return Ok(mime.to_string());
        }
    }
    // Fallback: extension guess.
    let ext = file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => anyhow::bail!("unsupported image type (cannot detect mime): {}", file.display()),
    };
    Ok(mime.to_string())
}

fn to_png(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Cursor;
    let img = image::load_from_memory(bytes).context("decode image")?;
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .context("encode png")?;
    Ok(out)
}

async fn send_image(
    ctx: &Ctx,
    room: &str,
    file: &PathBuf,
    relay: &str,
    max_bytes: usize,
    image_mode: ImageMode,
) -> anyhow::Result<()> {
    let bytes = tokio::fs::read(file).await.context("read image")?;
    if bytes.len() > max_bytes {
        anyhow::bail!("image too large: {} bytes > {}", bytes.len(), max_bytes);
    }
    let mime = detect_image_mime(&bytes, file)?;
    if !image_mimes().iter().any(|m| *m == mime) {
        anyhow::bail!("unsupported image mime {}", mime);
    }

    let (send_mime, send_bytes) = match image_mode {
        ImageMode::Passthrough | ImageMode::MultiMime | ImageMode::SpoofPng => (mime.as_str(), bytes),
        ImageMode::ForcePng => ("image/png", to_png(&bytes)?),
    };
    let stream = connect(relay).await?;
    let mut msg = Message::new_image(&ctx.device_id, room, send_mime, send_bytes);
    let sha = sha256_hex(msg.payload.as_deref().unwrap_or_default());
    msg.sha256 = Some(sha.clone());
    let buf = msg.to_bytes();
    send_frame(stream, buf).await?;
    record_send(
        ctx,
        room,
        relay,
        Kind::Image,
        Some(send_mime.to_string()),
        file.file_name().and_then(|s| s.to_str()).map(|s| s.to_string()),
        msg.size,
        Some(sha),
    )
    .await;
    println!("sent image to room {}", room);
    Ok(())
}

async fn send_join(writer: &mut tokio::net::tcp::OwnedWriteHalf, device_id: &str, room: &str) -> anyhow::Result<()> {
    let join = Message::new_join(device_id, room).to_bytes();
    writer
        .write_u32(join.len() as u32)
        .await
        .context("write join len")?;
    writer.write_all(&join).await.context("write join")?;
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
        "watch" => wl_watch_evented(ctx, room, relay, max_text_bytes, max_image_bytes, max_file_bytes, image_mode).await,
        "poll" => wl_watch_poll(ctx, room, relay, interval_ms, max_text_bytes, max_image_bytes, max_file_bytes, image_mode).await,
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
    let mut last_img_hash: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut last_file_hash: Option<String> = None;

    loop {
        // text/plain
        if let Ok(text_bytes) = wl_paste("text/plain;charset=utf-8").await {
            if !text_bytes.is_empty() && text_bytes.len() <= max_text_bytes {
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
                        ctx,
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
                        ctx,
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

        // files (text/uri-list / gnome)
        let mut list_bytes: Option<Vec<u8>> = None;
        if let Ok(b) = wl_paste(URI_LIST_MIME).await {
            if !b.is_empty() {
                list_bytes = Some(b);
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
                if let Some(sha) = send_paths_as_file(ctx, room, relay, paths, max_file_bytes).await? {
                    Some(sha)
                } else {
                    None
                }
            };

            if let Some(sha) = maybe_sha {
                if last_file_hash.as_deref() != Some(&sha)
                    && !is_suppressed(&ctx.state_dir, room, FILE_SUPPRESS_KEY, &sha).await
                {
                    last_file_hash = Some(sha);
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

    // Spawn watchers. On each change, wl-paste runs our binary to publish current content.
    let mut children: Vec<tokio::process::Child> = Vec::new();

    let text_child = Command::new("wl-paste")
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
        .arg("--image-mode")
        .arg(match image_mode {
            ImageMode::Passthrough => "passthrough",
            ImageMode::ForcePng => "force-png",
            ImageMode::MultiMime => "multi",
            ImageMode::SpoofPng => "spoof-png",
        })
        .kill_on_drop(true)
        .spawn()
        .context("spawn wl-paste watch text")?;

    children.push(text_child);

    // files
    for &mime in [URI_LIST_MIME, GNOME_COPIED_FILES_MIME].iter() {
        let c = Command::new("wl-paste")
            .arg("--type")
            .arg(mime)
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
            .arg(mime)
            .arg("--max-text-bytes")
            .arg(max_text_bytes.to_string())
            .arg("--max-image-bytes")
            .arg(max_image_bytes.to_string())
            .arg("--max-file-bytes")
            .arg(max_file_bytes.to_string())
            .arg("--image-mode")
            .arg(match image_mode {
                ImageMode::Passthrough => "passthrough",
                ImageMode::ForcePng => "force-png",
                ImageMode::MultiMime => "multi",
                ImageMode::SpoofPng => "spoof-png",
            })
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn wl-paste watch {mime}"))?;
        children.push(c);
    }

    for &mime in image_mimes().iter() {
        let c = Command::new("wl-paste")
            .arg("--type")
            .arg(mime)
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
            .arg(mime)
            .arg("--max-text-bytes")
            .arg(max_text_bytes.to_string())
            .arg("--max-image-bytes")
            .arg(max_image_bytes.to_string())
            .arg("--max-file-bytes")
            .arg(max_file_bytes.to_string())
            .arg("--image-mode")
            .arg(match image_mode {
                ImageMode::Passthrough => "passthrough",
                ImageMode::ForcePng => "force-png",
                ImageMode::MultiMime => "multi",
                ImageMode::SpoofPng => "spoof-png",
            })
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn wl-paste watch {mime}"))?;
        children.push(c);
    }

    tokio::select! {
        _ = sigterm.recv() => {
            for c in children.iter_mut() { let _ = c.kill().await; }
        }
        _ = sigint.recv() => {
            for c in children.iter_mut() { let _ = c.kill().await; }
        }
        _ = tokio::signal::ctrl_c() => {
            for c in children.iter_mut() { let _ = c.kill().await; }
        }
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
    // File selection: read uri-list and send file bytes.
    if mime == URI_LIST_MIME || mime == GNOME_COPIED_FILES_MIME {
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
        let _ = send_paths_as_file(ctx, room, relay, paths, max_file_bytes).await?;
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
        ctx,
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
    let mut last_applied_sha: std::collections::HashMap<String, String> = std::collections::HashMap::new();

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
            let key = msg
                .mime
                .clone()
                .unwrap_or_else(|| "(no-mime)".to_string());
            if last_applied_sha.get(&key).map(|s| s.as_str()) == Some(sha) {
                continue;
            }
        }

        match msg.kind {
            Kind::Text => {
                if let Some(payload) = msg.payload.as_deref() {
                    wl_copy("text/plain;charset=utf-8", payload).await.ok();
                    record_recv(ctx, room, relay, &msg).await;
                    if let Some(sha) = msg.sha256.as_deref() {
                        set_suppress(&ctx.state_dir, room, "text/plain;charset=utf-8", sha, Duration::from_secs(2)).await;
                        last_applied_sha.insert("text/plain;charset=utf-8".to_string(), sha.to_string());
                    }
                    println!("applied text ({} bytes)", payload.len());
                }
            }
            Kind::Image => {
                if let Some(payload) = msg.payload.as_deref() {
                    record_recv(ctx, room, relay, &msg).await;
                    let mime = msg.mime.clone().unwrap_or_else(|| "image/png".to_string());
                    match image_mode {
                        ImageMode::ForcePng => {
                            let (apply_mime, apply_bytes) = match to_png(payload) {
                                Ok(png) => ("image/png".to_string(), png),
                                Err(_) => (mime.clone(), payload.to_vec()),
                            };
                            let _ = wl_copy(&apply_mime, &apply_bytes).await;
                            if let Some(sha) = msg.sha256.as_deref() {
                                set_suppress(&ctx.state_dir, room, &apply_mime, sha, Duration::from_secs(2)).await;
                                last_applied_sha.insert(apply_mime.clone(), sha.to_string());
                            }
                            println!("applied {} ({} bytes)", apply_mime, apply_bytes.len());
                        }
                        ImageMode::Passthrough => {
                            let apply_mime = mime.clone();
                            let apply_bytes = payload.to_vec();
                            let _ = wl_copy(&apply_mime, &apply_bytes).await;
                            if let Some(sha) = msg.sha256.as_deref() {
                                set_suppress(&ctx.state_dir, room, &apply_mime, sha, Duration::from_secs(2)).await;
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
                                    set_suppress(&ctx.state_dir, room, &apply_mime, sha, Duration::from_secs(2)).await;
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
                                    set_suppress(&ctx.state_dir, room, &m, &sha, Duration::from_secs(2)).await;
                                    last_applied_sha.insert(m, sha);
                                }
                                println!("applied multi-mime {} (+png fallback)", mime);
                            }
                        }
                        ImageMode::SpoofPng => {
                            // Experimental / high-risk mode: declare image/png but serve the original bytes.
                            // Some applications may crash or hang if they trust the MIME type.
                            log::warn!("spoof-png: offering image/png with original payload mime={}", mime);

                            let apply_mime = "image/png".to_string();
                            let apply_bytes = payload.to_vec();
                            let _ = wl_copy(&apply_mime, &apply_bytes).await;

                            if let Some(sha) = msg.sha256.as_deref() {
                                set_suppress(&ctx.state_dir, room, &apply_mime, sha, Duration::from_secs(2)).await;
                                last_applied_sha.insert(apply_mime.clone(), sha.to_string());
                            }
                            println!("applied spoof-png (orig {} bytes as image/png)", apply_bytes.len());
                        }
                    }
                }
            }
            Kind::File => {
                let Some(payload) = msg.payload.as_deref() else { continue; };
                record_recv(ctx, room, relay, &msg).await;
                let sha = msg.sha256.clone().unwrap_or_else(|| sha256_hex(payload));
                if last_applied_sha.get(FILE_SUPPRESS_KEY).map(|s| s.as_str()) == Some(sha.as_str()) {
                    continue;
                }

                let name = msg
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("cliprelay-{}", &sha[..8]));
                let safe = safe_for_filename(&name);

                let dir = received_dir();
                tokio::fs::create_dir_all(&dir).await.ok();
                let sha8 = first_8(&sha).to_string();

                // If this is a tar bundle, extract into a directory and put that directory into the clipboard.
                if is_tar_payload(&name, msg.mime.as_deref()) {
                    let stem = safe
                        .trim_end_matches(".tar")
                        .trim_end_matches(".TAR")
                        .to_string();
                    let out_dir = dir.join(format!("{}_{}", sha8, stem));
                    tokio::fs::create_dir_all(&out_dir).await.ok();

                    // unpack in a blocking task
                    let out_dir2 = out_dir.clone();
                    let payload2 = payload.to_vec();
                    let _ = tokio::task::spawn_blocking(move || unpack_tar_bytes(&payload2, &out_dir2)).await;

                    // Multi-file compatibility: publish individual file URIs.
                    // Limit to avoid gigantic clipboard payloads.
                    let files = list_files_recursively(&out_dir, 200);
                    let uri_list = build_uri_list(&files);
                    let gnome_list = format!("copy\n{}", uri_list);
                    let plain_lines: String = files
                        .iter()
                        .take(20)
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let plain = if plain_lines.is_empty() {
                        out_dir.to_string_lossy().to_string()
                    } else {
                        plain_lines
                    };

                    let mut items = vec![
                        ("text/plain;charset=utf-8".to_string(), plain.as_bytes().to_vec()),
                        (URI_LIST_MIME.to_string(), uri_list.as_bytes().to_vec()),
                        (GNOME_COPIED_FILES_MIME.to_string(), gnome_list.as_bytes().to_vec()),
                    ];
                    // If no files were extracted, fall back to directory uri.
                    if files.is_empty() {
                        let uri = format!("file://{}/\n", out_dir.to_string_lossy());
                        items[1].1 = uri.as_bytes().to_vec();
                        items[2].1 = format!("copy\n{}", uri).as_bytes().to_vec();
                    }

                    let _ = wl_copy_multi(items).await;
                    println!("received bundle -> {} ({} bytes, {} files)", out_dir.display(), payload.len(), files.len());
                } else {
                    let out_path = dir.join(format!("{}_{}", sha8, safe));
                    tokio::fs::write(&out_path, payload).await.ok();

                    // Write clipboard as file URI + plain path.
                    // NOTE: We can't preserve original remote paths; we point to the local received file.
                    let uri = format!("file://{}\n", out_path.to_string_lossy());
                    let plain = out_path.to_string_lossy().to_string();
                    let _ = wl_copy_multi(vec![
                        ("text/plain;charset=utf-8".to_string(), plain.as_bytes().to_vec()),
                        (URI_LIST_MIME.to_string(), uri.as_bytes().to_vec()),
                    ])
                    .await;
                    println!("received file -> {} ({} bytes)", out_path.display(), payload.len());
                }

                set_suppress(&ctx.state_dir, room, FILE_SUPPRESS_KEY, &sha, Duration::from_secs(2)).await;
                last_applied_sha.insert(FILE_SUPPRESS_KEY.to_string(), sha.clone());
            }
            Kind::Join => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uri_list_ignores_comments_and_gnome_prefix() {
        let s = b"# comment\ncopy\nfile:///tmp/a.txt\n\nfile:///tmp/b.txt\n";
        let urls = parse_uri_list(s);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].scheme(), "file");
    }

    #[test]
    fn tar_bundle_roundtrip_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let b = sub.join("b.txt");
        std::fs::write(&a, b"hello").unwrap();
        std::fs::write(&b, b"world").unwrap();

        let tar = build_tar_bundle(&vec![a.clone(), sub.clone()]).unwrap();
        assert!(!tar.is_empty());

        let out = tempfile::tempdir().unwrap();
        unpack_tar_bytes(&tar, &out.path().to_path_buf()).unwrap();

        // a.txt should exist; sub/b.txt should exist (directory preserved).
        assert!(out.path().join("a.txt").exists());
        assert!(out.path().join("sub").join("b.txt").exists());
    }
}
