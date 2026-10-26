#![deny(unsafe_code)]

pub mod color;
pub mod command;
pub mod decode;
pub mod format;
#[cfg(windows)]
pub mod identity;
pub mod jpeg_extent;
pub mod listing;
pub mod prefetch;
pub mod settings;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod viewport;
