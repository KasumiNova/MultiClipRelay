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
        // - Setting file/URI payloads to PRIMARY can confuse some toolchains / file managers.
        // - Some environments may accidentally interpret PRIMARY text as a "folder name" and
        //   still paste the URI list from the regular clipboard, producing empty weird folders.
        // To reduce these artifacts, only set BOTH for *pure* text copies.
        let only_text = items
            .iter()
            .all(|(mime, _)| mime.starts_with("text/plain"));
        let clipboard = if only_text {
            ClipboardType::Both
        } else {
            ClipboardType::Regular
        };

        let mut opts = Options::new();
        opts.clipboard(clipboard).seat(Seat::All);

        match opts.copy_multi(sources.clone()) {
            Ok(()) => Ok(()),
            Err(WlCopyError::PrimarySelectionUnsupported) if only_text => {
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
