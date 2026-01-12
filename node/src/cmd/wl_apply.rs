use std::time::Duration;

use tokio::io::AsyncReadExt;

use utils::{Kind, Message};

use node::clipboard::{wl_copy, wl_copy_multi};
use node::consts::{APPLIED_MARKER_MIME, FILE_SUPPRESS_KEY, GNOME_COPIED_FILES_MIME, URI_LIST_MIME};
use node::hash::sha256_hex;
use node::history::record_recv;
use node::image_mode::ImageMode;
use node::net::{connect, send_join};
use node::paths::{first_8, is_tar_payload, received_dir, safe_for_filename};
use node::suppress::{set_file_suppress, set_suppress};
use node::transfer_file::{build_uri_list, unpack_tar_bytes};
use node::transfer_image::to_png;

pub(super) async fn run_wl_apply(
    ctx: &super::Ctx,
    room: &str,
    relay: &str,
    image_mode: ImageMode,
) -> anyhow::Result<()> {
    // Guard against accidentally starting multiple appliers (which can cause confusing race-y clipboard behavior).
    let _lock = super::acquire_instance_lock(&ctx.state_dir, "wl-apply", room, relay)?;

    // Heartbeat + reconnect:
    // - If the TCP connection drops, don't exit cleanly (systemd won't restart on exit 0).
    // - Periodically send Join as a lightweight heartbeat to keep NAT/stateful firewalls happy.
    let reconnect_backoff = Duration::from_millis(800);
    let heartbeat_interval = Duration::from_secs(20);

    // Simple loop-prevention: skip if we applied same sha recently.
    let mut last_applied_sha: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    loop {
        let stream = match connect(relay).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("wl-apply: connect failed: {e:?}");
                tokio::time::sleep(reconnect_backoff).await;
                continue;
            }
        };

        let (mut reader, mut writer) = stream.into_split();
        if let Err(e) = send_join(&mut writer, &ctx.device_id, room).await {
            log::warn!("wl-apply: send join failed: {e:?}");
            tokio::time::sleep(reconnect_backoff).await;
            continue;
        }
        println!("wl-apply: room='{}' relay='{}'", room, relay);

        let mut hb = tokio::time::interval(heartbeat_interval);
        hb.tick().await;

        loop {
            let len: usize = tokio::select! {
                _ = hb.tick() => {
                    if let Err(e) = send_join(&mut writer, &ctx.device_id, room).await {
                        log::warn!("wl-apply: heartbeat failed (will reconnect): {e:?}");
                        break;
                    }
                    continue;
                }
                res = reader.read_u32() => {
                    match res {
                        Ok(l) => l as usize,
                        Err(e) => {
                            log::warn!("wl-apply: read failed (will reconnect): {e:?}");
                            break;
                        }
                    }
                }
            };

            let mut buf = vec![0u8; len];
            if let Err(e) = reader.read_exact(&mut buf).await {
                log::warn!("wl-apply: read payload failed (will reconnect): {e:?}");
                break;
            }
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
                                    last_applied_sha
                                        .insert(apply_mime.clone(), sha.to_string());
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
                                    last_applied_sha
                                        .insert(apply_mime.clone(), sha.to_string());
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
                                        last_applied_sha
                                            .insert(apply_mime.clone(), sha.to_string());
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
                                    last_applied_sha
                                        .insert(apply_mime.clone(), sha.to_string());
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
                    if last_applied_sha
                        .get(FILE_SUPPRESS_KEY)
                        .map(|s| s.as_str())
                        == Some(sha.as_str())
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
                        let _ = tokio::task::spawn_blocking(move || unpack_tar_bytes(&payload2, &out_dir2)).await;

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

                        async fn move_entry_best_effort(
                            src: &std::path::PathBuf,
                            dst: &std::path::PathBuf,
                        ) {
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
                                    for e in WalkDir::new(&src2)
                                        .follow_links(false)
                                        .into_iter()
                                        .filter_map(|e| e.ok())
                                    {
                                        let p = e.path();
                                        let Ok(rel) = p.strip_prefix(&src2) else {
                                            continue;
                                        };
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
                                    sha, name, root_name_for_plain
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
                        println!("received file -> {} ({} bytes)", out_path.display(), payload.len());
                    }

                    set_file_suppress(&ctx.state_dir, room, &sha, Duration::from_secs(2)).await;
                    last_applied_sha.insert(FILE_SUPPRESS_KEY.to_string(), sha.clone());
                }
                Kind::Join => {}
            }
        }

        tokio::time::sleep(reconnect_backoff).await;
    }
}
