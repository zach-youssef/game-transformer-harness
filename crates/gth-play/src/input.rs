//! Controller state to the RetroPad mask the core sees and the recording stores.
//!
//! Sticks become d-pad bits here ([`StickMap`]), and opposite directions cancel
//! ([`clean_opposites`]), so a recording only ever holds the d-pad a player could press.

use gth_config::RetroButton;

/// The d-pad bits of the mask.
pub const UP: u16 = 1 << RetroButton::Up as u16;
pub const DOWN: u16 = 1 << RetroButton::Down as u16;
pub const LEFT: u16 = 1 << RetroButton::Left as u16;
pub const RIGHT: u16 = 1 << RetroButton::Right as u16;

/// How a stick drives the 8-way d-pad. Both values are the player's choice, from the
/// bindings file: there is no right deadzone for every stick, and the diagonal width trades
/// easy diagonals against accidental ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StickMap {
    deadzone: f32,
    diagonal_degrees: f32,
}

impl StickMap {
    /// `deadzone`: the stick's distance from centre (0 to 1) below which it is neutral, in
    /// `[0, 1)`. `diagonal_degrees`: the width of each diagonal's sector, in `(0, 90)`; the
    /// cardinal directions share the rest, so 45 makes all eight sectors equal.
    pub fn new(deadzone: f32, diagonal_degrees: f32) -> Result<StickMap, String> {
        if !(0.0..1.0).contains(&deadzone) {
            return Err(format!("deadzone must be in [0, 1), not {deadzone}"));
        }
        if !(diagonal_degrees > 0.0 && diagonal_degrees < 90.0) {
            return Err(format!(
                "diagonal_degrees must be in (0, 90), not {diagonal_degrees}"
            ));
        }
        Ok(StickMap {
            deadzone,
            diagonal_degrees,
        })
    }

    /// The d-pad bits for a stick at (`x`, `y`): `x` positive to the right, `y` positive up,
    /// each in `[-1, 1]` (as gilrs reports them). 0 inside the deadzone.
    pub fn dpad(&self, x: f32, y: f32) -> u16 {
        let distance = x.hypot(y);
        if !distance.is_finite() || distance < self.deadzone || distance == 0.0 {
            return 0;
        }
        // 0 degrees is right, 90 up, counter-clockwise.
        let angle = y.atan2(x).to_degrees().rem_euclid(360.0);
        const DIAGONALS: [(f32, u16); 4] = [
            (45.0, UP | RIGHT),
            (135.0, UP | LEFT),
            (225.0, DOWN | LEFT),
            (315.0, DOWN | RIGHT),
        ];
        for (centre, bits) in DIAGONALS {
            let off = (angle - centre).abs();
            if off.min(360.0 - off) < self.diagonal_degrees / 2.0 {
                return bits;
            }
        }
        const CARDINALS: [u16; 4] = [RIGHT, UP, LEFT, DOWN];
        CARDINALS[((angle / 90.0).round() as usize) % 4]
    }
}

/// Clears both bits of an opposite pair (left and right, up and down): a d-pad can't press
/// both, and cores and games disagree on what they do with it.
pub fn clean_opposites(mask: u16) -> u16 {
    let mut mask = mask;
    for pair in [LEFT | RIGHT, UP | DOWN] {
        if mask & pair == pair {
            mask &= !pair;
        }
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn polar(degrees: f32, distance: f32) -> (f32, f32) {
        let radians = degrees.to_radians();
        (distance * radians.cos(), distance * radians.sin())
    }

    #[test]
    fn bits_are_the_retropad_ids() {
        assert_eq!((UP, DOWN, LEFT, RIGHT), (1 << 4, 1 << 5, 1 << 6, 1 << 7));
    }

    #[test]
    fn deadzone_is_neutral() {
        let map = StickMap::new(0.5, 45.0).unwrap();
        assert_eq!(map.dpad(0.0, 0.0), 0);
        assert_eq!(map.dpad(0.49, 0.0), 0);
        assert_eq!(map.dpad(0.3, 0.3), 0, "distance 0.42");
        assert_eq!(map.dpad(0.5, 0.0), RIGHT);
        assert_eq!(map.dpad(f32::NAN, 1.0), 0);
        assert_eq!(StickMap::new(0.0, 45.0).unwrap().dpad(0.0, 0.0), 0);
    }

    /// The bindings file's deadzone is the only one (gilrs's per-axis one is off: the
    /// binary's `pads::builder`), and radial: a small deadzone takes deflections that a
    /// per-axis deadzone of the same size would drop on both axes.
    #[test]
    fn small_deadzones_are_radial() {
        let map = StickMap::new(0.1, 45.0).unwrap();
        assert_eq!(map.dpad(0.08, 0.08), UP | RIGHT, "each axis under 0.1");
        assert_eq!(map.dpad(0.0, -0.11), DOWN);
        assert_eq!(map.dpad(0.07, 0.07), 0, "distance 0.099");
        assert_eq!(StickMap::new(0.0, 45.0).unwrap().dpad(0.01, 0.0), RIGHT);
    }

    #[test]
    fn equal_sectors_at_45_degrees() {
        let map = StickMap::new(0.2, 45.0).unwrap();
        let expected = [
            (0.0, RIGHT),
            (22.0, RIGHT),
            (23.0, UP | RIGHT),
            (67.0, UP | RIGHT),
            (68.0, UP),
            (90.0, UP),
            (135.0, UP | LEFT),
            (180.0, LEFT),
            (225.0, DOWN | LEFT),
            (270.0, DOWN),
            (315.0, DOWN | RIGHT),
            (338.0, RIGHT),
            (359.0, RIGHT),
        ];
        for (degrees, bits) in expected {
            let (x, y) = polar(degrees, 1.0);
            assert_eq!(map.dpad(x, y), bits, "{degrees} degrees");
        }
    }

    #[test]
    fn narrow_diagonals_widen_the_cardinals() {
        let map = StickMap::new(0.2, 20.0).unwrap();
        let (x, y) = polar(30.0, 0.9);
        assert_eq!(map.dpad(x, y), RIGHT, "30 degrees is outside 45 +- 10");
        let (x, y) = polar(36.0, 0.9);
        assert_eq!(map.dpad(x, y), UP | RIGHT);
        let (x, y) = polar(-60.0, 0.9);
        assert_eq!(map.dpad(x, y), DOWN, "-60 is 300, outside 315 +- 10");
    }

    #[test]
    fn rejects_out_of_range_settings() {
        assert!(StickMap::new(1.0, 45.0).is_err());
        assert!(StickMap::new(-0.1, 45.0).is_err());
        assert!(StickMap::new(0.3, 0.0).is_err());
        assert!(StickMap::new(0.3, 90.0).is_err());
        assert!(StickMap::new(0.3, f32::NAN).is_err());
    }

    #[test]
    fn opposites_cancel() {
        let a = 1 << RetroButton::A as u16;
        assert_eq!(clean_opposites(LEFT | RIGHT | a), a);
        assert_eq!(clean_opposites(UP | DOWN | LEFT), LEFT);
        assert_eq!(clean_opposites(UP | LEFT), UP | LEFT);
        assert_eq!(clean_opposites(UP | DOWN | LEFT | RIGHT), 0);
    }
}
