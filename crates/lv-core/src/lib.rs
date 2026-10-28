#![deny(unsafe_code)]

pub mod color;
pub mod command;
pub mod decode;
pub mod exif_edit;
pub mod exif_info;
pub mod format;
#[doc(hidden)]
pub mod fuzzing;
pub mod heif_edit;
#[cfg(windows)]
pub mod identity;
pub mod jpeg_check;
pub mod jpeg_extent;
mod jpeg_markers;
pub mod listing;
pub mod portable;
pub mod prefetch;
pub mod profile;
pub mod resize;
pub mod settings;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod tiling;
pub mod unwind;
pub mod viewport;
