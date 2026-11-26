//! Loads the built core by path, as the host will, to check its file name and exports on
//! each OS.

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::PathBuf;
use std::process::Command;

use gth_libretro_sys::{RETRO_API_VERSION, retro_api_version_fn};

/// Builds the test core and returns its dynamic library: `libgth_testcore.so`,
/// `libgth_testcore.dylib` or `gth_testcore.dll`.
///
/// `cargo test` doesn't build a cdylib-only package's library, and where cargo puts artifacts
/// depends on `CARGO_TARGET_DIR`, `--target` and the cargo version, so this asks the cargo
/// running the tests to build it (a no-op when it is fresh) and reads the path from cargo's
/// JSON messages.
fn build_testcore() -> PathBuf {
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()));
    cargo.current_dir(env!("CARGO_MANIFEST_DIR")).args([
        "build",
        "--locked",
        "-p",
        "gth-testcore",
        "--message-format=json",
    ]);
    if !cfg!(debug_assertions) {
        cargo.arg("--release");
    }
    let output = cargo.output().expect("run cargo");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let name = format!("{DLL_PREFIX}gth_testcore{DLL_SUFFIX}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(r#""reason":"compiler-artifact""#))
        .flat_map(|line| line.split('"'))
        // JSON escapes the backslashes in Windows paths.
        .map(|field| PathBuf::from(field.replace(r"\\", r"\")))
        .find(|path| path.file_name().is_some_and(|file| file == name.as_str()))
        .expect("cargo reported the test core's library")
}

const ENTRY_POINTS: [&str; 25] = [
    "retro_set_environment",
    "retro_set_video_refresh",
    "retro_set_audio_sample",
    "retro_set_audio_sample_batch",
    "retro_set_input_poll",
    "retro_set_input_state",
    "retro_init",
    "retro_deinit",
    "retro_api_version",
    "retro_get_system_info",
    "retro_get_system_av_info",
    "retro_set_controller_port_device",
    "retro_reset",
    "retro_run",
    "retro_serialize_size",
    "retro_serialize",
    "retro_unserialize",
    "retro_cheat_reset",
    "retro_cheat_set",
    "retro_load_game",
    "retro_load_game_special",
    "retro_unload_game",
    "retro_get_region",
    "retro_get_memory_data",
    "retro_get_memory_size",
];

#[test]
fn library_exports_the_libretro_api() {
    let path = build_testcore();
    // SAFETY: the test core has no load-time initialisers.
    let library = unsafe { libloading::Library::new(&path) }.expect("load the test core");
    for name in ENTRY_POINTS {
        // SAFETY: only checks that the symbol exists; it isn't called.
        unsafe { library.get::<unsafe extern "C" fn()>(name.as_bytes()) }
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    // SAFETY: `retro_api_version` has this type (checked in the crate) and takes no arguments.
    let api_version = unsafe { library.get::<retro_api_version_fn>(b"retro_api_version") }
        .expect("retro_api_version");
    // SAFETY: as above.
    assert_eq!(unsafe { api_version() }, RETRO_API_VERSION);
}
