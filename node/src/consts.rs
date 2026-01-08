pub const APP_DIR_NAME: &str = "multicliprelay";

// Suppress marker keys (used only locally for loop prevention).
pub const FILE_SUPPRESS_KEY: &str = "application/x-multicliprelay-file";

pub const URI_LIST_MIME: &str = "text/uri-list";
pub const GNOME_COPIED_FILES_MIME: &str = "x-special/gnome-copied-files";

// Marker MIME set by wl-apply when it writes clipboard content originating from the relay.
// wl-watch should ignore clipboard changes while this marker exists to avoid feedback loops.
pub const APPLIED_MARKER_MIME: &str = "application/x-multicliprelay-applied";

pub const TAR_MIME: &str = "application/x-tar";
