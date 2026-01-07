use anyhow::Context;
use std::io::Cursor;
use std::path::PathBuf;
use url::Url;
use walkdir::WalkDir;

use crate::consts::TAR_MIME;
use crate::hash::sha256_hex;
use crate::history::record_send;
use crate::net::{connect, send_frame};
use crate::suppress::is_file_suppressed;

use utils::{Kind, Message};

pub fn detect_file_mime(bytes: &[u8], file: &PathBuf) -> String {
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
        "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" => {
            "text/plain;charset=utf-8".to_string()
        }
        _ => "application/octet-stream".to_string(),
    }
}

pub fn parse_uri_list(bytes: &[u8]) -> Vec<Url> {
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

pub fn collect_clipboard_paths(bytes: &[u8]) -> Vec<PathBuf> {
    parse_uri_list(bytes)
        .into_iter()
        .filter_map(|u| u.to_file_path().ok())
        .collect()
}

pub fn bundle_name_for(paths: &[PathBuf]) -> String {
    if paths.len() == 1 {
        if let Some(n) = paths[0].file_name().and_then(|s| s.to_str()) {
            return format!("{}.tar", n);
        }
    }
    format!("multicliprelay-bundle-{}.tar", utils::now_ms())
}

pub fn build_tar_bundle(paths: &[PathBuf]) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());

    for p in paths {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item")
            .to_string();

        let md = std::fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
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

pub fn unpack_tar_bytes(bytes: &[u8], dest: &PathBuf) -> anyhow::Result<()> {
    let mut ar = tar::Archive::new(Cursor::new(bytes));
    for e in ar.entries().context("tar entries")? {
        let mut e = e.context("tar entry")?;
        // `unpack_in` defends against path traversal.
        e.unpack_in(dest).context("unpack_in")?;
    }
    Ok(())
}

pub fn build_uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        if let Ok(u) = Url::from_file_path(p) {
            // Many file managers expect directory URIs to end with '/'.
            // `Url::from_file_path` does not guarantee that.
            let mut s = u.as_str().to_string();
            if p.is_dir() && !s.ends_with('/') {
                s.push('/');
            }
            // RFC 2483 / text/uri-list commonly uses CRLF line endings; some consumers are picky.
            out.push_str(&s);
            out.push_str("\r\n");
        }
    }
    out
}

/// List top-level items under `dir` (both files and directories), sorted.
///
/// This is preferred over listing all files recursively when we want to preserve
/// "copy folder" semantics across machines.
pub fn list_top_level_items(dir: &PathBuf, max_items: usize) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect(),
        Err(_) => Vec::new(),
    };
    items.sort();
    if items.len() > max_items {
        items.truncate(max_items);
    }
    items
}

pub fn list_files_recursively(dir: &PathBuf, max_items: usize) -> Vec<PathBuf> {
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

pub async fn send_file(
    local_device_id: &str,
    room: &str,
    file: &PathBuf,
    relay: &str,
    max_file_bytes: usize,
) -> anyhow::Result<()> {
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
    let mut msg = Message::new_file(local_device_id, room, &name, &mime, bytes);
    msg.sha256 = Some(sha.clone());
    send_frame(stream, msg.to_bytes()).await?;

    record_send(
        local_device_id,
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

pub async fn send_paths_as_file(
    state_dir: &PathBuf,
    local_device_id: &str,
    room: &str,
    relay: &str,
    paths: Vec<PathBuf>,
    max_file_bytes: usize,
) -> anyhow::Result<Option<String>> {
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
                if is_file_suppressed(state_dir, room, &sha).await {
                    return Ok(None);
                }

                let name = paths[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string();
                let mime = detect_file_mime(&bytes, &paths[0]);

                let stream = connect(relay).await?;
                let mut msg = Message::new_file(local_device_id, room, &name, &mime, bytes);
                msg.sha256 = Some(sha.clone());
                send_frame(stream, msg.to_bytes()).await?;

                record_send(
                    local_device_id,
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
    if is_file_suppressed(state_dir, room, &sha).await {
        return Ok(None);
    }

    let name = bundle_name_for(&paths);

    let stream = connect(relay).await?;
    let mut msg = Message::new_file(local_device_id, room, &name, TAR_MIME, tar_bytes);
    msg.sha256 = Some(sha.clone());
    send_frame(stream, msg.to_bytes()).await?;

    record_send(
        local_device_id,
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

    #[test]
    fn build_uri_list_uses_file_scheme_and_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a b.txt");
        std::fs::write(&p, b"x").unwrap();

        let s = build_uri_list(&vec![p]);
        assert!(s.starts_with("file:///"), "uri list should start with file:/// but got: {s:?}");
        assert!(s.ends_with("\r\n"), "uri list should end with CRLF but got: {s:?}");
        assert!(
            !s.contains("file:////"),
            "uri list must not contain file://// (too many slashes): {s:?}"
        );
    }
}
