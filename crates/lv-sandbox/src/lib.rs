//! Starts light-view's `lv-worker.exe` in its sandbox (spec §6.1): a low integrity
//! restricted token, a kill-on-close Job the process is born in, the worker's AppContainer
//! when its folder allows it (ruling R2), creation mitigations, only the three pipe ends
//! inherited and a minimal environment, with a stepwise fallback for the best-effort layers.
//!
//! [`sandbox`] creates one process, [`appcontainer`] decides on and creates the AppContainer,
//! and [`process`] runs a started worker's writer, reader and stderr threads. The app's
//! worker pool builds on these; the Explorer extension uses [`run_once`].

pub mod appcontainer;
mod once;
pub mod process;
pub mod sandbox;
mod threads;

pub use appcontainer::delete_profile;
pub use once::{run_once, RunError};
pub use threads::live_threads;

#[cfg(test)]
pub(crate) mod test_support;
