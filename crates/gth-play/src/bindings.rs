//! Bindings files: which keys and controller buttons press which RetroPad buttons
//! (docs/human-play.md, "Bindings").
//!
//! They live in `bindings/` beside the user config file (`gth_config::user_config_path`):
//! `keyboard.toml`, and one `gamepad-<name>.toml` per controller model, named after what
//! gilrs reports. A missing file is written from a template on first use, so every value in
//! play is in a file the player can read and change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gilrs::Button;
use gth_config::RetroButton;
use serde::Deserialize;
use winit::keyboard::KeyCode;

use crate::input::StickMap;

/// The keyboard: RetroPad buttons and the player's hotkeys.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardBindings {
    pub buttons: BTreeMap<RetroButton, Vec<KeyCode>>,
    pub hotkeys: Hotkeys,
}

/// Keys that control the player rather than the game.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hotkeys {
    /// Pause and resume emulation.
    pub pause: KeyCode,
    /// Save the state into the slot (and its file).
    pub save_state: KeyCode,
    /// Load the slot's state; finishes a recording first.
    pub load_state: KeyCode,
    /// Start a recording from the current state, or finish the one running.
    pub record: KeyCode,
    /// Finish any recording and quit.
    pub quit: KeyCode,
}

impl Hotkeys {
    /// Whether `key` is one of the hotkeys.
    pub fn contains(&self, key: KeyCode) -> bool {
        self.all().iter().any(|(k, _)| *k == key)
    }

    fn all(&self) -> [(KeyCode, &'static str); 5] {
        [
            (self.pause, "pause"),
            (self.save_state, "save_state"),
            (self.load_state, "load_state"),
            (self.record, "record"),
            (self.quit, "quit"),
        ]
    }
}

/// Which stick drives the d-pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stick {
    Left,
    Right,
}

/// One controller model.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GamepadBindings {
    /// See [`StickMap::new`].
    pub deadzone: f32,
    pub diagonal_degrees: f32,
    /// Sticks that drive the d-pad, besides the d-pad itself.
    pub sticks: Vec<Stick>,
    pub buttons: BTreeMap<RetroButton, Vec<Button>>,
}

impl GamepadBindings {
    pub fn stick_map(&self) -> StickMap {
        StickMap::new(self.deadzone, self.diagonal_degrees).expect("checked when parsed")
    }

    /// The mask bits of the buttons for which `pressed` holds.
    pub fn mask(&self, pressed: impl Fn(Button) -> bool) -> u16 {
        mask(&self.buttons, pressed)
    }
}

impl KeyboardBindings {
    /// The mask bits of the keys for which `pressed` holds.
    pub fn mask(&self, pressed: impl Fn(KeyCode) -> bool) -> u16 {
        mask(&self.buttons, pressed)
    }
}

fn mask<T: Copy>(buttons: &BTreeMap<RetroButton, Vec<T>>, pressed: impl Fn(T) -> bool) -> u16 {
    buttons
        .iter()
        .filter(|(_, inputs)| inputs.iter().any(|&input| pressed(input)))
        .fold(0, |mask, (button, _)| mask | button.bit())
}

/// The keyboard template, written when `keyboard.toml` is missing.
pub const KEYBOARD_TEMPLATE: &str = r#"# gth-play keyboard bindings (docs/human-play.md, "Bindings").
# Keys are winit KeyCode names: KeyA..KeyZ, Digit0..Digit9, ArrowUp, Enter, Space,
# ShiftLeft, ShiftRight, F1..F12, Escape, ...

[buttons]          # RetroPad button = [keys]
up = ["ArrowUp"]
down = ["ArrowDown"]
left = ["ArrowLeft"]
right = ["ArrowRight"]
b = ["KeyZ"]       # PC Engine II
a = ["KeyX"]       # PC Engine I
y = ["KeyA"]
x = ["KeyS"]
l = ["KeyQ"]
r = ["KeyW"]
select = ["ShiftRight"]
start = ["Enter"]

[hotkeys]          # one key each, not bound above
pause = "KeyP"
save_state = "Digit1"
load_state = "Digit2"
record = "KeyR"
quit = "Escape"
"#;

/// The gamepad template, written when a controller model has no file.
pub const GAMEPAD_TEMPLATE: &str = r#"# gth-play bindings for one controller model (docs/human-play.md, "Bindings").
# Buttons are gilrs names in the SDL layout: South (Xbox A), East (B), West (X), North (Y),
# LeftTrigger, RightTrigger (bumpers), LeftTrigger2, RightTrigger2, Select, Start, Mode,
# LeftThumb, RightThumb, DPadUp, DPadDown, DPadLeft, DPadRight.

deadzone = 0.5             # stick distance from centre (0-1) below which it is neutral
diagonal_degrees = 45.0    # width of each diagonal's sector; 45 makes all 8 equal
sticks = ["left"]          # sticks that drive the d-pad: "left", "right" or none ([])

[buttons]                  # RetroPad button = [controller buttons]
up = ["DPadUp"]
down = ["DPadDown"]
left = ["DPadLeft"]
right = ["DPadRight"]
b = ["South"]              # PC Engine II
a = ["East"]               # PC Engine I
y = ["West"]
x = ["North"]
l = ["LeftTrigger"]
r = ["RightTrigger"]
select = ["Select"]
start = ["Start"]
"#;

/// Parses `keyboard.toml`; `what` names it in errors.
pub fn parse_keyboard(text: &str, what: &str) -> Result<KeyboardBindings, String> {
    let bindings: KeyboardBindings =
        toml::from_str(text).map_err(|error| format!("invalid {what}: {error}"))?;
    let hotkeys = bindings.hotkeys.all();
    for (i, (key, name)) in hotkeys.iter().enumerate() {
        if let Some((_, other)) = hotkeys[..i].iter().find(|(k, _)| k == key) {
            return Err(format!(
                "invalid {what}: hotkeys {other} and {name} share {key:?}"
            ));
        }
        if let Some((button, _)) = bindings.buttons.iter().find(|(_, keys)| keys.contains(key)) {
            return Err(format!(
                "invalid {what}: {key:?} is both hotkey {name} and button {button:?}"
            ));
        }
    }
    Ok(bindings)
}

/// Parses a gamepad file; `what` names it in errors.
pub fn parse_gamepad(text: &str, what: &str) -> Result<GamepadBindings, String> {
    let bindings: GamepadBindings =
        toml::from_str(text).map_err(|error| format!("invalid {what}: {error}"))?;
    StickMap::new(bindings.deadzone, bindings.diagonal_degrees)
        .map_err(|error| format!("invalid {what}: {error}"))?;
    Ok(bindings)
}

/// The bindings directory: `bindings/` beside the user config file.
pub fn bindings_dir() -> Option<PathBuf> {
    Some(gth_config::user_config_path()?.parent()?.join("bindings"))
}

/// The file of a controller model: `gamepad-<name>.toml`, with every character but ASCII
/// letters, digits and `-` replaced by `_`.
pub fn gamepad_file(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("gamepad-{name}.toml")
}

/// Reads `path`, first writing `template` there if it doesn't exist (and saying so), then
/// parses it with `parse`.
pub fn load_or_create<T>(
    path: &Path,
    template: &str,
    parse: impl Fn(&str, &str) -> Result<T, String>,
) -> Result<T, String> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        std::fs::write(path, template)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        eprintln!(
            "gth-play: wrote default bindings to {}; edit it to change them",
            path.display()
        );
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse(&text, &path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_parse() {
        let keyboard = parse_keyboard(KEYBOARD_TEMPLATE, "template").unwrap();
        assert_eq!(keyboard.buttons[&RetroButton::B], [KeyCode::KeyZ]);
        assert_eq!(keyboard.hotkeys.quit, KeyCode::Escape);
        let gamepad = parse_gamepad(GAMEPAD_TEMPLATE, "template").unwrap();
        assert_eq!(gamepad.sticks, [Stick::Left]);
        assert_eq!(gamepad.buttons[&RetroButton::Start], [Button::Start]);
        assert_eq!(gamepad.stick_map(), StickMap::new(0.5, 45.0).unwrap());
    }

    #[test]
    fn masks_hold_every_bound_button() {
        let keyboard = parse_keyboard(KEYBOARD_TEMPLATE, "template").unwrap();
        let held = [KeyCode::KeyZ, KeyCode::ArrowUp, KeyCode::KeyP];
        assert_eq!(
            keyboard.mask(|key| held.contains(&key)),
            RetroButton::B.bit() | RetroButton::Up.bit(),
            "a hotkey presses nothing"
        );
        let gamepad = parse_gamepad(GAMEPAD_TEMPLATE, "template").unwrap();
        assert_eq!(gamepad.mask(|b| b == Button::East), RetroButton::A.bit());
    }

    #[test]
    fn rejects_clashing_hotkeys_and_bad_settings() {
        let shared = KEYBOARD_TEMPLATE.replace("record = \"KeyR\"", "record = \"KeyP\"");
        let error = parse_keyboard(&shared, "k").unwrap_err();
        assert!(error.contains("pause and record share KeyP"), "{error}");
        let bound = KEYBOARD_TEMPLATE.replace("quit = \"Escape\"", "quit = \"KeyZ\"");
        let error = parse_keyboard(&bound, "k").unwrap_err();
        assert!(error.contains("both hotkey quit and button B"), "{error}");
        let error = parse_keyboard("[buttons]\n", "k").unwrap_err();
        assert!(error.contains("hotkeys"), "{error}");
        let wide = GAMEPAD_TEMPLATE.replace("diagonal_degrees = 45.0", "diagonal_degrees = 95.0");
        let error = parse_gamepad(&wide, "g").unwrap_err();
        assert!(error.contains("diagonal_degrees"), "{error}");
        let missing = GAMEPAD_TEMPLATE.replace("deadzone = 0.5", "");
        assert!(
            parse_gamepad(&missing, "g").is_err(),
            "deadzone is required"
        );
    }

    #[test]
    fn gamepad_files_are_safe_names() {
        assert_eq!(
            gamepad_file("Xbox Wireless Controller"),
            "gamepad-Xbox_Wireless_Controller.toml"
        );
        assert_eq!(gamepad_file("8BitDo/Pro 2"), "gamepad-8BitDo_Pro_2.toml");
    }

    #[test]
    fn load_or_create_writes_the_template_once() {
        let dir = std::env::temp_dir().join(format!("gth-play-bindings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("bindings").join("keyboard.toml");
        let first = load_or_create(&path, KEYBOARD_TEMPLATE, parse_keyboard).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), KEYBOARD_TEMPLATE);
        let edited = KEYBOARD_TEMPLATE.replace("b = [\"KeyZ\"]", "b = [\"KeyV\"]");
        std::fs::write(&path, &edited).unwrap();
        let second = load_or_create(&path, KEYBOARD_TEMPLATE, parse_keyboard).unwrap();
        assert_ne!(first, second, "an existing file is kept");
        assert_eq!(second.buttons[&RetroButton::B], [KeyCode::KeyV]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
