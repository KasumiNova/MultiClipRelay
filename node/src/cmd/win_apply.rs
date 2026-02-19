#![cfg(windows)]

use anyhow::Context;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use utils::{Kind, Message};

use node::hash::sha256_hex;
use node::history::record_recv;
use node::net::{connect, send_join};
use node::suppress::{set_file_suppress, set_suppress};
use node::transfer_file::{list_top_level_items, unpack_tar_bytes};
use node::paths::{first_8, received_dir};
use node::consts::FILE_SUPPRESS_KEY;
use crate::win_clipboard;
use crate::win_image;

pub(super) async fn run_win_apply(
    ctx: &super::Ctx,
    room: &str,
    relay: &str,
) -> anyhow::Result<()> {
    // Guard against multiple appliers.
    let _lock = super::acquire_instance_lock(&ctx.state_dir, "win-apply", room, relay)?;

    let reconnect_backoff = Duration::from_millis(800);
    let heartbeat_interval = Duration::from_secs(20);

    let mut last_applied_sha: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    loop {
        let stream = match connect(relay).await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("win-apply: connect failed: {e:?}");
                tokio::time::sleep(reconnect_backoff).await;
                continue;
            }
        };

        let (mut reader, mut writer) = stream.into_split();
        if let Err(e) = send_join(&mut writer, &ctx.device_id, &ctx.device_name, room).await {
            log::warn!("win-apply: send join failed: {e:?}");
            tokio::time::sleep(reconnect_backoff).await;
            continue;
        }

        log::info!("win-apply: connected room='{}' relay='{}'", room, relay);
        println!("win-apply: room='{}' relay='{}'", room, relay);

        let mut hb = tokio::time::interval(heartbeat_interval);
        hb.tick().await;

        loop {
            let len: usize = tokio::select! {
                _ = hb.tick() => {
                    if let Err(e) = send_join(&mut writer, &ctx.device_id, &ctx.device_name, room).await {
                        log::warn!("win-apply: heartbeat failed (will reconnect): {e:?}");
                        break;
                    }
                    continue;
                }
                res = reader.read_u32() => {
                    match res {
                        Ok(l) => l as usize,
                        Err(e) => {
                            log::warn!("win-apply: read failed (will reconnect): {e:?}");
                            break;
                        }
                    }
                }
            };

            let mut buf = vec![0u8; len];
            if let Err(e) = reader.read_exact(&mut buf).await {
                log::warn!("win-apply: read payload failed (will reconnect): {e:?}");
                break;
            }

            let msg = match Message::try_from_bytes(&buf) {
                Ok(m) => m,
                Err(e) => {
                    let prefix_len = buf.len().min(16);
                    log::warn!(
                        "win-apply: decode failed (will reconnect): len={} prefix={:02x?} err={:?}",
                        len,
                        &buf[..prefix_len],
                        e
                    );
                    break;
                }
            };

            // don't apply our own
            if msg.device_id == ctx.device_id {
                continue;
            }

            // loop prevention: skip if we applied same sha recently
            if let Some(sha) = msg.sha256.as_deref() {
                let key = msg.mime.clone().unwrap_or_else(|| "(no-mime)".to_string());
                if last_applied_sha.get(&key).map(|s| s.as_str()) == Some(sha) {
                    continue;
                }
            }

            match msg.kind {
                Kind::Text => {
                    let Some(payload) = msg.payload.as_deref() else { continue; };
                    let text = String::from_utf8_lossy(payload).to_string();

                    let mime = "text/plain;charset=utf-8";
                    let sha = msg.sha256.clone().unwrap_or_else(|| sha256_hex(payload));

                    // Write clipboard with an applied-marker format to prevent feedback loops.
                    // Marker payload: sha (ascii).
                    let _ = win_clipboard::set_unicode_text_with_applied_marker(&text, sha.as_bytes());

                    record_recv(&ctx.device_id, Some(ctx.device_name.clone()), room, relay, &msg).await;
                    set_suppress(&ctx.state_dir, room, mime, &sha, Duration::from_secs(2)).await;
                    last_applied_sha.insert(mime.to_string(), sha);

                    println!("applied text ({} bytes)", payload.len());
                }
                Kind::Image => {
                    let Some(payload) = msg.payload.as_deref() else { continue; };

                    let mime = msg.mime.clone().unwrap_or_else(|| "image/png".to_string());
                    let sha = msg.sha256.clone().unwrap_or_else(|| sha256_hex(payload));

                    // Decode the incoming image and write to Windows clipboard as DIBV5.
                    match win_image::bytes_to_dibv5(payload) {
                        Ok(dibv5) => {
                            let _ = win_clipboard::set_dibv5_with_applied_marker(&dibv5, sha.as_bytes());
                            record_recv(&ctx.device_id, Some(ctx.device_name.clone()), room, relay, &msg).await;
                            // Keep suppress for extra safety (encoding roundtrips can differ).
                            set_suppress(&ctx.state_dir, room, "image/png", &sha, Duration::from_secs(2)).await;
                            last_applied_sha.insert(mime, sha);
                            println!("applied image ({} bytes)", payload.len());
                        }
                        Err(e) => {
                            log::warn!("win-apply: decode image failed: {e:?}");
                        }
                    }
                }
                Kind::File => {
                    let Some(payload) = msg.payload.as_deref() else { continue; };

                    let sha = msg.sha256.clone().unwrap_or_else(|| sha256_hex(payload));
                    let sha8 = first_8(&sha).to_string();
                    let out_dir = received_dir().join(&sha8);
                    tokio::fs::create_dir_all(&out_dir).await.ok();

                    // Feedback-loop guard: suppress file + text briefly, then write CF_HDROP.
                    set_file_suppress(&ctx.state_dir, room, "*", Duration::from_millis(1500)).await;
                    set_suppress(&ctx.state_dir, room, "text/plain;charset=utf-8", "*", Duration::from_millis(1500)).await;
                    set_suppress(&ctx.state_dir, room, "text/plain", "*", Duration::from_millis(1500)).await;

                    let out_dir2 = out_dir.clone();
                    let tar_bytes = payload.to_vec();
                    let unpack_res: anyhow::Result<()> = tokio::task::spawn_blocking(move || {
                        unpack_tar_bytes(&tar_bytes, &out_dir2)
                    })
                    .await
                    .context("tar unpack join")?;
                    if let Err(e) = unpack_res {
                        log::warn!("win-apply: unpack tar failed: {e:?}");
                        continue;
                    }

                    let mut items = list_top_level_items(&out_dir, 4096);
                    if items.is_empty() {
                        items.push(out_dir.clone());
                    }

                    let _ = win_clipboard::set_hdrop_paths_with_applied_marker(&items, sha.as_bytes());

                    record_recv(&ctx.device_id, Some(ctx.device_name.clone()), room, relay, &msg).await;
                    set_file_suppress(&ctx.state_dir, room, &sha, Duration::from_secs(2)).await;
                    last_applied_sha.insert(FILE_SUPPRESS_KEY.to_string(), sha);

                    println!("applied file bundle -> {} item(s) ({} bytes)", items.len(), payload.len());
                }

                Kind::Join => {}
            }
        }

        tokio::time::sleep(reconnect_backoff).await;
    }
}
