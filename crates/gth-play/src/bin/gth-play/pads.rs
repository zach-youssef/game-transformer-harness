//! Game controllers through gilrs, each with its model's bindings file.

use std::collections::HashMap;
use std::path::PathBuf;

use gilrs::ev::filter::{Filter, Jitter, axis_dpad_to_button};
use gilrs::{Axis, EventType, GamepadId, Gilrs, GilrsBuilder};
use gth_play::bindings::{self, GamepadBindings, Stick};

pub struct Pads {
    gilrs: Gilrs,
    jitter: Jitter,
    dir: PathBuf,
    /// Bindings per connected controller; `None` when its file is invalid (it is ignored,
    /// having said why).
    bindings: HashMap<GamepadId, Option<GamepadBindings>>,
}

impl Pads {
    /// `None` (having said why) if the platform's controller support fails to start: the
    /// keyboard still plays.
    pub fn new(dir: PathBuf) -> Option<Pads> {
        match builder().build() {
            Ok(gilrs) => {
                let mut pads = Pads {
                    gilrs,
                    jitter: Jitter::new(),
                    dir,
                    bindings: HashMap::new(),
                };
                pads.load_new();
                Some(pads)
            }
            Err(error) => {
                eprintln!("gth-play: no controllers ({error}); the keyboard still plays");
                None
            }
        }
    }

    /// Loads the bindings of controllers seen for the first time.
    fn load_new(&mut self) {
        let new: Vec<(GamepadId, String)> = self
            .gilrs
            .gamepads()
            .filter(|(id, _)| !self.bindings.contains_key(id))
            .map(|(id, pad)| (id, pad.name().to_owned()))
            .collect();
        for (id, name) in new {
            let path = self.dir.join(bindings::gamepad_file(&name));
            let loaded = bindings::load_or_create(
                &path,
                bindings::GAMEPAD_TEMPLATE,
                bindings::parse_gamepad,
            );
            match &loaded {
                Ok(_) => eprintln!("gth-play: controller {name:?} uses {}", path.display()),
                Err(error) => eprintln!("gth-play: ignoring controller {name:?}: {error}"),
            }
            self.bindings.insert(id, loaded.ok());
        }
    }

    /// Takes the controllers' events; returns whether any could change the mask.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(raw) = self.gilrs.next_event() {
            // gilrs's own filters minus its per-axis deadzone (see `builder`).
            let Some(event) = Some(raw)
                .filter_ev(&axis_dpad_to_button, &mut self.gilrs)
                .filter_ev(&self.jitter, &mut self.gilrs)
            else {
                continue;
            };
            if event.is_dropped() {
                continue;
            }
            self.gilrs.update(&event);
            match event.event {
                EventType::Connected => self.load_new(),
                EventType::Disconnected => {
                    self.bindings.remove(&event.id);
                    changed = true;
                }
                EventType::ButtonPressed(..)
                | EventType::ButtonReleased(..)
                | EventType::ButtonChanged(..)
                | EventType::AxisChanged(..) => changed = true,
                _ => {}
            }
        }
        changed
    }

    /// The RetroPad bits every connected controller presses, sticks as d-pad bits.
    pub fn mask(&self) -> u16 {
        let mut mask = 0;
        for (id, pad) in self.gilrs.gamepads() {
            let Some(Some(bindings)) = self.bindings.get(&id) else {
                continue;
            };
            mask |= bindings.mask(|button| pad.is_pressed(button));
            let map = bindings.stick_map();
            for stick in &bindings.sticks {
                let (x, y) = match stick {
                    Stick::Left => (Axis::LeftStickX, Axis::LeftStickY),
                    Stick::Right => (Axis::RightStickX, Axis::RightStickY),
                };
                mask |= map.dpad(pad.value(x), pad.value(y));
            }
        }
        mask
    }
}

/// gilrs without its default filters, which include a per-axis deadzone chosen by the
/// platform: the bindings file's radial `deadzone` must be the only one, or a smaller one
/// would do nothing and the sectors would stop being radial near the centre. `poll` applies
/// the other two default filters itself (hat d-pads as buttons, and jitter), and updates the
/// state after them.
fn builder() -> GilrsBuilder {
    GilrsBuilder::new()
        .with_default_filters(false)
        .set_update_state(false)
}
