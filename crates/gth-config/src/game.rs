//! `games/<game-id>/game.toml`: everything the harness needs to run one game (docs/games.md).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Condition, RamVar, Result, invalid};

/// One game's config. [`GameConfig::parse`] and [`GameConfig::load`] validate it fully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameConfig {
    /// The core's name in `cores.lock`.
    pub core: String,
    /// How far to turn the core's frame clockwise: 90 or 270 for vertical (tate) games.
    pub orientation: Orientation,
    /// The part of the core's frame to keep, before rotation; `None` keeps all of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<Crop>,
    /// The content file the core loads, with its `parents` (a clone set's parent sets).
    pub content: FileSpec,
    /// BIOS files the core needs in the system directory; `file` may be a path relative to
    /// it (`fbneo/pgm.zip`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bios: Vec<FileSpec>,
    /// Game buttons (RetroPad names) and the action-space slot each one is pressed by.
    pub buttons: BTreeMap<RetroButton, Slot>,
    /// How an episode starts after the core is back at its load state.
    pub reset: Reset,
    /// Named RAM variables (`score`, `lives`, `bombs`, `stage`, `credits`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ram: BTreeMap<String, RamVar>,
    pub episode: Episode,
    /// Core option values (`SET_VARIABLES` keys); the rest keep the core's defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub core_options: BTreeMap<String, String>,
}

/// Clockwise rotation of the core's frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub enum Orientation {
    Deg0,
    Deg90,
    Deg270,
}

impl Orientation {
    pub fn degrees(self) -> u32 {
        match self {
            Orientation::Deg0 => 0,
            Orientation::Deg90 => 90,
            Orientation::Deg270 => 270,
        }
    }
}

impl TryFrom<u32> for Orientation {
    type Error = String;

    fn try_from(degrees: u32) -> Result<Self, String> {
        match degrees {
            0 => Ok(Orientation::Deg0),
            90 => Ok(Orientation::Deg90),
            270 => Ok(Orientation::Deg270),
            _ => Err(format!("orientation must be 0, 90 or 270, not {degrees}")),
        }
    }
}

impl From<Orientation> for u32 {
    fn from(orientation: Orientation) -> u32 {
        orientation.degrees()
    }
}

/// `[x, y, width, height]` in the core's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "[usize; 4]", into = "[usize; 4]")]
pub struct Crop {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl TryFrom<[usize; 4]> for Crop {
    type Error = String;

    fn try_from([x, y, width, height]: [usize; 4]) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("crop {width}x{height} is empty"));
        }
        Ok(Crop {
            x,
            y,
            width,
            height,
        })
    }
}

impl From<Crop> for [usize; 4] {
    fn from(crop: Crop) -> Self {
        [crop.x, crop.y, crop.width, crop.height]
    }
}

/// A file the harness looks for: identified by size and hashes, or, for an archive whose own
/// hash depends on how it was packed (an arcade set's zip), by name with its members listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawFileSpec", into = "RawFileSpec")]
pub struct FileSpec {
    /// The name the core expects: a plain file name, or for a BIOS a relative path whose
    /// components are plain names.
    pub file: String,
    /// `None` for an archive, which has `members` instead.
    pub hashes: Option<Hashes>,
    /// Files inside an archive, checked by the ROM audit.
    pub members: Vec<Member>,
    /// For the content only: files the core needs next to it, such as the parent set of an
    /// arcade clone (`ddp3.zip` for `ddpdojblk.zip`).
    pub parents: Vec<FileSpec>,
    /// For the content only: the content is a clone inside its first parent's archive (a
    /// merged set), so no file of its own exists. The harness finds that parent and passes
    /// the core `<parent's directory>/<file>`, which a core that loads from the path (FBNeo)
    /// resolves through the parent archive.
    pub in_parent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hashes {
    pub size: u64,
    /// 40 lowercase hex digits.
    pub sha1: String,
    /// 8 lowercase hex digits.
    pub crc32: String,
}

/// A file inside an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub name: String,
    pub size: u64,
    pub sha1: String,
    pub crc32: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFileSpec {
    file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crc32: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    members: Vec<Member>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parents: Vec<FileSpec>,
    #[serde(default, skip_serializing_if = "is_false")]
    in_parent: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

fn check_hex(what: &str, value: &str, digits: usize) -> Result<(), String> {
    if value.len() == digits
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(format!(
            "{what} {value:?} isn't {digits} lowercase hex digits"
        ))
    }
}

/// A plain file name: no directories, so a config can't point outside its directory, and
/// no `:`, so on Windows it can't be drive-relative (`C:x`) or name a stream.
fn check_file_name(what: &str, name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains(['/', '\\', ':']) || name == "." || name == ".." {
        Err(format!("{what} {name:?} must be a plain file name"))
    } else {
        Ok(())
    }
}

/// A relative path of plain names joined by `/` (no `..`, no root, no drive), so it stays
/// inside the directory it is joined to.
pub(crate) fn check_relative_path(what: &str, path: &str) -> Result<(), String> {
    (path.split('/'))
        .try_for_each(|name| check_file_name(what, name))
        .map_err(|_| format!("{what} {path:?} must be a plain file name or relative path"))
}

impl TryFrom<RawFileSpec> for FileSpec {
    type Error = String;

    fn try_from(raw: RawFileSpec) -> Result<Self, String> {
        check_relative_path("file", &raw.file)?;
        let hashes = match (raw.size, raw.sha1, raw.crc32, raw.members.is_empty()) {
            (Some(size), Some(sha1), Some(crc32), true) => {
                check_hex("sha1", &sha1, 40)?;
                check_hex("crc32", &crc32, 8)?;
                Some(Hashes { size, sha1, crc32 })
            }
            (None, None, None, false) => None,
            _ => {
                return Err(format!(
                    "{}: give `size`, `sha1` and `crc32`, or (for an archive) `members`",
                    raw.file
                ));
            }
        };
        for member in &raw.members {
            check_hex("sha1", &member.sha1, 40)?;
            check_hex("crc32", &member.crc32, 8)?;
        }
        Ok(FileSpec {
            file: raw.file,
            hashes,
            members: raw.members,
            parents: raw.parents,
            in_parent: raw.in_parent,
        })
    }
}

impl From<FileSpec> for RawFileSpec {
    fn from(spec: FileSpec) -> Self {
        let (size, sha1, crc32) = match spec.hashes {
            Some(Hashes { size, sha1, crc32 }) => (Some(size), Some(sha1), Some(crc32)),
            None => (None, None, None),
        };
        RawFileSpec {
            file: spec.file,
            size,
            sha1,
            crc32,
            members: spec.members,
            parents: spec.parents,
            in_parent: spec.in_parent,
        }
    }
}

/// The libretro RetroPad's buttons, by their `RETRO_DEVICE_ID_JOYPAD_*` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetroButton {
    B,
    Y,
    Select,
    Start,
    Up,
    Down,
    Left,
    Right,
    A,
    X,
    L,
    R,
    L2,
    R2,
    L3,
    R3,
}

impl RetroButton {
    /// Its bit in the joypad mask (`1 << RETRO_DEVICE_ID_JOYPAD_*`).
    pub fn bit(self) -> u16 {
        1 << self as u16
    }

    /// Whether a game may map it to a slot: not the d-pad (the direction does that) and not
    /// select or start (coin and start are the harness's, pressed only by reset scripts).
    pub fn is_action_button(self) -> bool {
        !matches!(
            self,
            RetroButton::Select
                | RetroButton::Start
                | RetroButton::Up
                | RetroButton::Down
                | RetroButton::Left
                | RetroButton::Right
        )
    }
}

/// A unified action-space button slot, the same across games (docs/actions.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Shot,
    Bomb,
    Aux1,
    Aux2,
}

impl Slot {
    pub const ALL: [Slot; 4] = [Slot::Shot, Slot::Bomb, Slot::Aux1, Slot::Aux2];

    pub fn name(self) -> &'static str {
        match self {
            Slot::Shot => "shot",
            Slot::Bomb => "bomb",
            Slot::Aux1 => "aux1",
            Slot::Aux2 => "aux2",
        }
    }
}

/// How an episode starts, after the core is back at the state right after loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Reset {
    /// Inputs to play, in order: insert a coin, press start, wait for the stage.
    Script(Vec<ResetStep>),
    /// A savestate file, in `<data_dir>/states/<game-id>/` (made from the user's own ROM, so
    /// never committed).
    State(StateFile),
}

/// A reset savestate: its file name and SHA-1, checked when the game loads, since a
/// savestate only fits the core build that made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateFile {
    pub file: String,
    /// 40 lowercase hex digits.
    pub sha1: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetStep {
    /// Buttons held, any RetroPad button (select is coin on arcade cores).
    pub press: Vec<RetroButton>,
    /// Frames to hold them, at least 1.
    pub frames: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Episode {
    /// When the episode ends, on RAM variables, e.g. `lives == 0 && game_over != 0`.
    pub terminate: Condition,
}

impl GameConfig {
    /// Parses and validates a `game.toml`; `what` names it in errors.
    pub fn parse(text: &str, what: &str) -> Result<GameConfig> {
        let config: GameConfig =
            toml::from_str(text).map_err(|error| invalid(what, error.to_string()))?;
        config.validate().map_err(|reason| invalid(what, reason))?;
        Ok(config)
    }

    /// Reads, parses and validates a `game.toml`.
    pub fn load(path: &Path) -> Result<GameConfig> {
        Self::parse(&crate::read(path)?, &path.display().to_string())
    }

    /// The config as TOML; [`parse`](Self::parse) reads it back to an equal config.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("every field serializes to TOML")
    }

    /// The rules serde's types can't express on their own.
    fn validate(&self) -> Result<(), String> {
        if self.core.is_empty() {
            return Err("core is empty".into());
        }
        if self.buttons.is_empty() {
            return Err("buttons is empty: map at least one button to a slot".into());
        }
        if let Some(button) = self.buttons.keys().find(|b| !b.is_action_button()) {
            return Err(format!(
                "buttons: {button:?} can't be in the action space (the d-pad is the direction; \
                 select and start belong to reset scripts)"
            ));
        }
        match &self.reset {
            Reset::Script(steps) if steps.is_empty() => return Err("reset script is empty".into()),
            Reset::Script(steps) if steps.iter().any(|step| step.frames == 0) => {
                return Err("reset script steps need at least 1 frame".into());
            }
            Reset::Script(_) => {}
            Reset::State(state) => {
                check_file_name("reset state", &state.file)?;
                check_hex("reset state sha1", &state.sha1, 40)?;
            }
        }
        check_file_name("content file", &self.content.file)?;
        if self.content.in_parent {
            if self.content.parents.is_empty() {
                return Err(format!(
                    "content {}: in_parent needs `parents`, the first being the archive that \
                     holds it",
                    self.content.file
                ));
            }
            if self.content.hashes.is_some() {
                return Err(format!(
                    "content {}: in_parent is for an archive set, which gives `members`",
                    self.content.file
                ));
            }
        }
        for parent in &self.content.parents {
            check_file_name("content parent", &parent.file)?;
            if !parent.parents.is_empty() {
                return Err(format!(
                    "content parent {}: parents don't nest",
                    parent.file
                ));
            }
            if parent.in_parent {
                return Err(format!(
                    "content parent {}: only the content can be in_parent",
                    parent.file
                ));
            }
        }
        if let Some(bios) = self.bios.iter().find(|bios| !bios.parents.is_empty()) {
            return Err(format!("bios {}: only the content has parents", bios.file));
        }
        if let Some(bios) = self.bios.iter().find(|bios| bios.in_parent) {
            return Err(format!(
                "bios {}: only the content can be in_parent",
                bios.file
            ));
        }
        if let Some(name) = self.ram.keys().find(|name| !is_identifier(name)) {
            return Err(format!(
                "RAM variable name {name:?} isn't an identifier ([A-Za-z_][A-Za-z0-9_]*)"
            ));
        }
        if let Some(name) = (self.episode.terminate.variables().into_iter())
            .find(|name| !self.ram.contains_key(*name))
        {
            return Err(format!(
                "episode.terminate reads {name:?}, which isn't a RAM variable"
            ));
        }
        Ok(())
    }

    /// The reset script as `(joypad mask, frames)` pairs, or `None` for a savestate reset.
    pub fn reset_inputs(&self) -> Option<Vec<(u16, u32)>> {
        match &self.reset {
            Reset::Script(steps) => Some(
                steps
                    .iter()
                    .map(|step| (step.press.iter().fold(0, |m, b| m | b.bit()), step.frames))
                    .collect(),
            ),
            Reset::State(_) => None,
        }
    }

    /// Whether the episode ends with these RAM variable values (from every variable in
    /// [`ram`](Self::ram)).
    pub fn terminated(&self, values: &BTreeMap<String, i64>) -> Result<bool> {
        self.episode
            .terminate
            .eval(values)
            .map_err(|reason| invalid("episode.terminate", reason))
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Encoding, Location, MemoryId};

    const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Every field, both RAM locations, and an archive BIOS.
    pub(crate) fn full() -> String {
        format!(
            r#"
core = "gth_testcore"
orientation = 90
crop = [0, 8, 320, 232]

[content]
file = "game.gthc"
size = 21
sha1 = "{SHA1}"
crc32 = "89abcdef"
parents = [{{ file = "parent.zip", members = [{{ name = "p0.bin", size = 4, sha1 = "{SHA1}", crc32 = "00000000" }}] }}]

[[bios]]
file = "fbneo/bios.zip"
members = [{{ name = "rom0.bin", size = 4, sha1 = "{SHA1}", crc32 = "00000000" }}]

[buttons]
a = "shot"
b = "bomb"
y = "aux1"
r = "shot"

[reset]
script = [{{ press = ["start"], frames = 2 }}, {{ press = [], frames = 60 }}]

[ram.score]
memory = "system_ram"
offset = 0
size = 4
encoding = "bcd"
scale = 10

[ram.lives]
address = 0x8
size = 2
encoding = "le"
swap16 = true

[episode]
terminate = "lives == 0 && score > 0"

[core_options]
gth_testcore_step = "4"
"#
        )
    }

    #[test]
    fn parses_every_field_and_round_trips() {
        let config = GameConfig::parse(&full(), "full").unwrap();
        assert_eq!(config.orientation, Orientation::Deg90);
        assert_eq!(config.crop.unwrap().height, 232);
        assert_eq!(config.content.hashes.as_ref().unwrap().size, 21);
        assert_eq!(config.bios[0].hashes, None);
        assert_eq!(config.bios[0].file, "fbneo/bios.zip");
        assert_eq!(config.content.parents[0].file, "parent.zip");
        assert_eq!(config.buttons[&RetroButton::R], Slot::Shot);
        assert_eq!(
            config.ram["score"].location,
            Location::Memory {
                memory: MemoryId::SystemRam,
                offset: 0
            }
        );
        assert_eq!(config.ram["lives"].location, Location::Address(8));
        assert_eq!(config.ram["lives"].encoding, Encoding::Le);
        assert!(config.ram["lives"].swap16 && !config.ram["score"].swap16);
        assert_eq!(
            (config.ram["score"].scale, config.ram["lives"].scale),
            (10, 1)
        );
        assert_eq!(config.reset_inputs(), Some(vec![(1 << 3, 2), (0, 60)]));
        assert_eq!(config.core_options["gth_testcore_step"], "4");
        let again = GameConfig::parse(&config.to_toml(), "round trip").unwrap();
        assert_eq!(again, config);
    }

    #[test]
    fn evaluates_the_terminate_condition() {
        let config = GameConfig::parse(&full(), "full").unwrap();
        let values =
            |score, lives| BTreeMap::from([("score".into(), score), ("lives".into(), lives)]);
        assert!(config.terminated(&values(10, 0)).unwrap());
        assert!(!config.terminated(&values(10, 1)).unwrap());
        assert!(config.terminated(&BTreeMap::new()).is_err());
    }

    #[test]
    fn savestate_reset_round_trips() {
        let text = full().replace(
            "script = [{ press = [\"start\"], frames = 2 }, { press = [], frames = 60 }]",
            &format!("state = {{ file = \"stage1.state\", sha1 = \"{SHA1}\" }}"),
        );
        let config = GameConfig::parse(&text, "state").unwrap();
        let state = StateFile {
            file: "stage1.state".into(),
            sha1: SHA1.into(),
        };
        assert_eq!(config.reset, Reset::State(state));
        assert_eq!(config.reset_inputs(), None);
        assert_eq!(
            GameConfig::parse(&config.to_toml(), "again").unwrap(),
            config
        );
    }

    /// [`full`] with the content a clone inside its parent's archive (a merged set).
    pub(crate) fn merged() -> String {
        let text = full().replacen(
            &format!("file = \"game.gthc\"\nsize = 21\nsha1 = \"{SHA1}\"\ncrc32 = \"89abcdef\"\n"),
            &format!(
                "file = \"clone.zip\"\nin_parent = true\n\
                 members = [{{ name = \"c0.bin\", size = 4, sha1 = \"{SHA1}\", crc32 = \"00000000\" }}]\n"
            ),
            1,
        );
        assert_ne!(text, full());
        text
    }

    #[test]
    fn a_clone_can_be_in_its_parents_archive() {
        let config = GameConfig::parse(&merged(), "merged").unwrap();
        assert!(config.content.in_parent);
        assert_eq!(config.content.file, "clone.zip");
        assert!(!full().contains("in_parent"));
        assert!(
            !GameConfig::parse(&full(), "full")
                .unwrap()
                .content
                .in_parent
        );
        assert_eq!(
            GameConfig::parse(&config.to_toml(), "again").unwrap(),
            config
        );
        let orphan: String = (merged().lines())
            .filter(|line| !line.starts_with("parents ="))
            .map(|line| format!("{line}\n"))
            .collect();
        let error = GameConfig::parse(&orphan, "orphan")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("clone.zip: in_parent needs `parents`"),
            "{error}"
        );
    }

    #[test]
    fn validation_errors_name_the_problem() {
        let cases = [
            ("orientation = 90", "orientation = 180", "0, 90 or 270"),
            (
                "crop = [0, 8, 320, 232]",
                "crop = [0, 8, 0, 232]",
                "is empty",
            ),
            (
                "crc32 = \"89abcdef\"",
                "crc32 = \"89ABCDEF\"",
                "lowercase hex",
            ),
            ("size = 21\n", "", "give `size`, `sha1` and `crc32`"),
            (
                "file = \"game.gthc\"",
                "file = \"../game.gthc\"",
                "plain file name",
            ),
            (
                "file = \"game.gthc\"",
                "file = \"roms/game.gthc\"",
                "content file \"roms/game.gthc\" must be a plain file name",
            ),
            ("\"parent.zip\"", "\"C:parent.zip\"", "plain file name"),
            ("fbneo/bios.zip", "fbneo/../bios.zip", "or relative path"),
            ("fbneo/bios.zip", "/fbneo/bios.zip", "or relative path"),
            ("fbneo/bios.zip", "fbneo\\\\bios.zip", "or relative path"),
            (
                "file = \"fbneo/bios.zip\"",
                "file = \"fbneo/bios.zip\"\nparents = [{ file = \"p.zip\", size = 1, sha1 = \"0123456789abcdef0123456789abcdef01234567\", crc32 = \"00000000\" }]",
                "only the content has parents",
            ),
            (
                "crc32 = \"89abcdef\"\nparents",
                "crc32 = \"89abcdef\"\nin_parent = true\nparents",
                "in_parent is for an archive set",
            ),
            (
                "\"parent.zip\", members",
                "\"parent.zip\", in_parent = true, members",
                "content parent parent.zip: only the content can be in_parent",
            ),
            (
                "file = \"fbneo/bios.zip\"",
                "file = \"fbneo/bios.zip\"\nin_parent = true",
                "bios fbneo/bios.zip: only the content can be in_parent",
            ),
            ("a = \"shot\"", "start = \"shot\"", "select and start"),
            ("a = \"shot\"", "up = \"shot\"", "the d-pad"),
            ("a = \"shot\"", "a = \"jump\"", "unknown variant"),
            ("frames = 2", "frames = 0", "at least 1 frame"),
            (
                "encoding = \"bcd\"",
                "encoding = \"bcd\"\naddress = 4",
                "either `memory`",
            ),
            ("size = 2\n", "size = 3\n", "even size"),
            ("size = 4\n", "size = 9\n", "1 to 8 bytes"),
            ("scale = 10", "scale = 0", "at least 1"),
            ("size = 2\n", "size = 8\n", "doesn't fit an i64"),
            ("[ram.lives]", "[ram.\"lives left\"]", "isn't an identifier"),
            (
                "score > 0",
                "credits > 0",
                "\"credits\", which isn't a RAM variable",
            ),
            ("score > 0\"", "score >\"", "unexpected end"),
            (
                "core = \"gth_testcore\"",
                "core = \"gth_testcore\"\ncolour = 1",
                "unknown field",
            ),
            (
                "encoding = \"le\"",
                "encoding = \"le\"\nendian = 1",
                "unknown field",
            ),
        ];
        for (from, to, reason) in cases {
            let text = full().replacen(from, to, 1);
            assert_ne!(text, full(), "{from:?} not found");
            let error = GameConfig::parse(&text, "case").unwrap_err().to_string();
            assert!(error.contains(reason), "{to:?}: {error}");
            assert!(error.starts_with("invalid case: "), "{error}");
        }
    }
}
