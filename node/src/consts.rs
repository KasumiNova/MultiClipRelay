pub const APP_DIR_NAME: &str = "multicliprelay";

// Suppress marker keys (used only locally for loop prevention).
pub const FILE_SUPPRESS_KEY: &str = "application/x-multicliprelay-file";

pub const URI_LIST_MIME: &str = "text/uri-list";
// KDE/Dolphin may offer this alongside (or instead of) text/uri-list.
pub const KDE_URI_LIST_MIME: &str = "application/x-kde4-urilist";
pub const GNOME_COPIED_FILES_MIME: &str = "x-special/gnome-copied-files";

// Marker MIME set by wl-apply when it writes clipboard content originating from the relay.
// wl-watch should ignore clipboard changes while this marker exists to avoid feedback loops.
pub const APPLIED_MARKER_MIME: &str = "application/x-multicliprelay-applied";

// Local-only coordination marker for X11 <-> Wayland sync.
//
// We intentionally keep this as a single MIME and put direction/origin into the payload bytes:
// - On Wayland: x11-sync writes this marker with payload "from=x11".
// - On X11:     x11-sync writes this marker with payload "from=wl".
//
// This marker should generally NOT be forwarded over the network.
pub const X11_SYNC_MARKER_MIME: &str = "application/x-multicliprelay-x11-sync";

pub const TAR_MIME: &str = "application/x-tar";
