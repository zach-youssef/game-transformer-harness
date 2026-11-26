//! The action space (docs/actions.md): an 8-way direction and the four button slots, as a
//! pair or one flat integer, the same in every game, and its mapping to the RetroPad and to
//! NitroGen's gamepad.

use std::collections::BTreeMap;

use crate::{RetroButton, Slot};

/// Neutral or one of 8 directions, numbered clockwise from up. Opposite d-pad directions
/// can't be expressed, so no action presses them together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Direction {
    Neutral,
    Up,
    UpRight,
    Right,
    DownRight,
    Down,
    DownLeft,
    Left,
    UpLeft,
}

impl Direction {
    pub const COUNT: u32 = 9;
    pub const ALL: [Direction; 9] = [
        Direction::Neutral,
        Direction::Up,
        Direction::UpRight,
        Direction::Right,
        Direction::DownRight,
        Direction::Down,
        Direction::DownLeft,
        Direction::Left,
        Direction::UpLeft,
    ];

    pub fn from_index(index: u32) -> Option<Direction> {
        Self::ALL.get(usize::try_from(index).ok()?).copied()
    }

    /// `(x, y)`, each -1, 0 or 1; y grows downwards, as on screen.
    pub fn delta(self) -> (i8, i8) {
        match self {
            Direction::Neutral => (0, 0),
            Direction::Up => (0, -1),
            Direction::UpRight => (1, -1),
            Direction::Right => (1, 0),
            Direction::DownRight => (1, 1),
            Direction::Down => (0, 1),
            Direction::DownLeft => (-1, 1),
            Direction::Left => (-1, 0),
            Direction::UpLeft => (-1, -1),
        }
    }

    /// The direction of a d-pad state; opposite presses cancel on their axis.
    pub fn from_dpad(up: bool, down: bool, left: bool, right: bool) -> Direction {
        let x = i8::from(right) - i8::from(left);
        let y = i8::from(down) - i8::from(up);
        *Self::ALL
            .iter()
            .find(|direction| direction.delta() == (x, y))
            .expect("every (x, y) in -1..=1 is a direction")
    }

    /// The RetroPad d-pad bits.
    pub fn joypad(self) -> u16 {
        let (x, y) = self.delta();
        let bit = |on: bool, button: RetroButton| if on { button.bit() } else { 0 };
        bit(y < 0, RetroButton::Up)
            | bit(y > 0, RetroButton::Down)
            | bit(x < 0, RetroButton::Left)
            | bit(x > 0, RetroButton::Right)
    }
}

/// One action: a direction and the held slots (bit i = `Slot::ALL[i]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Action {
    pub direction: Direction,
    pub buttons: u8,
}

/// A game's action space: `Discrete(9) x MultiBinary(4)` over the four slots in [`Slot`]
/// order (`shot`, `bomb`, `aux1`, `aux2`), the same layout in every game. A slot the game's
/// `buttons` table doesn't map presses nothing. The flat encoding is
/// `direction * 16 + buttons`, in `0..144`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSpace {
    /// The RetroPad bits each slot presses, indexed like [`Slot::ALL`]; 0 for an unmapped
    /// slot.
    slot_joypad: [u16; ActionSpace::N_BUTTONS as usize],
}

impl ActionSpace {
    /// The number of button bits: one per slot, mapped or not.
    pub const N_BUTTONS: u32 = Slot::ALL.len() as u32;
    /// The number of flat actions, `9 * 2^4`.
    pub const LEN: u32 = Direction::COUNT << Self::N_BUTTONS;

    /// The space of a game's `buttons` table (RetroPad button -> slot).
    pub fn new(buttons: &BTreeMap<RetroButton, Slot>) -> ActionSpace {
        ActionSpace {
            slot_joypad: Slot::ALL.map(|slot| {
                (buttons.iter())
                    .filter(|&(_, &s)| s == slot)
                    .fold(0, |mask, (button, _)| mask | button.bit())
            }),
        }
    }

    /// The slots the game maps to at least one button, in slot order; the others are no-ops.
    pub fn mapped_slots(&self) -> Vec<Slot> {
        (Slot::ALL.into_iter().zip(self.slot_joypad))
            .filter(|&(_, bits)| bits != 0)
            .map(|(slot, _)| slot)
            .collect()
    }

    /// The flat integer of `action`, or `None` if it sets a bit above the four slots.
    pub fn encode(&self, action: Action) -> Option<u32> {
        let buttons = u32::from(action.buttons);
        (buttons >> Self::N_BUTTONS == 0)
            .then_some((action.direction as u32) << Self::N_BUTTONS | buttons)
    }

    /// The action of a flat integer, or `None` if it is out of range.
    pub fn decode(&self, flat: u32) -> Option<Action> {
        Some(Action {
            direction: Direction::from_index(flat >> Self::N_BUTTONS)?,
            buttons: (flat & ((1 << Self::N_BUTTONS) - 1)) as u8,
        })
    }

    /// The RetroPad mask (bit = `RETRO_DEVICE_ID_JOYPAD_*`) that `action` holds.
    pub fn joypad(&self, action: Action) -> u16 {
        (self.slot_joypad.iter().enumerate())
            .filter(|&(i, _)| action.buttons >> i & 1 == 1)
            .fold(action.direction.joypad(), |mask, (_, bits)| mask | bits)
    }

    /// The action label of a RetroPad mask, the inverse of [`joypad`](Self::joypad) for
    /// training shards (docs/recording-format.md): the d-pad bits give the direction
    /// (opposites cancel on their axis) and a slot is held when any button mapped to it is.
    /// `None` when the mask holds a button no slot maps (start, select, an unmapped button),
    /// which no action can press.
    pub fn action_of(&self, joypad: u16) -> Option<Action> {
        let dpad = Direction::ALL
            .iter()
            .fold(0, |mask, direction| mask | direction.joypad());
        let mapped = self.slot_joypad.iter().fold(dpad, |mask, bits| mask | bits);
        if joypad & !mapped != 0 {
            return None;
        }
        let held = |button: RetroButton| joypad & button.bit() != 0;
        Some(Action {
            direction: Direction::from_dpad(
                held(RetroButton::Up),
                held(RetroButton::Down),
                held(RetroButton::Left),
                held(RetroButton::Right),
            ),
            buttons: (self.slot_joypad.iter().enumerate())
                .filter(|&(_, &bits)| joypad & bits != 0)
                .fold(0, |buttons, (i, _)| buttons | 1 << i),
        })
    }

    /// The RetroPad mask of every flat action, indexed by the flat integer, so a caller can
    /// map actions with one lookup.
    pub fn joypad_table(&self) -> Vec<u16> {
        (0..Self::LEN)
            .map(|flat| self.joypad(self.decode(flat).expect("in range")))
            .collect()
    }

    /// NitroGen's gamepad for `action`: the direction on the d-pad (sticks stay centred) and
    /// each held slot on its face button (docs/actions.md, "NitroGen").
    pub fn to_gamepad(&self, action: Action) -> Gamepad {
        let (x, y) = action.direction.delta();
        let held = |slot: Slot| action.buttons >> slot as u8 & 1 == 1;
        Gamepad {
            dpad_up: y < 0,
            dpad_down: y > 0,
            dpad_left: x < 0,
            dpad_right: x > 0,
            south: held(Slot::Shot),
            east: held(Slot::Bomb),
            west: held(Slot::Aux1),
            north: held(Slot::Aux2),
        }
    }

    /// The action for NitroGen's gamepad output: the d-pad resolved to 8-way (opposites
    /// cancel), each face button to its slot (a no-op if the game doesn't map it); sticks,
    /// shoulders and triggers are ignored.
    pub fn from_gamepad(&self, pad: &Gamepad) -> Action {
        let bit = |on: bool, slot: Slot| u8::from(on) << slot as u8;
        Action {
            direction: Direction::from_dpad(
                pad.dpad_up,
                pad.dpad_down,
                pad.dpad_left,
                pad.dpad_right,
            ),
            buttons: bit(pad.south, Slot::Shot)
                | bit(pad.east, Slot::Bomb)
                | bit(pad.west, Slot::Aux1)
                | bit(pad.north, Slot::Aux2),
        }
    }
}

/// The part of NitroGen's gamepad the harness uses: the d-pad and the four face buttons,
/// named by position. Its sticks, shoulders and triggers have no field: the harness neither
/// sets nor reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gamepad {
    pub dpad_up: bool,
    pub dpad_down: bool,
    pub dpad_left: bool,
    pub dpad_right: bool,
    pub south: bool,
    pub east: bool,
    pub west: bool,
    pub north: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP_DOWN: u16 = 1 << 4 | 1 << 5;
    const LEFT_RIGHT: u16 = 1 << 6 | 1 << 7;

    fn space(slots: &[Slot]) -> ActionSpace {
        let buttons = [
            RetroButton::A,
            RetroButton::B,
            RetroButton::Y,
            RetroButton::X,
        ];
        ActionSpace::new(&buttons.into_iter().zip(slots.iter().copied()).collect())
    }

    /// A space for every subset of mapped slots.
    fn spaces() -> Vec<ActionSpace> {
        (0..16u8)
            .map(|subset| {
                let slots: Vec<Slot> = (Slot::ALL.into_iter().enumerate())
                    .filter(|&(i, _)| subset >> i & 1 == 1)
                    .map(|(_, slot)| slot)
                    .collect();
                space(&slots)
            })
            .collect()
    }

    #[test]
    fn encode_and_decode_are_inverse_over_every_action() {
        assert_eq!(ActionSpace::LEN, 144);
        for space in spaces() {
            for flat in 0..ActionSpace::LEN {
                let action = space.decode(flat).unwrap();
                assert_eq!(space.encode(action), Some(flat));
            }
            assert_eq!(space.decode(ActionSpace::LEN), None);
            let extra = Action {
                direction: Direction::Up,
                buttons: 1 << 4,
            };
            assert_eq!(space.encode(extra), None, "a bit above the four slots");
        }
    }

    #[test]
    fn no_action_presses_opposite_directions_or_harness_buttons() {
        for space in spaces() {
            let table = space.joypad_table();
            assert_eq!(table.len() as u32, ActionSpace::LEN);
            for mask in table {
                assert_ne!(mask & UP_DOWN, UP_DOWN);
                assert_ne!(mask & LEFT_RIGHT, LEFT_RIGHT);
                assert_eq!(mask & (1 << 2 | 1 << 3), 0, "never select or start");
            }
        }
    }

    #[test]
    fn action_of_inverts_joypad_and_rejects_unmapped_buttons() {
        for space in spaces() {
            for flat in 0..ActionSpace::LEN {
                let action = space.decode(flat).unwrap();
                let label = space.action_of(space.joypad(action)).unwrap();
                assert_eq!(label.direction, action.direction);
                // An unmapped slot presses nothing, so its bit can't come back.
                let mapped = (space.mapped_slots().iter()).fold(0, |m, &s| m | 1 << s as u8);
                assert_eq!(label.buttons, action.buttons & mapped);
            }
            assert_eq!(space.action_of(1 << 3), None, "start");
            assert_eq!(space.action_of(1 << 2 | 1 << 4), None, "select with up");
            assert_eq!(space.action_of(1 << 10), None, "L, mapped in no space here");
        }
        let all = space(&Slot::ALL);
        let label = all.action_of(UP_DOWN | 1 << 7 | 1 << 8).unwrap();
        assert_eq!(label.direction, Direction::Right, "up and down cancel");
        assert_eq!(label.buttons, 1, "A is shot");
    }

    #[test]
    fn maps_slots_to_their_buttons_and_unmapped_slots_to_nothing() {
        let mut buttons = BTreeMap::from([
            (RetroButton::B, Slot::Bomb),
            (RetroButton::Y, Slot::Shot),
            (RetroButton::R, Slot::Shot),
        ]);
        let space = ActionSpace::new(&buttons);
        assert_eq!(space.mapped_slots(), [Slot::Shot, Slot::Bomb]);
        let action = |direction, buttons| Action { direction, buttons };
        let shot = RetroButton::Y.bit() | RetroButton::R.bit();
        assert_eq!(space.joypad(action(Direction::Neutral, 0b0001)), shot);
        assert_eq!(
            space.joypad(action(Direction::DownLeft, 0b0010)),
            RetroButton::B.bit() | RetroButton::Down.bit() | RetroButton::Left.bit()
        );
        assert_eq!(
            space.joypad(action(Direction::Up, 0b1100)),
            RetroButton::Up.bit(),
            "aux1 and aux2 are unmapped"
        );
        assert_eq!(
            space.encode(action(Direction::Right, 0b0011)),
            Some(3 * 16 + 3),
            "the layout doesn't depend on the mapped slots"
        );
        buttons.insert(RetroButton::X, Slot::Aux2);
        let space = ActionSpace::new(&buttons);
        assert_eq!(space.mapped_slots(), [Slot::Shot, Slot::Bomb, Slot::Aux2]);
        assert_eq!(
            space.joypad(action(Direction::Neutral, 0b1000)),
            RetroButton::X.bit()
        );
    }

    #[test]
    fn nitrogen_round_trips_and_resolves_the_dpad() {
        for space in spaces() {
            for flat in 0..ActionSpace::LEN {
                let action = space.decode(flat).unwrap();
                assert_eq!(space.from_gamepad(&space.to_gamepad(action)), action);
            }
        }
        let space = space(&[Slot::Shot, Slot::Aux2]);
        let pad = Gamepad {
            dpad_up: true,
            dpad_down: true,
            dpad_left: true,
            north: true,
            east: true,
            ..Gamepad::default()
        };
        let action = space.from_gamepad(&pad);
        assert_eq!(action.direction, Direction::Left, "up and down cancel");
        assert_eq!(action.buttons, 0b1010, "aux2, and bomb (a no-op here)");
        assert_eq!(
            space.joypad(action),
            RetroButton::Left.bit() | RetroButton::B.bit()
        );
        let shot = space.to_gamepad(Action {
            direction: Direction::UpRight,
            buttons: 0b0001,
        });
        let expected = Gamepad {
            dpad_up: true,
            dpad_right: true,
            south: true,
            ..Gamepad::default()
        };
        assert_eq!(shot, expected);
    }
}
