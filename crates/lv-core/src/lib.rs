#![deny(unsafe_code)]

pub mod color;
pub mod command;
pub mod decode;
pub mod exif_edit;
pub mod format;
pub mod heif_edit;
#[cfg(windows)]
pub mod identity;
pub mod jpeg_check;
pub mod jpeg_extent;
mod jpeg_markers;
pub mod listing;
pub mod prefetch;
pub mod settings;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod viewport;
