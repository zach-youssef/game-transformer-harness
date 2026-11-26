//! libretro host (no Python, no windowing): load a core, run frames, savestates, memory.
//!
//! One core per process: see [`Core`] and docs/architecture.md.

mod core;
mod error;
mod host;

pub use crate::core::{AvInfo, Core, SystemInfo};
pub use error::{Error, Result};
pub use host::MemoryRegion;
