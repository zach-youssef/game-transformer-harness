//! Helpers shared by the workspace's tests (a dev-dependency only, never shipped).

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The test core's dynamic library, built on first use: `libgth_testcore.so`,
/// `libgth_testcore.dylib` or `gth_testcore.dll`.
///
/// `cargo test` doesn't build a cdylib-only package's library, and where cargo puts artifacts
/// depends on `CARGO_TARGET_DIR`, `--target` and the cargo version, so this asks the cargo
/// running the tests to build it (a no-op when it is fresh) and reads the path from cargo's
/// JSON messages. It builds once per process: on Windows a loaded DLL can't be overwritten,
/// so a test must not rebuild the core while another test in the process has it loaded
/// (docs/libretro.md, "Loading the test core in tests").
///
/// # Panics
///
/// If the build fails or cargo doesn't report the library.
pub fn testcore_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(build_testcore)
}

fn build_testcore() -> PathBuf {
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()));
    // The calling test's package dir, read at run time: a path baked in with `env!` goes stale
    // when worktrees share a CARGO_TARGET_DIR and cargo reuses this crate's build from another.
    if let Some(dir) = std::env::var_os("CARGO_MANIFEST_DIR") {
        cargo.current_dir(dir);
    }
    cargo.args([
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
    let stdout = String::from_utf8(output.stdout).expect("cargo prints UTF-8");
    library_from_messages(&stdout).expect("cargo reported the test core's library")
}

/// The test core's library among the `filenames` of cargo's `compiler-artifact` messages.
fn library_from_messages(messages: &str) -> Option<PathBuf> {
    let name = format!("{DLL_PREFIX}gth_testcore{DLL_SUFFIX}");
    messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| message["filenames"].as_array().cloned())
        .flatten()
        .filter_map(|file| file.as_str().map(PathBuf::from))
        .find(|path| path.file_name().is_some_and(|file| file == name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_library_in_cargo_messages() {
        let name = format!("{DLL_PREFIX}gth_testcore{DLL_SUFFIX}");
        let dir = if cfg!(windows) {
            r"C:\t\debug"
        } else {
            "/t/debug"
        };
        let library = Path::new(dir).join(&name);
        let artifact = serde_json::json!({
            "reason": "compiler-artifact",
            "filenames": [Path::new(dir).join("gth_testcore.d"), &library],
        });
        let messages = format!(
            "{}\nnot json\n{}\n{}\n",
            serde_json::json!({"reason": "build-script-executed"}),
            artifact,
            serde_json::json!({"reason": "build-finished", "success": true}),
        );
        assert_eq!(library_from_messages(&messages), Some(library));
        assert_eq!(
            library_from_messages("{\"reason\":\"compiler-artifact\"}"),
            None
        );
    }
}
