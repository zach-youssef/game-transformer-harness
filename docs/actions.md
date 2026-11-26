# Action space

Every game has the same action space (`gth_config::ActionSpace`,
`crates/gth-config/src/actions.rs`); only what its slots press comes from the game's
`[buttons]` table (docs/games.md):

- **direction**: `Discrete(9)`, neutral or one of 8 directions, numbered clockwise from up:
  0 neutral, 1 up, 2 up-right, 3 right, 4 down-right, 5 down, 6 down-left, 7 left, 8 up-left.
  It drives the RetroPad d-pad, so opposite directions (up with down, left with right) can't
  be pressed together: no action expresses them.
- **buttons**: `MultiBinary(4)`, one bit per slot, always in the order `shot`, `bomb`,
  `aux1`, `aux2` (bit 0 is always shot, bit 3 always aux2). Each slot presses the RetroPad
  buttons mapped to it; a slot the game doesn't map presses nothing.

The same space is also one flat integer, `direction * 16 + buttons`, in `0..144`, for
policies with a single categorical head.

Coin (`select`) and `start` are never in the action space: the harness presses them in reset
scripts only (docs/games.md, "Reset"). Sticks, `select`/`start` and the d-pad as separate
buttons are never exposed either.

The mapping to the libretro joypad mask is Rust: `ActionSpace::joypad_table` gives the mask
of every flat action, so a step maps its action with one lookup.

## Slots across games

Slots name a role, not a button: `shot` is the main fire button in every game, `bomb` the
bomb or special, `aux1` and `aux2` whatever else the game needs (a speed change, a second
fire type), chosen per game in its `[buttons]` table.

The layout is fixed so that one policy head fits every game (the cross-game convention):
bit i and flat action k mean the same slot in every game, whichever slots the game maps.
The price is redundant actions: in a game without `aux2`, an action and the same action
with `aux2` held do the same thing. A policy can mask them with `mapped_slots` if it wants
to; the env doesn't.

## NitroGen

NitroGen's gamepad has a d-pad, two sticks, four face buttons named by position, shoulders
and triggers. The harness maps its actions onto that gamepad and back
(`ActionSpace::to_gamepad` and `from_gamepad`, used by the NitroGen bridge, P9):

| Harness | NitroGen gamepad |
|---|---|
| direction | d-pad only; both sticks centred |
| `shot` | South (A on Xbox, Cross on PlayStation) |
| `bomb` | East (B, Circle) |
| `aux1` | West (X, Square) |
| `aux2` | North (Y, Triangle) |
| (none) | shoulders, triggers, stick clicks, start, select: never set |

The inverse, NitroGen output to harness action:

- the d-pad becomes the 8-way direction; opposite presses cancel on their axis (up + down +
  left is left);
- sticks, shoulders, triggers, start and select are ignored;
- a face button whose slot the game doesn't map sets that slot's bit, which does nothing.

Every harness action survives the round trip through the gamepad unchanged (tested
exhaustively for every subset of mapped slots).
