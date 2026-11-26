# Gunhed

Compile and Hudson's vertical shooter for the PC Engine (HuCard, 1989), the Japanese release
of Blazing Lazers, in its English-text Japanese dump (`Gunhed (Japan) (En).pce`). It runs on
`mednafen_pce_fast` with no BIOS. `game.toml` here is the config; this file says where each
part of it comes from, so it can be checked again (for example after a core bump).

## Buttons

The core maps RetroPad B to the PC Engine's button II and A to button I, and the game fires
with II and bombs with I, so `b` is the shot slot and `a` the bomb slot. Holding II fires on
its own (the game's autofire). Select is the speed change (shown as arrows under "SC"); the harness never lets a policy press Select or Start (docs/actions.md), so the ship
keeps its starting speed for the whole episode.

## Reset

From power-on: Run (Start) held for the first 20 frames skips the intro to the title, which
ignores Run until it has faded in (about frame 130); Run then starts a game, and Area 1 fades
in. The d-pad and button I first answer about 470 frames after power-on. The script ends at
frame 484, and `RetroEnv.reset` runs `frameskip` more frames, so a policy's first action
lands at frame 484 + `frameskip` (488 at frameskip 4) and counts.

## RAM variables

All in the 8 KB work RAM (`system_ram`, CPU $2000-$3FFF, RetroAchievements $1F0000-$1F1FFF).
Each was confirmed with `gth ram-search` (docs/games.md, "RAM search"), and the script that
does it is in `searches/`, named after the variable; each narrows the whole work RAM to that
variable's bytes alone, and `tests/test_gunhed.py` reruns them.

| Variable | Offset | Encoding | Source | Script |
|---|---|---|---|---|
| `score` | 0x36a | `bcd`, 4 bytes, most significant first | found by search | `searches/score.toml`: shooting from the reset, against the HUD (1300, 2300, 8210, 18750); the last needs five digits, which rules out the 2-byte view of 0x36c |
| `lives` | 0x37d | `le`, 1 byte | the libretro-database cheat files for Gunhed, Gunhed Special Version and Blazing Lazers (one source, three versions); confirmed by search | `searches/lives.toml`: no input from the reset, against the HUD (2, 1, 0) |
| `bombs` | 0x381 | `le`, 1 byte | the same cheat files; confirmed by search | `searches/bombs.toml`: one bomb from the reset, against the HUD (3, then 2) |
| `stage` | 0x90 | `le`, 1 byte | found by search | `searches/stage.toml`: savestates after the "AREA-1" to "AREA-4" banners (below) |
| `mode` | 0x375 | `le`, 1 byte | found by search | `searches/mode.toml`: no input from the reset through GAME OVER, CONTINUE and the next GAME OVER; the area-clear value from `states/area1-end.state` (below), in `tests/test_gunhed.py` |

What the values mean:

- `score` is the HUD's number. Its ones digit is always 0. A long invincible run read
  1042880 through it (the top byte 0x01), which fits 8 digits, but the HUD was checked only
  up to 18750, so the digit count is unverified.
- `lives` counts the ships in reserve, as the HUD does: 2 at the start, 0 while the last ship
  plays. So `lives == 0` isn't the end of the game, and `mode` is needed. Points add a
  ship (the first at 20000 in a shooting-only run).
- `bombs` is 3 at the start, rises with pickups and goes back to 3 when a ship is lost.
- `stage` is the area, from 1 (seen 1 to 5).
- `mode` is the game's mode byte, with three values seen:
  - 0 while an area plays;
  - 0x80 during an area clear: from the end of the boss, for about 135 steps at frameskip 4,
    while `stage` rises to the next area, then 0 again as it starts;
  - 0x40 from the GAME OVER text on, through the screen with the score and CONTINUE / GAME
    OVER, which waits for input. Choosing CONTINUE starts a new game from Area 1 with the
    score at 0 and clears it.

  So `terminate = "mode == 64"` ends the episode at the first GAME OVER, and the harness never
  continues. `mode != 0` would also end it at every area clear, which runs that only die
  don't show: check an end condition across a stage clear too.

There is no credit count: CONTINUE was always offered.

The cheat files also give an invincibility byte: 0x1310 = 0xff works (the Blazing Lazers
file's address); 0x310, the Gunhed files' address, does nothing. Only `stage.toml`'s
savestates use it, as below; it isn't a config variable.

## Scoring notes

`score_delta` (`gtharness.rewards`) pays the rise in `score` each step, so a policy is paid for
kills and pickups as the game scores them, in steps of 10. Losing a ship costs nothing directly
but ends the reward sooner, and the first GAME OVER ends the episode. The speed is fixed
(above), and continues are never used, so the score is a one-credit score.

## Savestates

Savestates come from the user's ROM, so they are never committed (`*.state` is git-ignored),
and the tests that need them skip until they are made locally, in `searches/states/`.

`stage.toml` (and its test) needs
`area1.state` to `area4.state`, each taken a few seconds into the area, after its "AREA-n"
banner. Any way of reaching the areas works, since the script only checks the value in each.
The ones used here were made by driving a `RetroEnv` with the invincibility byte (above) and
`lives` set to 9 written into the core's savestate, and saving 300 frames after `stage`
changed. With `gth-play` (docs/human-play.md), play to each area and save with `1`, then copy
`<out>/gunhed.state` to `searches/states/area<n>.state`.

`tests/test_gunhed.py`'s area-clear test needs `area1-end.state`: Area 1 with its boss
beaten or nearly so, about 600 frames before `mode` turns 0x80, so that the clear plays out
with no input (the test pins its steps for this file). The one used here came from the same
invincible run, played from `area1.state`, saved 590 frames before the clear. Made another
way, it needs the ship to survive to the clear with no input, and the test's pinned steps
change with it.

## Checking it again

With the ROM in `GTH_ROM_DIR` and the pinned core in `data/cores/` (`uv run gth cores fetch
data`), run each search, for example
`uv run gth ram-search gunhed data games/gunhed/searches/score.toml`, or the ROM tests:
`GTH_ROM_DIR=~/roms uv run pytest tests/test_gunhed.py`.
