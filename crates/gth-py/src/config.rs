//! `_native.GameConfig`: a parsed `game.toml` (docs/games.md) with its action space
//! (docs/actions.md), and the ROM lookup (docs/roms.md).

use std::collections::BTreeMap;
use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::errors;

#[pyclass(module = "gtharness._native", frozen)]
pub struct GameConfig {
    pub config: gth_config::GameConfig,
    space: gth_config::ActionSpace,
}

impl GameConfig {
    pub(crate) fn new(config: gth_config::GameConfig) -> Self {
        let space = gth_config::ActionSpace::new(&config.buttons);
        GameConfig { config, space }
    }

    fn decode_action(&self, action: u32) -> errors::Result<gth_config::Action> {
        let decoded = self.space.decode(action).ok_or_else(|| {
            PyValueError::new_err(format!(
                "action {action} is out of range (0..{})",
                gth_config::ActionSpace::LEN
            ))
        })?;
        Ok(decoded)
    }
}

#[pymethods]
impl GameConfig {
    /// Reads and validates a `game.toml`.
    #[staticmethod]
    fn load(py: Python<'_>, path: PathBuf) -> errors::Result<Self> {
        Ok(Self::new(
            py.detach(|| gth_config::GameConfig::load(&path))?,
        ))
    }

    /// Parses and validates `game.toml` text; `what` names it in errors.
    #[staticmethod]
    fn parse(text: &str, what: &str) -> errors::Result<Self> {
        Ok(Self::new(gth_config::GameConfig::parse(text, what)?))
    }

    fn to_toml(&self) -> String {
        self.config.to_toml()
    }

    #[getter]
    fn core(&self) -> &str {
        &self.config.core
    }

    /// Neutral frames the core runs after each savestate load (docs/games.md).
    #[getter]
    fn settle_frames(&self) -> u32 {
        self.config.settle_frames
    }

    /// Clockwise degrees to turn the frame: 0, 90 or 270.
    #[getter]
    fn orientation(&self) -> u32 {
        self.config.orientation.degrees()
    }

    /// `(x, y, width, height)`, or `None` for the whole frame.
    #[getter]
    fn crop(&self) -> Option<(usize, usize, usize, usize)> {
        let crop = self.config.crop?;
        Some((crop.x, crop.y, crop.width, crop.height))
    }

    /// The board whose sprite list labels shards (`psikyo`, `testcore`), or `None`.
    #[getter]
    fn sprite_hw(&self) -> Option<&'static str> {
        self.config.sprite_hw.map(|hw| hw.name())
    }

    /// The ALE action numbers of an Atari game's minimal action set, ascending; empty for a
    /// game without one.
    #[getter]
    fn atari_minimal_actions(&self) -> Vec<u32> {
        // u32, not u8: PyO3 turns a Vec<u8> into bytes rather than a list.
        self.config
            .atari_minimal_actions
            .iter()
            .copied()
            .map(u32::from)
            .collect()
    }

    #[getter]
    fn core_options(&self) -> BTreeMap<String, String> {
        self.config.core_options.clone()
    }

    /// The RAM variable names, sorted.
    #[getter]
    fn ram_names(&self) -> Vec<String> {
        self.config.ram.keys().cloned().collect()
    }

    /// The reset savestate's file name, or `None` for a script reset.
    #[getter]
    fn reset_state(&self) -> Option<&str> {
        match &self.config.reset {
            gth_config::Reset::State(state) => Some(&state.file),
            gth_config::Reset::Script(_) => None,
        }
    }

    /// Raises `ConfigError` unless `state` (the reset savestate's bytes) has the SHA-1 in
    /// game.toml; a no-op for a script reset.
    fn check_reset_state(&self, state: &[u8]) -> errors::Result<()> {
        if let gth_config::Reset::State(spec) = &self.config.reset {
            spec.check(state)?;
        }
        Ok(())
    }

    /// The reset script as `(joypad mask, frames)` pairs, or `None` for a savestate reset.
    #[getter]
    fn reset_inputs(&self) -> Option<Vec<(u16, u32)>> {
        self.config.reset_inputs()
    }

    /// The action space's button slots, in bit order, the same in every game: `"shot"`,
    /// `"bomb"`, `"aux1"`, `"aux2"`.
    #[getter]
    fn slots(&self) -> Vec<&'static str> {
        gth_config::Slot::ALL.map(gth_config::Slot::name).to_vec()
    }

    /// The slots this game maps to a button, in bit order; the others do nothing.
    #[getter]
    fn mapped_slots(&self) -> Vec<&'static str> {
        self.space
            .mapped_slots()
            .into_iter()
            .map(|slot| slot.name())
            .collect()
    }

    /// The number of flat actions: always 144 (`9 * 2**4`).
    #[getter]
    fn n_actions(&self) -> u32 {
        gth_config::ActionSpace::LEN
    }

    /// The flat action of a direction (0 neutral, 1-8 clockwise from up) and held slots (bit
    /// i = `slots[i]`).
    fn encode(&self, direction: u32, buttons: u32) -> errors::Result<u32> {
        let action = (gth_config::Direction::from_index(direction))
            .zip(u8::try_from(buttons).ok())
            .map(|(direction, buttons)| gth_config::Action { direction, buttons })
            .and_then(|action| self.space.encode(action));
        action.ok_or_else(|| {
            PyValueError::new_err(format!(
                "no action has direction {direction} and buttons {buttons:#b} (4 slots)"
            ))
            .into()
        })
    }

    /// `(direction, buttons)` of a flat action.
    fn decode(&self, action: u32) -> errors::Result<(u32, u32)> {
        let decoded = self.decode_action(action)?;
        Ok((decoded.direction as u32, u32::from(decoded.buttons)))
    }

    /// The Atari action (ALE's enum order, 0-17) of a flat action, or `None` if it holds a
    /// mapped `bomb`, `aux1` or `aux2` (docs/actions.md, "Atari").
    fn to_atari(&self, action: u32) -> errors::Result<Option<u8>> {
        Ok(self.space.to_atari(self.decode_action(action)?))
    }

    /// The flat action of an Atari action (ALE's enum order, 0-17): its direction, with
    /// `shot` for fire.
    #[pyo3(name = "from_atari")]
    fn flat_of_atari(&self, atari: u8) -> errors::Result<u32> {
        let action = gth_config::ActionSpace::from_atari(atari).ok_or_else(|| {
            PyValueError::new_err(format!(
                "Atari action {atari} is out of range (0..{})",
                gth_config::ActionSpace::ATARI_LEN
            ))
        })?;
        Ok((self.space.encode(action)).expect("an Atari action holds at most shot"))
    }

    /// The mask over the 18 Atari actions of an action set (a game's minimal set).
    #[staticmethod]
    fn atari_mask(actions: Vec<u8>) -> errors::Result<Vec<bool>> {
        let mask = gth_config::ActionSpace::atari_mask(&actions).ok_or_else(|| {
            PyValueError::new_err(format!(
                "an Atari action in {actions:?} is out of range (0..{})",
                gth_config::ActionSpace::ATARI_LEN
            ))
        })?;
        Ok(mask.to_vec())
    }

    /// The RetroPad mask of every flat action, indexed by the action.
    fn joypad_table(&self) -> Vec<u16> {
        self.space.joypad_table()
    }

    /// Whether the episode ends with these RAM variable values.
    fn terminated(&self, ram: BTreeMap<String, i64>) -> errors::Result<bool> {
        Ok(self.config.terminated(&ram)?)
    }

    /// The content path under `rom_dir` (the file found by SHA-1, or for content `in_parent`
    /// a path beside its parent archive), or `None`.
    fn find_content(&self, py: Python<'_>, rom_dir: PathBuf) -> errors::Result<Option<PathBuf>> {
        Ok(py.detach(|| self.config.find_content(&rom_dir))?)
    }

    /// The names of the BIOS files missing from `system_dir` (or not matching their hashes).
    fn missing_bios(&self, py: Python<'_>, system_dir: PathBuf) -> errors::Result<Vec<String>> {
        let missing = py.detach(|| self.config.missing_bios(&system_dir))?;
        Ok(missing.into_iter().map(|spec| spec.file.clone()).collect())
    }

    /// The names of the content's parents missing from beside `content` (the found content
    /// file).
    fn missing_parents(&self, py: Python<'_>, content: PathBuf) -> errors::Result<Vec<String>> {
        let missing = py.detach(|| self.config.missing_parents(&content))?;
        Ok(missing.into_iter().map(|spec| spec.file.clone()).collect())
    }

    /// The ROM audit of the game's files (docs/roms.md, "Auditing"), as JSON: the content
    /// under `rom_dir`, its parents beside it, the BIOS files under `system_dir`.
    fn audit(
        &self,
        py: Python<'_>,
        rom_dir: PathBuf,
        system_dir: PathBuf,
    ) -> errors::Result<String> {
        Ok(py
            .detach(|| self.config.audit(&rom_dir, &system_dir))?
            .to_json())
    }

    /// The content file's name, for messages.
    #[getter]
    fn content_file(&self) -> &str {
        &self.config.content.file
    }

    /// Whether the content is a clone inside its first parent's archive (docs/games.md).
    #[getter]
    fn content_in_parent(&self) -> bool {
        self.config.content.in_parent
    }

    /// The `[content]` table as TOML, members and parents included (docs/games.md).
    #[getter]
    fn content_toml(&self) -> String {
        self.config.content_toml()
    }
}

/// The ROM directory from `GTH_ROM_DIR` or the user config file, or `None`.
#[pyfunction]
fn rom_dir() -> errors::Result<Option<PathBuf>> {
    Ok(gth_config::rom_dir()?)
}

/// Where the user config file is looked for, or `None` if no location variable is set.
#[pyfunction]
fn user_config_path() -> Option<PathBuf> {
    gth_config::user_config_path()
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<GameConfig>()?;
    module.add_function(wrap_pyfunction!(rom_dir, module)?)?;
    module.add_function(wrap_pyfunction!(user_config_path, module)?)?;
    Ok(())
}
