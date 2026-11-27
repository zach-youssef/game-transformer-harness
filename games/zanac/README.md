# Zanac

Compile's vertical shooter for the NES (1986, Japan), in the Project Egg re-release dump
(`Zanac (Project Egg).nes`: an iNES file, mapper 2, 128 KB of PRG and CHR-RAM). It runs on
`fceumm` with no BIOS. `game.toml` here is the config; this file says where each part of it
comes from, so it can be checked again (for example after a core bump).

The header needs no override: the core loads the file as it is and the game plays, so
`core_options` is empty and the ROM file is untouched.

## Buttons

fceumm maps RetroPad B to the NES B button and A to A. Holding B fires the main weapon (a
pink shot) and holding A fires the special weapon (a blue ball, with the weapon the ship
starts with); with neither held the ship doesn't fire at all, so `b` is the shot slot and
`a` the bomb slot. Both can be held together. Select and Start are never pressed by a policy
(docs/actions.md).

## Reset

From power-on the title ignores Start until about frame 30; Start there opens a GAME START /
CONTINUE menu, and Start again (the cursor is on GAME START) loads Area 1. The ship appears
and first answers the d-pad about 50 frames after that second Start is released, under the
"AREA 1" banner (the play flag below turns 1 at 200 frames in). The script ends at frame 126,
and `RetroEnv.reset` runs `frameskip` more, so a policy's first action lands at frame 126 +
`frameskip` and moves the ship (`tests/test_zanac.py` checks this at frameskip 4). Two
presses are needed: one at the title only opens the menu.

## RAM variables

All in the 2 KB work RAM (`system_ram`, CPU $0000-$07FF). Each was found by `gth ram-search`
(docs/games.md, "RAM search") with the script in `searches/` named after it, except the
score, which a search can't find (below); `tests/test_zanac.py` reruns those scripts.

| Variable | Offset | Encoding | Source | Script |
|---|---|---|---|---|
| `score` | 0x17c | `digits`, 6 bytes, most significant first | found by dumping the work RAM against the HUD (below) | none |
| `lives` | 0x32 | `le`, 1 byte | found by search | `searches/lives.toml`: no input from the reset, against the HUD (2, 1, 0 for 3, 2, 1) and GAME OVER |
| `stage` | 0x7d | `le`, 1 byte | found by search; 0x91 holds the same value | `searches/stage.toml`: savestates after the "AREA 1" to "AREA 3" banners (below) |
| `playing` | 0x31a | `le`, 1 byte | found by search; 0x31b holds the same value | `searches/playing.toml`: from the reset, a savestate before the end of Area 1, and the two area savestates |

What the values mean:

- `score` is the HUD's number (shown at the top). The game keeps it as one decimal digit per
  byte, 0x17c the 100000s to 0x181 the ones, so no `bcd`, `le` or `be` view reads it; the
  `digits` encoding (docs/games.md, "RAM variables") does. Its ones digit is always 0. It
  was found by running from the reset, reading the HUD (0, 60, 230) and looking for bytes
  holding those digits in the RAM dumped at those moments: only 0x17c-0x181 do. Values up
  to 5 digits were seen (the HUD read 23230 when the digits were 0, 2, 3, 2, 3, 0); 0x17b
  and below were always 0, so the sixth digit and a seventh are unverified.
- `lives` counts the ships left including the one flying: 3 at the start, one less per
  death, and 0 at GAME OVER. The HUD (bottom right) shows one less, so it reads 2, 1, 0
  while `lives` is 3, 2, 1. `terminate = "lives ==
  0"` ends the episode when the last ship is lost, at GAME OVER, and the harness never
  continues.
- `stage` is the area, from 1 (1 to 3 seen; the HUD shows it only in the "AREA n" banner
  at the start of each area).
- `playing` is the game's play flag: 0 under the "AREA n" banner at the start of an area (the
  first 200 frames from the reset), 1 while it plays, 0 again from the end of an area
  (about 600 frames before `stage` rises) until the next area's banner ends, and 0 from
  GAME OVER on. So `lives == 0` ends the episode, while `playing == 0` alone would also
  match every area clear: check an end condition across a stage clear too (docs/games.md,
  "Example: Gunhed"); `tests/test_zanac.py` plays one.

Not found: the main weapon's power level, the special weapon's number and stock, a boss
flag. Zanac's HUD shows none of them (only the score, the lives and the area banner), so
there was no number to search against. The numbered item boxes and the power chips change
them, and each death resets them. Finding them needs a run that collects items on purpose
(an item's number looks to be drawn as a digit sprite beside a box, in the OAM copy at $0200),
and a search script from savestates taken before and after each pickup. Holding A also
makes a counter at 0x37e run down, which may be the special weapon's timer.

## Scoring notes

`score_delta` (`gtharness.rewards`) pays the rise in `score` each step, so a policy is paid
for kills and pickups as the game scores them (60 for the first enemy, in steps of 10).
Losing a ship costs nothing directly but ends the reward sooner, and GAME OVER ends the
episode. Continues are never used, so the score is a one-credit score.

## Savestates

Savestates come from the user's ROM, so they are never committed (`*.state` is git-ignored),
and the tests that need them skip until they are made locally, in `searches/states/`.

`stage.toml` and `playing.toml` need `area1.state` to `area3.state`, each taken a few seconds
(300 frames) into the area, after its banner, and `playing.toml` and the test of the area
clear need `area1-end.state`: Area 1 about 660 frames before the play flag drops, so the
clear plays out with no input (the test pins its steps for this file). Any way of reaching
them works, since the scripts only check the values in each. The ones used here were made
by driving the core with no input from the reset, with `lives` set to 99 in the savestate
whenever it fell below 50 (the work RAM is inside the core's savestate, found by searching
its bytes for the dumped RAM; writing 99 at that offset + 0x32 and loading it back) and
saving at the right frame: area 1 300 frames after the banner, area 2 and 3 300 frames after
`stage` changed (Area 2 starts about 19500 frames in, Area 3 about 31400), and
`area1-end.state` at 18600. With `gth-play` (docs/human-play.md), play to each area and save
with `1`, then copy `<out>/zanac.state` to `searches/states/area<n>.state`; its
`area1-end.state` needs the clear to come with the ship alive, so the test's pinned steps
change with it.

## Checking it again

With the ROM in `GTH_ROM_DIR` and the pinned core in `data/cores/` (`uv run gth cores fetch
data`), run each search, for example
`uv run gth ram-search zanac data games/zanac/searches/lives.toml`, or the ROM tests:
`GTH_ROM_DIR=~/roms uv run pytest tests/test_zanac.py`.
