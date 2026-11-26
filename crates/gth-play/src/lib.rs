//! gth-play's headless half (docs/human-play.md): masks from controllers, bindings files,
//! the play session and its recordings, the display transform, latency probes and the
//! headless smoke. The binary adds the window, audio device and controllers; only it and
//! this crate depend on windowing, audio or input libraries (gth-core stays headless).

pub mod audio;
pub mod bindings;
pub mod display;
pub mod game;
pub mod input;
pub mod latency;
pub mod session;
pub mod smoke;
