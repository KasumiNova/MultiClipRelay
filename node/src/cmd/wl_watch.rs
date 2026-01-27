use anyhow::Context;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;

use utils::{Kind, Message};

use node::clipboard::wl_paste;
use node::consts::{
    APPLIED_MARKER_MIME, GNOME_COPIED_FILES_MIME, KDE_URI_LIST_MIME, URI_LIST_MIME,
};
use node::hash::sha256_hex;
use node::history::record_send;
use node::image_mode::{parse_image_mode, ImageMode};
use node::net::{connect, send_frame, send_join};
use node::paths::{first_8, received_dir};
use node::suppress::{is_file_suppressed, is_suppressed, set_suppress};
use node::transfer_file::{collect_clipboard_paths, send_paths_as_file};
use node::transfer_image::{image_mimes, to_png};

fn image_ext_from_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/jpg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

async fn persist_image_best_effort(sha: &str, mime: &str, bytes: &[u8]) {
    let sha8 = first_8(sha).to_string();
    let dir = received_dir().join(&sha8);
    tokio::fs::create_dir_all(&dir).await.ok();
    let ext = image_ext_from_mime(mime).unwrap_or("bin");
    let p = dir.join(format!("image.{ext}"));
    let _ = tokio::fs::write(&p, bytes).await;
}

pub(super) async fn wl_watch_hook() -> anyhow::Result<()> {
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
        .unwrap_or_else(super::default_state_dir);
    tokio::fs::create_dir_all(&state_dir).await.ok();

    let device_id = std::env::var("MCR_DEVICE_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
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

    let device_name = super::default_device_name();
    let ctx = super::Ctx {
        state_dir,
        device_id,
        device_name,
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
            let _hook_lock = match super::acquire_instance_lock(&ctx.state_dir, "wl-watch-hook-file", &room, &relay) {
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
                &ctx.device_name,
                &room,
                &relay,
                paths,
                max_file_bytes,
            )
            .await?;

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
                        &ctx.device_name,
                        &room,
                        &relay,
                        existing,
                        max_file_bytes,
                    )
                    .await?;

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
                ImageMode::Passthrough | ImageMode::MultiMime | ImageMode::SpoofPng => (chosen, stored),
            }
        } else {
            (chosen, stored)
        };

        let sha = sha256_hex(&send_bytes);
        if is_suppressed(&ctx.state_dir, &room, send_mime, &sha).await {
            debug(&format!("hook: suppressed mime={} sha={}", send_mime, sha));
            return Ok(());
        }

        if send_mime.starts_with("image/") {
            persist_image_best_effort(&sha, send_mime, &send_bytes).await;
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
        if !ctx.device_name.trim().is_empty() {
            msg.sender_name = Some(ctx.device_name.clone());
        }
        msg.sha256 = Some(sha);
        if let Err(e) = send_frame(stream, msg.to_bytes()).await {
            debug(&format!("hook: send_frame failed: {:#}", e));
            return Ok(());
        }
        record_send(
            &ctx.device_id,
            Some(ctx.device_name.clone()),
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

pub(super) async fn run_wl_watch(
    ctx: &super::Ctx,
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
    let _lock = super::acquire_instance_lock(&ctx.state_dir, "wl-watch", room, relay)?;

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
    ctx: &super::Ctx,
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
    send_join(&mut writer, &ctx.device_id, &ctx.device_name, room).await?;
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
                    &ctx.device_name,
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
                            &ctx.device_name,
                            room,
                            relay,
                            existing,
                            max_file_bytes,
                        )
                        .await?
                        {
                            last_file_hash = Some(sha);
                        }

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
                    if !ctx.device_name.trim().is_empty() {
                        msg.sender_name = Some(ctx.device_name.clone());
                    }
                    msg.sha256 = Some(h.clone());
                    let buf = msg.to_bytes();
                    writer.write_u32(buf.len() as u32).await?;
                    writer.write_all(&buf).await?;
                    record_send(
                        &ctx.device_id,
                        Some(ctx.device_name.clone()),
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
                    persist_image_best_effort(&h, send_mime, &send_bytes).await;
                    let mut msg = Message::new_image(&ctx.device_id, room, send_mime, send_bytes);
                    if !ctx.device_name.trim().is_empty() {
                        msg.sender_name = Some(ctx.device_name.clone());
                    }
                    msg.sha256 = Some(h.clone());
                    let buf = msg.to_bytes();
                    writer.write_u32(buf.len() as u32).await?;
                    writer.write_all(&buf).await?;
                    record_send(
                        &ctx.device_id,
                        Some(ctx.device_name.clone()),
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
    ctx: &super::Ctx,
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
    ctx: &super::Ctx,
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

pub(super) async fn wl_publish_current(
    ctx: &super::Ctx,
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
            &ctx.device_name,
            room,
            relay,
            paths,
            max_file_bytes,
        )
        .await?;

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
                    &ctx.device_name,
                    room,
                    relay,
                    existing,
                    max_file_bytes,
                )
                .await?;

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

    if send_mime.starts_with("image/") {
        persist_image_best_effort(&sha, send_mime, &send_bytes).await;
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
    if !ctx.device_name.trim().is_empty() {
        msg.sender_name = Some(ctx.device_name.clone());
    }
    msg.sha256 = Some(sha);
    send_frame(stream, msg.to_bytes()).await?;
    record_send(
        &ctx.device_id,
        Some(ctx.device_name.clone()),
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
