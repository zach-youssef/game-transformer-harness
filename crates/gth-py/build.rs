//! Records build metadata for `_native.build_info()`: git commit and dirty state, target
//! triple, profile, rustc.

use std::env;
use std::path::Path;
use std::process::Command;

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    output.status.success().then(|| text.trim().to_owned())
}

fn git(args: &[&str]) -> Option<String> {
    command_output("git", args)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Builds outside a git checkout (an sdist) have no commit to record.
    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=GTH_GIT_SHA={sha}");

    // Whether the files that build the extension differ from HEAD. Watching them reruns this
    // script whenever they change, so the flag is current at every build. Paths are relative
    // to this crate, the script's working directory.
    let sources = ["../../crates", "../../Cargo.toml", "../../Cargo.lock"];
    let sources: Vec<&str> = sources
        .into_iter()
        .filter(|p| Path::new(p).exists())
        .collect();
    for source in &sources {
        println!("cargo:rerun-if-changed={source}");
    }
    let status = [
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
        ],
        &sources[..],
    ]
    .concat();
    let dirty = match git(&status) {
        Some(changes) if changes.is_empty() => "false",
        Some(_) => "true",
        None => "unknown",
    };
    println!("cargo:rustc-env=GTH_GIT_DIRTY={dirty}");

    // Rebuild when HEAD moves: HEAD itself (checkouts) and the branch it names (commits).
    // Only existing paths: cargo reruns the script on every build for a missing one.
    let mut watched = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(branch);
    }
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--git-path", &name])
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let target = env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=GTH_TARGET={target}");
    let profile = env::var("PROFILE").expect("cargo sets PROFILE for build scripts");
    println!("cargo:rustc-env=GTH_PROFILE={profile}");
    let rustc = env::var("RUSTC").expect("cargo sets RUSTC for build scripts");
    let rustc_version = command_output(&rustc, &["--version"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GTH_RUSTC={rustc_version}");
}
