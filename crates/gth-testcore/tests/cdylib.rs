//! Loads the built core by path, as the host will, to check its file name and exports on
//! each OS.

use gth_libretro_sys::{RETRO_API_VERSION, retro_api_version_fn};

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
    let path = gth_test_support::testcore_path();
    // SAFETY: the test core has no load-time initialisers.
    let library = unsafe { libloading::Library::new(path) }.expect("load the test core");
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
