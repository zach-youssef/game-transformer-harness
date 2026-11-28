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

`RetroEnv` takes a required `action_format`: `"dict"` gives the gymnasium space
`Dict({"direction": Discrete(9), "buttons": MultiBinary(4)})`, `"flat"` gives
`Discrete(144)`. `GameConfig.encode` and `decode` convert between the two, and
`GameConfig.mapped_slots` lists the slots that do something in that game.

Coin (`select`) and `start` are never in the action space: the harness presses them in reset
scripts only (docs/games.md, "Reset"). Sticks, `select`/`start` and the d-pad as separate
buttons are never exposed either.

The mapping to the libretro joypad mask is Rust: `ActionSpace::joypad_table` gives the mask
of every flat action, so a step maps its action with one lookup.

## Labels from masks

`ActionSpace::action_of(mask)` is the inverse mapping, used to label training shards from
recordings (docs/recording-format.md, "Training shards"): the d-pad bits give the direction,
opposites cancelling on their axis, and a slot is held when any RetroPad button mapped to it
is. A mask holding a button no slot maps (start, select, a button the game doesn't map) has
no label (`None`): no action presses it. A slot the game doesn't map never comes back set,
so `action_of(joypad(a))` is `a` with its unmapped slots cleared.

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

## Atari

ALE numbers the Atari 2600's actions 0-17: a joystick direction with or without its one fire
button. The harness maps them onto direction and `shot` (`ActionSpace::to_atari` and
`from_atari`), for Atari-trained policies and the `atari_full` and `atari_minimal` action
sets. The order is ALE's action enum, taken from ALE's documentation
(ale.farama.org/environments, "Action Space"); the research notes on machado2018revisiting
and mnih2015human give only the set's size (18, or a game's minimal 4-18):

| ALE | Name | Direction | `shot` |
|---|---|---|---|
| 0 | `NOOP` | 0 neutral | no |
| 1 | `FIRE` | 0 neutral | yes |
| 2 | `UP` | 1 up | no |
| 3 | `RIGHT` | 3 right | no |
| 4 | `LEFT` | 7 left | no |
| 5 | `DOWN` | 5 down | no |
| 6 | `UPRIGHT` | 2 up-right | no |
| 7 | `UPLEFT` | 8 up-left | no |
| 8 | `DOWNRIGHT` | 4 down-right | no |
| 9 | `DOWNLEFT` | 6 down-left | no |
| 10 | `UPFIRE` | 1 up | yes |
| 11 | `RIGHTFIRE` | 3 right | yes |
| 12 | `LEFTFIRE` | 7 left | yes |
| 13 | `DOWNFIRE` | 5 down | yes |
| 14 | `UPRIGHTFIRE` | 2 up-right | yes |
| 15 | `UPLEFTFIRE` | 8 up-left | yes |
| 16 | `DOWNRIGHTFIRE` | 4 down-right | yes |
| 17 | `DOWNLEFTFIRE` | 6 down-left | yes |

- `from_atari` never sets `bomb`, `aux1` or `aux2`: an Atari-trained policy can't press
  them, so its score on a game that needs a bomb is a lower bound.
- `to_atari` maps `shot` to fire whether the game maps it or not, drops an unmapped `bomb`,
  `aux1` or `aux2` (they press nothing), and returns `None` for an action that holds a mapped
  `bomb`, `aux1` or `aux2`, which no Atari action expresses. In a game that maps only `shot`,
  every harness action has an Atari action.
- Every Atari action survives `to_atari(from_atari(a))`, and every harness action without a
  mapped non-`shot` slot comes back from `from_atari(to_atari(a))` with its direction,
  `shot` and the same RetroPad mask (tested exhaustively for every subset of mapped slots).
- A game's minimal action set (ALE's `getMinimalActionSet`) is a list of these numbers.
  `ActionSpace::atari_mask(set)` turns it into a mask over the 18, for masking an 18-way
  policy head.
