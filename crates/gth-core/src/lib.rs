//! libretro host (no Python, no windowing): load a core, run frames (sticky actions included),
//! savestates, memory, and RAM search over that memory.
//!
//! One core per process: see [`Core`] and docs/architecture.md.

mod core;
mod error;
mod host;
mod ramsearch;
mod rng;

pub use crate::core::{AvInfo, Core, SystemInfo};
pub use error::{Error, Result};
pub use host::MemoryRegion;
pub use ramsearch::{Candidate, RamSearch};
pub use rng::Rng;
