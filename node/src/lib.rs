// Internal modules used by the `multicliprelay-node` binary.
//
// Keeping these in a library module allows us to split the former monolithic
// `main.rs` into smaller, testable units.

pub mod clipboard;
pub mod consts;
pub mod hash;
pub mod history;
pub mod image_mode;
pub mod net;
pub mod paths;
pub mod suppress;
pub mod transfer_file;
pub mod transfer_image;
pub mod x11_sync;
pub mod x11_native;
