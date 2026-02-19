// Internal modules used by the `multicliprelay-node` binary.
//
// Keeping these in a library module allows us to split the former monolithic
// `main.rs` into smaller, testable units.

#[cfg(unix)]
pub mod clipboard;

// Non-unix stub so the crate can compile on Windows even when the real
// clipboard backends (Wayland/X11) are not available.
#[cfg(not(unix))]
pub mod clipboard {
	pub async fn wl_paste(_mime: &str) -> anyhow::Result<Vec<u8>> {
		anyhow::bail!("wl-paste backend is only available on unix (Wayland)")
	}

	pub async fn wl_copy(_mime: &str, _bytes: &[u8]) -> anyhow::Result<()> {
		anyhow::bail!("wl-copy backend is only available on unix (Wayland)")
	}

	pub async fn wl_copy_multi(_items: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
		anyhow::bail!("wl-copy backend is only available on unix (Wayland)")
	}
}
pub mod consts;
pub mod dedup;
pub mod hash;
pub mod history;
pub mod image_mode;
pub mod net;
pub mod paths;
pub mod suppress;
#[path = "transfer/file.rs"]
pub mod transfer_file;

#[path = "transfer/image.rs"]
pub mod transfer_image;

#[cfg(unix)]
#[path = "x11/sync/mod.rs"]
pub mod x11_sync;

#[cfg(not(unix))]
pub mod x11_sync {
	use std::path::PathBuf;
	use std::time::Duration;

	#[derive(Clone, Debug)]
	pub struct X11SyncOpts {
		pub state_dir: PathBuf,
		pub poll_interval: Duration,
		pub max_text_bytes: usize,
		pub max_image_bytes: usize,
	}

	pub async fn x11_sync_service(_opts: X11SyncOpts) -> anyhow::Result<()> {
		anyhow::bail!("x11-sync is only available on unix")
	}

	pub async fn x11_hook_apply_wayland_to_x11(
		_state_dir: &PathBuf,
		_kind: &str,
		_sample: Vec<u8>,
	) {
		// no-op on non-unix
	}
}

#[cfg(unix)]
#[path = "x11/native.rs"]
pub mod x11_native;

#[cfg(not(unix))]
pub mod x11_native {
	pub fn spawn_clipboard_owner(_items: Vec<(String, Vec<u8>)>) -> anyhow::Result<()> {
		anyhow::bail!("x11 clipboard owner is only available on unix")
	}
}
