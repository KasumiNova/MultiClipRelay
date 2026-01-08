use anyhow::Context;
use std::io::Cursor;
use std::time::UNIX_EPOCH;
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
    fn file_url_to_path_fuzzy(u: &Url) -> Option<PathBuf> {
        if u.scheme() != "file" {
            return None;
        }

        if let Ok(p) = u.to_file_path() {
            return Some(p);
        }

        // Some clipboard producers (notably in KDE/Dolphin) may emit `file://home/...`
        // when they actually mean `/home/...` (i.e. missing the third slash).
        // In that case Url parses `home` as host and `/...` as path.
        // Reconstruct the local path as `/{host}{path}`.
        let host = u.host_str()?;
        let path = u.path();
        if host.is_empty() {
            return None;
        }
        Some(PathBuf::from(format!("/{}{}", host, path)))
    }

    parse_uri_list(bytes)
        .into_iter()
        .filter_map(|u| file_url_to_path_fuzzy(&u))
        .collect()
}

pub fn bundle_name_for(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return format!("multicliprelay-bundle-{}.tar", utils::now_ms());
    }

    // Single path (file or dir): preserve its visible name.
    if paths.len() == 1 {
        if let Some(n) = paths[0].file_name().and_then(|s| s.to_str()) {
            return format!("{}.tar", n);
        }
        return format!("multicliprelay-bundle-{}.tar", utils::now_ms());
    }

    // Heuristic: some file managers represent "copy folder" as selecting the folder's
    // immediate children (multiple entries) rather than the folder itself.
    // If all selected items share the same parent dir, use that parent dir name.
    if let Some(parent0) = paths[0].parent() {
        let same_parent = paths.iter().all(|p| p.parent() == Some(parent0));
        if same_parent {
            if let Some(n) = parent0.file_name().and_then(|s| s.to_str()) {
                return format!("{}.tar", n);
            }
        }
    }

    format!("multicliprelay-bundle-{}.tar", utils::now_ms())
}

fn common_path_prefix(paths: &[PathBuf]) -> Option<PathBuf> {
    if paths.is_empty() {
        return None;
    }
    let mut prefix = paths[0].clone();
    for p in paths.iter().skip(1) {
        prefix = common_path_prefix2(&prefix, p);
        if prefix.as_os_str().is_empty() {
            break;
        }
    }
    Some(prefix)
}

fn common_path_prefix2(a: &PathBuf, b: &PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    let mut ita = a.components();
    let mut itb = b.components();
    loop {
        match (ita.next(), itb.next()) {
            (Some(ca), Some(cb)) if ca == cb => out.push(ca.as_os_str()),
            _ => break,
        }
    }
    out
}

fn unix_mode_or_default(md: Option<&std::fs::Metadata>, default_mode: u32) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(md) = md {
            return md.permissions().mode() & 0o7777;
        }
        default_mode
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        default_mode
    }
}

fn mtime_secs_or_zero(md: Option<&std::fs::Metadata>) -> u64 {
    let Some(md) = md else {
        return 0;
    };
    let Ok(t) = md.modified() else {
        return 0;
    };
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn header_for_dir_with(md: Option<&std::fs::Metadata>) -> tar::Header {
    let mut h = tar::Header::new_ustar();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_size(0);
    h.set_mode(unix_mode_or_default(md, 0o755));
    h.set_mtime(mtime_secs_or_zero(md));
    h.set_uid(0);
    h.set_gid(0);
    h.set_cksum();
    h
}

fn header_for_file_with(len: u64, md: Option<&std::fs::Metadata>) -> tar::Header {
    let mut h = tar::Header::new_ustar();
    h.set_entry_type(tar::EntryType::Regular);
    h.set_size(len);
    h.set_mode(unix_mode_or_default(md, 0o644));
    h.set_mtime(mtime_secs_or_zero(md));
    h.set_uid(0);
    h.set_gid(0);
    h.set_cksum();
    h
}

fn append_dir_deterministic(
    builder: &mut tar::Builder<Vec<u8>>,
    fs_dir: &PathBuf,
    archive_dir: &PathBuf,
) -> anyhow::Result<()> {
    // Root dir entry.
    let md_root = std::fs::metadata(fs_dir)
        .with_context(|| format!("metadata {}", fs_dir.display()))?;
    let mut h = header_for_dir_with(Some(&md_root));
    h.set_path(archive_dir)
        .with_context(|| format!("set tar dir path {}", archive_dir.display()))?;
    h.set_cksum();
    builder
        .append(&h, std::io::empty())
        .with_context(|| format!("append dir header {}", archive_dir.display()))?;

    // Walk children in stable order.
    let mut entries: Vec<(PathBuf, walkdir::DirEntry)> = WalkDir::new(fs_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path() != fs_dir)
        .filter_map(|e| {
            let rel = e.path().strip_prefix(fs_dir).ok()?.to_path_buf();
            Some((rel, e))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel, e) in entries {
        let fs_path = e.path().to_path_buf();
        let archive_path = archive_dir.join(&rel);
        if e.file_type().is_dir() {
            let md = std::fs::metadata(&fs_path)
                .with_context(|| format!("metadata {}", fs_path.display()))?;
            let mut h = header_for_dir_with(Some(&md));
            h.set_path(&archive_path).with_context(|| {
                format!("set tar dir path {}", archive_path.display())
            })?;
            h.set_cksum();
            builder
                .append(&h, std::io::empty())
                .with_context(|| format!("append dir {}", archive_path.display()))?;
        } else if e.file_type().is_file() {
            let md = std::fs::metadata(&fs_path)
                .with_context(|| format!("metadata {}", fs_path.display()))?;
            let mut f = std::fs::File::open(&fs_path)
                .with_context(|| format!("open {}", fs_path.display()))?;
            let mut h = header_for_file_with(md.len(), Some(&md));
            h.set_path(&archive_path).with_context(|| {
                format!("set tar file path {}", archive_path.display())
            })?;
            h.set_cksum();
            builder
                .append(&h, &mut f)
                .with_context(|| format!("append file {}", fs_path.display()))?;
        } else {
            // Skip symlinks/special files for safety.
            continue;
        }
    }

    Ok(())
}

fn append_file_deterministic(
    builder: &mut tar::Builder<Vec<u8>>,
    fs_file: &PathBuf,
    archive_file: &PathBuf,
) -> anyhow::Result<()> {
    let md = std::fs::metadata(fs_file).with_context(|| format!("metadata {}", fs_file.display()))?;
    if !md.is_file() {
        return Ok(());
    }
    let mut f = std::fs::File::open(fs_file).with_context(|| format!("open {}", fs_file.display()))?;
    let mut h = header_for_file_with(md.len(), Some(&md));
    h.set_path(archive_file)
        .with_context(|| format!("set tar file path {}", archive_file.display()))?;
    h.set_cksum();
    builder
        .append(&h, &mut f)
        .with_context(|| format!("append file {}", fs_file.display()))?;
    Ok(())
}

pub fn build_tar_bundle(paths: &[PathBuf]) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());

    // Heuristic: some environments represent "copy folder" as a flat list of files
    // (no directory entry in the uri-list). If we detect that all selected items are
    // files under a single directory tree, we preserve their relative paths so the
    // receiver can reconstruct the folder structure.
    let mut all_files = true;
    let mut parent_dirs: Vec<PathBuf> = Vec::new();
    let mut rel_has_nesting = false;
    for p in paths {
        let md = std::fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
        if !md.is_file() {
            all_files = false;
            break;
        }
        if let Some(parent) = p.parent() {
            parent_dirs.push(parent.to_path_buf());
        }
    }

    let mut tree_root: Option<PathBuf> = None;
    let mut tree_root_name: Option<String> = None;
    if all_files && !parent_dirs.is_empty() {
        if let Some(root) = common_path_prefix(&parent_dirs) {
            // If any file lives in a subdirectory under root, we consider this a "folder tree".
            for p in paths {
                if let Ok(rel) = p.strip_prefix(&root) {
                    if rel.components().count() > 1 {
                        rel_has_nesting = true;
                        break;
                    }
                }
            }

            if rel_has_nesting {
                if let Some(n) = root.file_name().and_then(|s| s.to_str()) {
                    tree_root = Some(root.clone());
                    tree_root_name = Some(n.to_string());
                }
            }
        }
    }

    // If we're preserving a file-tree (only-files selection), collect and append
    // directory headers first so empty dirs can still be reconstructed when possible.
    if let (Some(root), Some(root_name)) = (&tree_root, &tree_root_name) {
        let mut dirs: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        dirs.insert(PathBuf::from(root_name));
        for p in paths {
            if let Ok(rel) = p.strip_prefix(root) {
                if let Some(parent) = rel.parent() {
                    for d in parent.ancestors() {
                        if d.as_os_str().is_empty() {
                            break;
                        }
                        dirs.insert(PathBuf::from(root_name).join(d));
                    }
                }
            }
        }

        for d in dirs {
            let fs_dir = if d == PathBuf::from(root_name) {
                root.clone()
            } else {
                let rel = d
                    .strip_prefix(root_name)
                    .unwrap_or(d.as_path());
                root.join(rel)
            };
            let md = std::fs::metadata(&fs_dir)
                .with_context(|| format!("metadata {}", fs_dir.display()))?;
            let mut h = header_for_dir_with(Some(&md));
            h.set_path(&d)
                .with_context(|| format!("set tar dir path {}", d.display()))?;
            h.set_cksum();
            builder
                .append(&h, std::io::empty())
                .with_context(|| format!("append dir {}", d.display()))?;
        }
    }

    for p in paths {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("item")
            .to_string();

        let md = std::fs::metadata(p).with_context(|| format!("metadata {}", p.display()))?;
        if md.is_dir() {
            // Preserve the directory as a top-level folder in the archive.
            let archive_dir = PathBuf::from(&name);
            append_dir_deterministic(&mut builder, p, &archive_dir)
                .with_context(|| format!("append dir {}", p.display()))?;
        } else if md.is_file() {
            if let (Some(root), Some(root_name)) = (&tree_root, &tree_root_name) {
                if let Ok(rel) = p.strip_prefix(root) {
                    let archive_name = PathBuf::from(root_name).join(rel);
                    append_file_deterministic(&mut builder, p, &archive_name)
                        .with_context(|| format!("append file {}", p.display()))?;
                    continue;
                }
            }

            append_file_deterministic(&mut builder, p, &PathBuf::from(&name))
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
            // NOTE: Do not force a trailing '/'.
            // Some file managers (notably KDE/Dolphin) may interpret `file:///dir/` as
            // "copy contents of dir" rather than "copy the dir itself".
            out.push_str(u.as_str());
            // Many consumers accept LF; CRLF can confuse some clipboard bridges / file managers.
            out.push('\n');
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

    // Fast path: if file clipboard is globally suppressed (wildcard '*'), avoid
    // doing any expensive IO / tar building.
    if is_file_suppressed(state_dir, room, "0").await {
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
    fn collect_clipboard_paths_accepts_file_host_form() {
        // KDE/Dolphin sometimes emits `file://home/...` for local `/home/...` paths.
        let s = b"file://home/user/a.txt\n";
        let paths = collect_clipboard_paths(s);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from("/home/user/a.txt"));
    }

    #[test]
    fn build_uri_list_does_not_force_trailing_slash_for_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d");
        std::fs::create_dir_all(&p).unwrap();
        let s = build_uri_list(&vec![p.clone()]);
        let u = Url::from_file_path(&p).unwrap();
        assert_eq!(s, format!("{}\n", u.as_str()));
    }

    #[test]
    #[cfg(unix)]
    fn tar_bundle_preserves_file_mtime_seconds() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, b"hello").unwrap();

        // Set mtime to a stable value.
        let target_secs: i64 = 1_700_000_000; // ~2023-11
        unsafe {
            let cpath = CString::new(p.as_os_str().as_bytes()).unwrap();
            let ts = [
                libc::timespec {
                    tv_sec: target_secs,
                    tv_nsec: 0,
                },
                libc::timespec {
                    tv_sec: target_secs,
                    tv_nsec: 0,
                },
            ];
            let rc = libc::utimensat(libc::AT_FDCWD, cpath.as_ptr(), ts.as_ptr(), 0);
            assert_eq!(rc, 0);
        }

        let tar = build_tar_bundle(&vec![p.clone()]).unwrap();
        let out = tempfile::tempdir().unwrap();
        unpack_tar_bytes(&tar, &out.path().to_path_buf()).unwrap();

        let out_p = out.path().join("a.txt");
        let md = std::fs::metadata(&out_p).unwrap();
        let mtime = md
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(mtime, target_secs as i64);
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
    fn tar_bundle_preserves_tree_when_only_files_selected() {
        // Simulate environments that put a folder selection into the clipboard as a list of files.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("folder");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let a = root.join("a.txt");
        let b = sub.join("b.txt");
        std::fs::write(&a, b"hello").unwrap();
        std::fs::write(&b, b"world").unwrap();

        // Clipboard gives us only files, no directory entry.
        let tar = build_tar_bundle(&vec![a.clone(), b.clone()]).unwrap();
        assert!(!tar.is_empty());

        let out = tempfile::tempdir().unwrap();
        unpack_tar_bytes(&tar, &out.path().to_path_buf()).unwrap();

        assert!(out.path().join("folder").join("a.txt").exists());
        assert!(out.path().join("folder").join("sub").join("b.txt").exists());
    }

    #[test]
    fn build_uri_list_uses_file_scheme_and_lf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a b.txt");
        std::fs::write(&p, b"x").unwrap();

        let s = build_uri_list(&vec![p]);
        assert!(s.starts_with("file:///"), "uri list should start with file:/// but got: {s:?}");
        assert!(s.ends_with("\n"), "uri list should end with LF but got: {s:?}");
        assert!(
            !s.contains("file:////"),
            "uri list must not contain file://// (too many slashes): {s:?}"
        );
    }
}
