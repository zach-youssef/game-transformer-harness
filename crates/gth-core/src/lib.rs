//! libretro host (no Python, no windowing): load a core, run frames, savestates, memory, and
//! RAM search over that memory.
//!
//! One core per process: see [`Core`] and docs/architecture.md.

mod core;
mod error;
mod host;
mod ramsearch;

pub use crate::core::{AvInfo, Core, SystemInfo};
pub use error::{Error, Result};
pub use host::MemoryRegion;
pub use ramsearch::{Candidate, RamSearch};
