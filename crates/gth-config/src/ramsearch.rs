//! RAM search vocabulary and scripts (docs/games.md, "RAM search"): which memory to search,
//! how to read a value from its bytes (a [`View`]), how a value must relate to its previous
//! one (a [`Relation`]), and a [`SearchScript`] that drives a game and narrows candidates.
//! The search itself runs in `gth-core`. RAM search is a developer tool for finding and
//! checking RAM variables; game RAM is never a policy's input.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::Deserialize;

use crate::game::{RetroButton, check_relative_path};
use crate::ram::{Encoding, Location, MemoryId, RamVar};
use crate::{Result, invalid};

/// The memory a search covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    /// One core memory (`retro_get_memory_data`), searched by offset.
    Memory(MemoryId),
    /// Every memory map region (`SET_MEMORY_MAPS`) that isn't constant (ROM), searched by
    /// emulated address.
    MemoryMap,
}

impl FromStr for Space {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        if name == "memory_map" {
            return Ok(Space::MemoryMap);
        }
        MemoryId::ALL
            .into_iter()
            .find(|memory| memory.name() == name)
            .map(Space::Memory)
            .ok_or_else(|| {
                format!(
                    "space must be system_ram, save_ram, video_ram, rtc or memory_map, not {name:?}"
                )
            })
    }
}

impl fmt::Display for Space {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Space::Memory(memory) => f.write_str(memory.name()),
            Space::MemoryMap => f.write_str("memory_map"),
        }
    }
}

/// How a candidate's bytes become a number: `size` bytes (1, 2 or 4) in `encoding`, with the
/// two bytes of each 16-bit word swapped first when `swap16` (FBNeo's 68k RAM). Written as
/// `<encoding><size>[-swap16]`: `bcd4`, `le2`, `be4-swap16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub size: usize,
    pub encoding: Encoding,
    pub swap16: bool,
}

impl View {
    /// Every distinct view. `le1` and `be1` read the same byte, as `le2-swap16` and `be2`
    /// read the same word, so only the first of each pair is here.
    pub const ALL: [View; 12] = [
        View::new(1, Encoding::Le, false),
        View::new(1, Encoding::Bcd, false),
        View::new(2, Encoding::Le, false),
        View::new(2, Encoding::Be, false),
        View::new(2, Encoding::Bcd, false),
        View::new(2, Encoding::Bcd, true),
        View::new(4, Encoding::Le, false),
        View::new(4, Encoding::Be, false),
        View::new(4, Encoding::Bcd, false),
        View::new(4, Encoding::Le, true),
        View::new(4, Encoding::Be, true),
        View::new(4, Encoding::Bcd, true),
    ];

    const fn new(size: usize, encoding: Encoding, swap16: bool) -> View {
        View {
            size,
            encoding,
            swap16,
        }
    }

    /// The RAM variable reading this view at `location`, unscaled; always valid (1 to 4
    /// bytes, even when `swap16`).
    pub fn ram_var(self, location: Location) -> RamVar {
        RamVar {
            location,
            size: self.size,
            encoding: self.encoding,
            swap16: self.swap16,
            scale: 1,
        }
    }
}

impl FromStr for View {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        let error = || {
            format!(
                "a view is <bcd|le|be><1|2|4>, with -swap16 for 2 and 4 (bcd4, be2-swap16), \
                 not {name:?}"
            )
        };
        let (base, swap16) = match name.strip_suffix("-swap16") {
            Some(base) => (base, true),
            None => (name, false),
        };
        let (encoding, size) = Encoding::SEARCHABLE
            .into_iter()
            .find_map(|encoding| Some((encoding, base.strip_prefix(encoding.name())?)))
            .ok_or_else(error)?;
        let size = match size {
            "1" if !swap16 => 1,
            "2" => 2,
            "4" => 4,
            _ => return Err(error()),
        };
        Ok(View::new(size, encoding, swap16))
    }
}

impl fmt::Display for View {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.encoding.name(), self.size)?;
        if self.swap16 {
            f.write_str("-swap16")?;
        }
        Ok(())
    }
}

/// How a candidate's value must relate to its value at the previous snapshot to stay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Equal,
    Changed,
    Increased,
    Decreased,
    /// The new value is this one, whatever the previous was.
    Equals(i64),
}

impl Relation {
    /// The relation named `name` (`equal`, `changed`, `increased`, `decreased`, `equals`);
    /// `value` is required by `equals` and refused by the others.
    pub fn new(name: &str, value: Option<i64>) -> Result<Relation, String> {
        let relation = match (name, value) {
            ("equals", Some(value)) => return Ok(Relation::Equals(value)),
            ("equals", None) => return Err("equals needs a value".into()),
            ("equal", _) => Relation::Equal,
            ("changed", _) => Relation::Changed,
            ("increased", _) => Relation::Increased,
            ("decreased", _) => Relation::Decreased,
            _ => {
                return Err(format!(
                    "a relation is equal, changed, increased, decreased or equals, not {name:?}"
                ));
            }
        };
        match value {
            Some(_) => Err(format!("{name} takes no value (only equals does)")),
            None => Ok(relation),
        }
    }

    /// Whether `new`, following `old`, satisfies the relation.
    pub fn holds(self, old: i64, new: i64) -> bool {
        match self {
            Relation::Equal => new == old,
            Relation::Changed => new != old,
            Relation::Increased => new > old,
            Relation::Decreased => new < old,
            Relation::Equals(value) => new == value,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Relation::Equal => "equal",
            Relation::Changed => "changed",
            Relation::Increased => "increased",
            Relation::Decreased => "decreased",
            Relation::Equals(_) => "equals",
        }
    }

    /// The value of `equals`, else `None`.
    pub fn value(self) -> Option<i64> {
        match self {
            Relation::Equals(value) => Some(value),
            _ => None,
        }
    }
}

/// A scripted RAM search: what to search, and the steps that drive the game and narrow the
/// candidates (docs/games.md, "RAM search").
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawScript")]
pub struct SearchScript {
    pub space: Space,
    /// The views searched at once, without duplicates.
    pub views: Vec<View>,
    pub steps: Vec<SearchStep>,
}

/// One step: load a savestate, then hold buttons for some frames, then filter the candidates
/// by `relation`, or, without one, take a new snapshot to compare the next step with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStep {
    /// A savestate file, relative to the script's directory.
    pub state: Option<String>,
    /// The joypad mask held, and for how many frames (0: run none).
    pub buttons: u16,
    pub frames: u32,
    pub relation: Option<Relation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScript {
    space: String,
    views: Vec<String>,
    #[serde(rename = "step")]
    steps: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    state: Option<String>,
    #[serde(default)]
    press: Vec<RetroButton>,
    #[serde(default)]
    frames: u32,
    expect: Option<String>,
    value: Option<i64>,
}

impl TryFrom<RawScript> for SearchScript {
    type Error = String;

    fn try_from(raw: RawScript) -> Result<Self, String> {
        let space = raw.space.parse()?;
        let mut views = Vec::new();
        for name in &raw.views {
            let more = match name.as_str() {
                "all" => View::ALL.to_vec(),
                _ => vec![name.parse()?],
            };
            for view in more {
                if !views.contains(&view) {
                    views.push(view);
                }
            }
        }
        if views.is_empty() {
            return Err("views is empty: name at least one view, or \"all\"".into());
        }
        if raw.steps.is_empty() {
            return Err("a script needs at least one [[step]]".into());
        }
        let steps = (raw.steps.into_iter().enumerate())
            .map(|(index, step)| {
                SearchStep::try_from(step).map_err(|reason| format!("step {}: {reason}", index + 1))
            })
            .collect::<Result<_, _>>()?;
        Ok(SearchScript {
            space,
            views,
            steps,
        })
    }
}

impl TryFrom<RawStep> for SearchStep {
    type Error = String;

    fn try_from(raw: RawStep) -> Result<Self, String> {
        if let Some(state) = &raw.state {
            check_relative_path("state", state)?;
        }
        if !raw.press.is_empty() && raw.frames == 0 {
            return Err("press needs frames".into());
        }
        let relation = match raw.expect {
            Some(name) => Some(Relation::new(&name, raw.value)?),
            None if raw.value.is_some() => return Err("value needs expect = \"equals\"".into()),
            None => None,
        };
        Ok(SearchStep {
            state: raw.state,
            buttons: raw.press.iter().fold(0, |mask, button| mask | button.bit()),
            frames: raw.frames,
            relation,
        })
    }
}

impl SearchScript {
    /// Parses and validates a script; `what` names it in errors.
    pub fn parse(text: &str, what: &str) -> Result<SearchScript> {
        toml::from_str(text).map_err(|error| invalid(what, error.to_string()))
    }

    /// Reads, parses and validates a script file.
    pub fn load(path: &Path) -> Result<SearchScript> {
        Self::parse(&crate::read(path)?, &path.display().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_round_trip_through_their_names() {
        for view in View::ALL {
            assert_eq!(view.to_string().parse::<View>(), Ok(view));
        }
        let names: Vec<String> = View::ALL.iter().map(View::to_string).collect();
        assert_eq!(
            names,
            [
                "le1",
                "bcd1",
                "le2",
                "be2",
                "bcd2",
                "bcd2-swap16",
                "le4",
                "be4",
                "bcd4",
                "le4-swap16",
                "be4-swap16",
                "bcd4-swap16"
            ]
        );
        assert_eq!("be1".parse(), Ok(View::new(1, Encoding::Be, false)));
        for bad in ["le1-swap16", "le3", "u16", "bcd", "le8", "", "le2-swap"] {
            let error = bad.parse::<View>().unwrap_err();
            assert!(error.contains(&format!("{bad:?}")), "{error}");
        }
    }

    #[test]
    fn spaces_parse_and_print() {
        for name in ["system_ram", "save_ram", "video_ram", "rtc", "memory_map"] {
            assert_eq!(name.parse::<Space>().unwrap().to_string(), name);
        }
        assert!("ram".parse::<Space>().unwrap_err().contains("\"ram\""));
    }

    #[test]
    fn relations_hold_and_check_their_value() {
        assert!(Relation::Equal.holds(3, 3) && !Relation::Equal.holds(3, 4));
        assert!(Relation::Changed.holds(3, 4) && !Relation::Changed.holds(3, 3));
        assert!(Relation::Increased.holds(3, 4) && !Relation::Increased.holds(4, 4));
        assert!(Relation::Decreased.holds(4, 3) && !Relation::Decreased.holds(3, 3));
        assert!(Relation::Equals(7).holds(0, 7) && !Relation::Equals(7).holds(7, 6));
        assert_eq!(Relation::new("equals", Some(5)), Ok(Relation::Equals(5)));
        assert_eq!(Relation::new("increased", None), Ok(Relation::Increased));
        assert!(Relation::new("equals", None).is_err());
        assert!(
            Relation::new("changed", Some(1))
                .unwrap_err()
                .contains("no value")
        );
        assert!(
            Relation::new("bigger", None)
                .unwrap_err()
                .contains("\"bigger\"")
        );
        for relation in [Relation::Equal, Relation::Decreased, Relation::Equals(-2)] {
            assert_eq!(
                Relation::new(relation.name(), relation.value()),
                Ok(relation)
            );
        }
    }

    #[test]
    fn parses_a_script() {
        let script = SearchScript::parse(
            r#"
space = "system_ram"
views = ["bcd4", "le2", "bcd4"]

[[step]]
state = "states/alive.state"

[[step]]
press = ["a", "start"]
frames = 300
expect = "increased"

[[step]]
frames = 60

[[step]]
expect = "equals"
value = 300
"#,
            "script",
        )
        .unwrap();
        assert_eq!(script.space, Space::Memory(MemoryId::SystemRam));
        assert_eq!(
            script.views,
            [
                View::new(4, Encoding::Bcd, false),
                View::new(2, Encoding::Le, false)
            ]
        );
        let step = |state: Option<&str>, buttons, frames, relation| SearchStep {
            state: state.map(str::to_owned),
            buttons,
            frames,
            relation,
        };
        let a_start = RetroButton::A.bit() | RetroButton::Start.bit();
        assert_eq!(
            script.steps,
            [
                step(Some("states/alive.state"), 0, 0, None),
                step(None, a_start, 300, Some(Relation::Increased)),
                step(None, 0, 60, None),
                step(None, 0, 0, Some(Relation::Equals(300))),
            ]
        );
        let all = "space = \"memory_map\"\nviews = [\"le2\", \"all\"]\n[[step]]\nframes = 1\n";
        let script = SearchScript::parse(all, "script").unwrap();
        assert_eq!(script.space, Space::MemoryMap);
        assert_eq!(script.views.len(), View::ALL.len());
        assert_eq!(script.views[0], View::new(2, Encoding::Le, false));
    }

    #[test]
    fn rejects_bad_scripts() {
        let head = "space = \"system_ram\"\nviews = [\"bcd4\"]\n";
        let cases = [
            (
                "space = \"rom\"\nviews = [\"bcd4\"]\n[[step]]\n",
                "space must be",
            ),
            ("space = \"rtc\"\nviews = []\n[[step]]\n", "views is empty"),
            (head, "missing field `step`"),
            (&format!("{head}step = []\n"), "at least one [[step]]"),
            (
                &format!("{head}[[step]]\npress = [\"a\"]\n"),
                "step 1: press needs frames",
            ),
            (
                &format!("{head}[[step]]\nframes = 1\n[[step]]\nvalue = 3\n"),
                "step 2: value needs expect",
            ),
            (
                &format!("{head}[[step]]\nexpect = \"equals\"\n"),
                "equals needs a value",
            ),
            (
                &format!("{head}[[step]]\nstate = \"../x.state\"\n"),
                "plain file name or relative path",
            ),
            (&format!("{head}[[step]]\npress = [\"coin\"]\n"), "coin"),
            (&format!("{head}[[step]]\nhold = 1\n"), "unknown field"),
        ];
        for (text, expected) in cases {
            let error = SearchScript::parse(text, "script").unwrap_err().to_string();
            assert!(error.contains(expected), "{text}: {error}");
        }
    }
}
