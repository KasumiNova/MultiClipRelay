#![cfg(windows)]

use std::time::Duration;

use tokio::io::AsyncWriteExt;

use utils::{Kind, Message};

use node::dedup::{last_sent_get, last_sent_set};
use node::hash::sha256_hex;
use node::history::record_send;
use node::net::{connect, send_join};
use node::suppress::is_suppressed;
use node::suppress::set_suppress;
use node::transfer_file::send_paths_as_file;
use crate::win_clipboard;
use crate::win_image;

pub(super) async fn run_win_watch(
    ctx: &super::Ctx,
    room: &str,
    relay: &str,
    interval_ms: u64,
    max_text_bytes: usize,
    max_image_bytes: usize,
    max_file_bytes: usize,
) -> anyhow::Result<()> {
    // Same instance-lock policy as wl-watch: keep the system tidy.
    let _lock = super::acquire_instance_lock(&ctx.state_dir, "win-watch", room, relay)?;

    let reconnect_backoff = Duration::from_millis(800);
    let heartbeat_interval = Duration::from_secs(20);

    loop {
        let stream = match connect(relay).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("win-watch: connect failed: {e:?}");
                tokio::time::sleep(reconnect_backoff).await;
                continue;
            }
        };

        let (_reader, mut writer) = stream.into_split();
        if let Err(e) = send_join(&mut writer, &ctx.device_id, &ctx.device_name, room).await {
            log::warn!("win-watch: send join failed: {e:?}");
            tokio::time::sleep(reconnect_backoff).await;
            continue;
        }

        log::info!("win-watch: connected room='{}' relay='{}'", room, relay);
        println!("win-watch: room='{}' relay='{}'", room, relay);

        let mut hb = tokio::time::interval(heartbeat_interval);
        hb.tick().await;

        let mut last_seq = win_clipboard::clipboard_sequence();
        let mut last_text_hash: Option<String> = None;
        let mut last_img_hash: Option<String> = None;

        loop {
            tokio::select! {
                _ = hb.tick() => {
                    if let Err(e) = send_join(&mut writer, &ctx.device_id, &ctx.device_name, room).await {
                        log::warn!("win-watch: heartbeat failed (will reconnect): {e:?}");
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(interval_ms)) => {
                    let seq = win_clipboard::clipboard_sequence();
                    if seq == last_seq {
                        continue;
                    }
                    last_seq = seq;

                    // If we recently applied clipboard content from the relay, wl-apply on Linux
                    // writes a marker MIME. On Windows we mirror that using a custom clipboard format.
                    // Ignore changes while this marker exists to avoid feedback loops.
                    if win_clipboard::has_applied_marker() {
                        continue;
                    }

                    // Highest priority: file list (CF_HDROP).
                    if let Ok(Some(paths)) = win_clipboard::get_hdrop_paths() {
                        if !paths.is_empty() {
                            // quick de-dupe + stable order
                            let mut uniq: std::collections::BTreeSet<std::path::PathBuf> = std::collections::BTreeSet::new();
                            for p in paths {
                                uniq.insert(p);
                            }
                            let paths: Vec<std::path::PathBuf> = uniq.into_iter().collect();

                            if let Ok(Some(_sha)) = send_paths_as_file(
                                &ctx.state_dir,
                                &ctx.device_id,
                                &ctx.device_name,
                                room,
                                relay,
                                paths,
                                max_file_bytes,
                            ).await {
                                // Some apps also expose a text representation of file paths.
                                // Suppress text briefly to avoid overriding the receiver clipboard.
                                set_suppress(&ctx.state_dir, room, "text/plain;charset=utf-8", "*", Duration::from_millis(1500)).await;
                                set_suppress(&ctx.state_dir, room, "text/plain", "*", Duration::from_millis(1500)).await;
                                continue;
                            }
                        }
                    }

                    // Prefer image if present and changed.
                    if let Ok(Some(dib)) = win_clipboard::get_dib_bytes() {
                        if !dib.is_empty() {
                            if let Ok(png) = win_image::dib_to_png(&dib) {
                                if !png.is_empty() && png.len() <= max_image_bytes {
                                    let mime = "image/png";
                                    let sha = sha256_hex(&png);
                                    if !is_suppressed(&ctx.state_dir, room, mime, &sha).await
                                        && last_img_hash.as_deref() != Some(&sha)
                                    {
                                        if let Some(last) = last_sent_get(&ctx.state_dir, room, mime).await {
                                            if last == sha {
                                                last_img_hash = Some(sha);
                                                continue;
                                            }
                                        }

                                        let mut msg = Message::new_image(&ctx.device_id, room, mime, png);
                                        if !ctx.device_name.trim().is_empty() {
                                            msg.sender_name = Some(ctx.device_name.clone());
                                        }
                                        msg.sha256 = Some(sha.clone());
                                        let buf = msg.to_bytes();

                                        if let Err(e) = writer.write_u32(buf.len() as u32).await {
                                            log::warn!("win-watch: write len failed (will reconnect): {e:?}");
                                            break;
                                        }
                                        if let Err(e) = writer.write_all(&buf).await {
                                            log::warn!("win-watch: write payload failed (will reconnect): {e:?}");
                                            break;
                                        }

                                        record_send(
                                            &ctx.device_id,
                                            Some(ctx.device_name.clone()),
                                            room,
                                            relay,
                                            Kind::Image,
                                            Some(mime.to_string()),
                                            None,
                                            msg.size,
                                            Some(sha.clone()),
                                        ).await;

                                        last_sent_set(&ctx.state_dir, room, mime, &sha).await;
                                        last_img_hash = Some(sha);
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    let text = match win_clipboard::get_unicode_text() {
                        Ok(v) => v,
                        Err(e) => {
                            log::debug!("win-watch: get_unicode_text failed: {e:?}");
                            continue;
                        }
                    };

                    let Some(text) = text else {
                        continue;
                    };

                    if text.is_empty() {
                        continue;
                    }

                    let bytes = text.into_bytes();
                    if bytes.len() > max_text_bytes {
                        // Too big: ignore to avoid surprises.
                        continue;
                    }

                    let mime = "text/plain;charset=utf-8";
                    let sha = sha256_hex(&bytes);

                    // Avoid feedback loops: if we recently applied this sha, don't re-send.
                    if is_suppressed(&ctx.state_dir, room, mime, &sha).await {
                        continue;
                    }

                    // In-memory dedup: only send when the content changes.
                    if last_text_hash.as_deref() == Some(&sha) {
                        continue;
                    }

                    // Cross-process safety: if a short-lived helper ever appears later.
                    if let Some(last) = last_sent_get(&ctx.state_dir, room, mime).await {
                        if last == sha {
                            last_text_hash = Some(sha);
                            continue;
                        }
                    }

                    let mut msg = Message::new_text(&ctx.device_id, room, "");
                    msg.payload = Some(bytes);
                    msg.size = msg.payload.as_ref().map(|p| p.len()).unwrap_or(0);
                    if !ctx.device_name.trim().is_empty() {
                        msg.sender_name = Some(ctx.device_name.clone());
                    }
                    msg.sha256 = Some(sha.clone());

                    let buf = msg.to_bytes();
                    if let Err(e) = writer.write_u32(buf.len() as u32).await {
                        log::warn!("win-watch: write len failed (will reconnect): {e:?}");
                        break;
                    }
                    if let Err(e) = writer.write_all(&buf).await {
                        log::warn!("win-watch: write payload failed (will reconnect): {e:?}");
                        break;
                    }

                    record_send(
                        &ctx.device_id,
                        Some(ctx.device_name.clone()),
                        room,
                        relay,
                        Kind::Text,
                        Some(mime.to_string()),
                        None,
                        msg.size,
                        Some(sha.clone()),
                    ).await;

                    last_sent_set(&ctx.state_dir, room, mime, &sha).await;
                    last_text_hash = Some(sha);
                }
            }
        }

        tokio::time::sleep(reconnect_backoff).await;
    }
}
