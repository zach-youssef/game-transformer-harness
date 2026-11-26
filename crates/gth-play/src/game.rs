//! Finding a game's files and loading its core, as `RetroEnv.from_game` does
//! (docs/roms.md): the content under the ROM directory, BIOS files in `<data>/system/`, the
//! core in `<data>/cores/`.

use std::env::consts::DLL_SUFFIX;
use std::path::{Path, PathBuf};

use gth_config::{CoreLock, CoresLock, GameConfig};
use gth_core::Core;

/// Where a game's files are. Every path is the caller's choice, with no hidden default but
/// the ROM directory's (`GTH_ROM_DIR` or the user config, as everywhere in the harness).
#[derive(Debug, Clone)]
pub struct GamePaths {
    /// The directory of game configs: `<games_dir>/<game_id>/game.toml`.
    pub games_dir: PathBuf,
    /// Holds `system/` and `cores/`.
    pub data_dir: PathBuf,
    /// `None`: `GTH_ROM_DIR` or the user config's `rom_dir`.
    pub rom_dir: Option<PathBuf>,
    /// `None`: `<data_dir>/cores/<core>_libretro<suffix>`.
    pub core: Option<PathBuf>,
}

/// A loaded game.
pub struct Loaded {
    pub core: Core,
    pub game: GameConfig,
    pub content: PathBuf,
    pub core_path: PathBuf,
    pub system_dir: PathBuf,
}

/// Where a data directory keeps core `core`: `cores/<core>_libretro<suffix>` (Python's
/// `core_library`).
pub fn core_library(data_dir: &Path, core: &str) -> PathBuf {
    data_dir
        .join("cores")
        .join(format!("{core}_libretro{DLL_SUFFIX}"))
}

/// The game's core's entry in the `cores.lock` at `lock_path` when `core_path` is the build
/// that lock pins for this platform (by SHA-256), else `None`: what a recording keeps as
/// `[core.lock]`, decided as `RetroEnv` decides it (`CoresLock::pinned`). A lock that is
/// missing or doesn't parse is an error, since the caller named it.
pub fn pinned_core(
    lock_path: &Path,
    game: &GameConfig,
    core_path: &Path,
) -> Result<Option<CoreLock>, String> {
    let lock = CoresLock::load(lock_path).map_err(|e| e.to_string())?;
    let target = gth_build_info::build_info()["target"];
    Ok(lock.pinned(&game.core, target, core_path).cloned())
}

/// Reads `<games_dir>/<game_id>/game.toml`.
pub fn load_config(games_dir: &Path, game_id: &str) -> Result<GameConfig, String> {
    GameConfig::load(&games_dir.join(game_id).join("game.toml")).map_err(|e| e.to_string())
}

/// Finds the game's content (and checks its parents and BIOS files), then loads its core.
pub fn load(game_id: &str, paths: &GamePaths) -> Result<Loaded, String> {
    let game = load_config(&paths.games_dir, game_id)?;
    let roms = match &paths.rom_dir {
        Some(dir) => dir.clone(),
        None => gth_config::rom_dir()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!(
                    "{game_id}: no ROM directory: pass --rom-dir, or set GTH_ROM_DIR or rom_dir in \
                 the user config ({})",
                    gth_config::user_config_path()
                        .map_or("none".into(), |p| p.display().to_string())
                )
            })?,
    };
    let error = |e: gth_config::Error| format!("{game_id}: {e}");
    let content = game.find_content(&roms).map_err(error)?.ok_or_else(|| {
        format!(
            "{game_id}: {} (by SHA-1) isn't in {}",
            game.content.file,
            roms.display()
        )
    })?;
    let names = |specs: Vec<&gth_config::FileSpec>| {
        specs
            .iter()
            .map(|s| s.file.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let parents = game.missing_parents(&content).map_err(error)?;
    if !parents.is_empty() {
        return Err(format!(
            "{game_id}: parent sets missing beside {}: {}",
            content.display(),
            names(parents)
        ));
    }
    let system_dir = paths.data_dir.join("system");
    let bios = game.missing_bios(&system_dir).map_err(error)?;
    if !bios.is_empty() {
        return Err(format!(
            "{game_id}: BIOS files missing from {}: {}",
            system_dir.display(),
            names(bios)
        ));
    }
    let core_path = paths
        .core
        .clone()
        .unwrap_or_else(|| core_library(&paths.data_dir, &game.core));
    let core = Core::load(
        &core_path,
        Some(&content),
        &system_dir,
        &game.core_options,
        game.settle_frames,
    )
    .map_err(|e| format!("{game_id}: cannot load {}: {e}", core_path.display()))?;
    Ok(Loaded {
        core,
        game,
        content,
        core_path,
        system_dir,
    })
}

/// Replays the recording in `dir` with zero desyncs (its content found as [`load`] finds a
/// game's, if it has any) and returns its frames.
pub fn verify(dir: &Path, paths: &GamePaths) -> Result<u64, String> {
    let recording = gth_record::Recording::open(dir).map_err(|e| e.to_string())?;
    let game = &recording.game;
    let content = match &recording.meta.content {
        None => None,
        Some(_) => {
            let roms = match &paths.rom_dir {
                Some(dir) => dir.clone(),
                None => gth_config::rom_dir()
                    .map_err(|e| e.to_string())?
                    .ok_or("no ROM directory: pass --rom-dir, or set GTH_ROM_DIR")?,
            };
            Some(
                game.find_content(&roms)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        format!(
                            "{} (by SHA-1) isn't in {}",
                            game.content.file,
                            roms.display()
                        )
                    })?,
            )
        }
    };
    let core_path = paths
        .core
        .clone()
        .unwrap_or_else(|| core_library(&paths.data_dir, &game.core));
    let system_dir = paths.data_dir.join("system");
    let mut replayer =
        gth_record::Replayer::load(recording, &core_path, content.as_deref(), &system_dir)
            .map_err(|e| e.to_string())?;
    replayer.run_to_end().map_err(|e| e.to_string())
}
