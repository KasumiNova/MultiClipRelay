use anyhow::Context;
use tokio::process::Command;

pub async fn wl_paste(mime: &str) -> anyhow::Result<Vec<u8>> {
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

pub async fn wl_copy(mime: &str, bytes: &[u8]) -> anyhow::Result<()> {
    wl_copy_multi(vec![(mime.to_string(), bytes.to_vec())]).await
}

pub async fn wl_copy_multi(items: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        use wl_clipboard_rs::copy::{
            ClipboardType, Error as WlCopyError, MimeSource, MimeType, Options, Seat, Source,
        };

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
        let clipboard = if want_both {
            ClipboardType::Both
        } else {
            ClipboardType::Regular
        };

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
