use anyhow::Context;
use std::path::PathBuf;

use crate::hash::sha256_hex;
use crate::history::record_send;
use crate::image_mode::ImageMode;
use crate::net::{connect, send_frame};
use crate::paths::{first_8, received_dir};

use utils::{Kind, Message};

pub fn image_mimes() -> &'static [&'static str] {
    &["image/png", "image/jpeg", "image/webp", "image/gif"]
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
        _ => anyhow::bail!(
            "unsupported image type (cannot detect mime): {}",
            file.display()
        ),
    };
    Ok(mime.to_string())
}

pub fn to_png(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("decode image")?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .context("encode png")?;
    Ok(out)
}

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

pub async fn send_image(
    local_device_id: &str,
    local_device_name: &str,
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
        ImageMode::Passthrough | ImageMode::MultiMime | ImageMode::SpoofPng => {
            (mime.as_str(), bytes)
        }
        ImageMode::ForcePng => ("image/png", to_png(&bytes)?),
    };

    let stream = connect(relay).await?;
    let mut msg = Message::new_image(local_device_id, room, send_mime, send_bytes);
    let local_name_opt = if local_device_name.trim().is_empty() {
        None
    } else {
        Some(local_device_name.to_string())
    };
    msg.sender_name = local_name_opt.clone();
    let sha = sha256_hex(msg.payload.as_deref().unwrap_or_default());
    msg.sha256 = Some(sha.clone());

    // Best-effort: persist sent image so local UI can preview it too.
    if let Some(payload) = msg.payload.as_deref() {
        let sha8 = first_8(&sha).to_string();
        let dir = received_dir().join(&sha8);
        tokio::fs::create_dir_all(&dir).await.ok();
        let ext = image_ext_from_mime(send_mime).unwrap_or("bin");
        let p = dir.join(format!("image.{ext}"));
        let _ = tokio::fs::write(&p, payload).await;
    }

    send_frame(stream, msg.to_bytes()).await?;

    record_send(
        local_device_id,
        local_name_opt,
        room,
        relay,
        Kind::Image,
        Some(send_mime.to_string()),
        file.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string()),
        msg.size,
        Some(sha),
    )
    .await;

    Ok(())
}
