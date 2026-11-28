# Human play (`gth-play`)

`gth-play` (`crates/gth-play`) plays a game in a window, with sound and a gamepad or the
keyboard, and records what you play through the same `gth-record` writer as `RetroEnv`
(docs/recording-format.md), with `recorder = "human"`. It runs the core at its own rate
(frameskip 1) and shows the core's own frame: the model's resolution is applied only when the
recording is exported to shards, never while playing.

Only this crate depends on windowing, audio and controller libraries. `gth-core` stays
headless; the player's headless half (`gth_play::session`, `input`, `bindings`, `display`,
`audio`, `latency`, `smoke`) is a library that the tests drive without a window, and the
binary (`src/bin/gth-play/`) only adds the window, the audio device and the controllers.

## Playing and recording one credit of Gunhed on the Mac

1. Build it (release: a debug build is too slow to hold 60 fps on some cores):

   ```sh
   cargo build --release -p gth-play      # target/release/gth-play
   ```

2. Put the pinned core builds in a data directory (docs/roms.md, "Data directory"):
   `gth cores fetch <data_dir>` (docs/cores.md) puts `mednafen_pce_fast_libretro.dylib`, the
   PC Engine core `games/gunhed/game.toml` names, in `<data_dir>/cores/`. Gunhed is a HuCard
   and needs no BIOS. Pass `--core PATH` instead to use a core elsewhere; a core that isn't
   the pinned build plays, but its recordings don't keep the pin (step 4).

3. Point the harness at your ROMs: `GTH_ROM_DIR=~/roms`, `rom_dir` in
   `~/.config/gtharness/config.toml`, or `--rom-dir ~/roms` (docs/roms.md). The ROM is found
   by its hash, so the file name doesn't matter.

4. Play:

   ```sh
   target/release/gth-play gunhed --games-dir games --data-dir <data_dir> --lock cores.lock \
       --out <recordings_dir> --player <your name> --license CC-BY-4.0 --checks-every 60 \
       --rewind-seconds 3
   ```

   Every recording option is required, as in `RetroEnv`'s `Record`: who recorded and under
   which license is never a default. `--checks-every` is how often (in frames) the recording
   stores a frame hash for the desync check (docs/recording-format.md). `--rewind-seconds`
   is how far a death or the rewind hotkey goes back (below, "Rewind"), `0` for never, at
   most 600 and at least one frame otherwise; it decides which frames the recording keeps,
   so it is required too.

   `--lock` is the `cores.lock` that pins the core builds (docs/cores.md), normally the
   checkout's. When the core file is the build it pins for this platform (by SHA-256, as
   `gth cores fetch` puts it in place), every recording keeps the core's lock entry as
   `[core.lock]`, saying exactly which build made it, as `RetroEnv`'s recordings do. At start
   it prints which: the pinned commit, or a warning that the core isn't the pinned build and
   recordings won't keep `[core.lock]` (they still replay on that same file). Record datasets
   with the pinned core. A missing or broken lock file is an error.

   On the first run it writes the keyboard bindings to `~/.config/gtharness/bindings/keyboard.toml`
   and prints the path; a gamepad gets its own file the first time it connects (below).

5. Record one credit:

   - Get to where the recording should start (the title screen, or after inserting the
     credit), then press **R**. A recording always starts from a savestate of that moment,
     written into the recording, so it replays without the menus before it.
   - Play the credit. The title shows `[REC n]` with the frames recorded.
   - When you lose a ship, play rewinds 3 seconds and pauses: press **P** to try that
     stretch again (below, "Rewind"). **Backspace** rewinds by hand.
   - Press **R** again to finish it (or **Escape** to finish it and quit). The path is
     printed: `<recordings_dir>/gunhed-<UTC time>-<pid>-<n>.gthrec`.

6. Check it replays with no desync:

   ```sh
   target/release/gth-play --verify <recordings_dir>/gunhed-...gthrec --data-dir <data_dir>
   ```

   It prints `N frames replayed, no desync`, or fails at the first frame whose hash differs.

### Hotkeys

| Key (default) | Does |
|---|---|
| `R` | Start a recording from the current state, or finish the running one |
| `1` | Save the practice state (kept in memory and in `<out>/<game>.state`); a recording goes on |
| `2` | Load the practice state (or the file from an earlier session). A recording can't hold a jump, so a running one is finished first. While paused, the screen keeps the old picture until the next frame runs (a loaded state carries no picture) |
| `P` | Pause (the core and sound stop; a recording simply gets no frames while paused) |
| `Backspace` | Rewind the running recording `--rewind-seconds` and pause (below, "Rewind"); `rewind` in `keyboard.toml`, Backspace when a file from before rewinds has no entry |
| `Escape` | Finish any recording and quit |

Every way of quitting finishes a running recording first, so it is never left incomplete:
`Escape`, closing the window, **Cmd+Q** (macOS; the recording is closed in winit's
`exiting` hook, since AppKit ends the process right after it), **Ctrl+C** in the terminal,
`kill` (SIGTERM) and closing the terminal (SIGHUP); on Windows, the console's Ctrl+C, close,
logoff and shutdown. Those signals only ask the loop to exit; a second one exits at once
(and loses the recording), in case the game is stuck. A panic unwinding out of the loop
finishes the recording too. Only `kill -9` or a crash in the core loses it.

Hotkeys are keyboard keys even when playing on a gamepad, and they never reach the core.

### Rewind

While recording, a death rewinds the game `--rewind-seconds` (3 s is 180 frames at 60 fps,
rounded to whole frames) and pauses, so you can play that stretch again; press pause to go
on. The hotkey does the same by hand, for a mistake that costs no life. The idea is one
credit's recording that plays better than you did in one go, plus the deaths as labelled
examples. No bomb is used for you: bomb after a rewind yourself if it makes sense.

- **A death** is the game's `lives` RAM variable (`[ram.lives]`, docs/games.md) going down
  between two frames. It is read by the player only, to decide when to rewind; nothing read
  from RAM goes into the observations. A game without `lives` (or whose `lives` the core's
  memory doesn't hold) rewinds on the hotkey only, and `gth-play` says so at start:
  `rewind: MANUAL ONLY ...` and why. `--rewind-seconds 0` turns both off. On Gunhed,
  `lives` counts the spare ships, so losing the last ship (`lives` 0, then game over) is no
  drop and doesn't rewind. Many shmups drop `lives` at the respawn, after the explosion, not
  at the hit: there 3 s may go back only 1-2 s before the hit, so pick `--rewind-seconds`
  for the game.
- **The recording stays one run.** A rewind from frame D to frame B = D - rewind cuts the
  recording's inputs and checks back to B and goes on from the state at B, so it replays from
  its start state as one deterministic run (`--verify` checks it like any other) and lists
  the rewind in `[[rewind]]` (docs/recording-format.md). B stops at the recording's start,
  and 1 + `settle_frames` (docs/games.md) frames after the oldest savestate the ring buffer
  (below) holds: a loaded savestate has no picture until a frame runs, so landing on one
  would make the check at B (finishing right after a rewind) hash a blank frame the replay
  doesn't show, and the first frame after a settled load shows what settling ran rather than
  the recording's picture.
- **Every rewind keeps a clip**: frames B to D as their own recording,
  `<recording>-rewind-<nnn>.gthrec` beside it, from the state at B, ending on the frame where
  `lives` dropped. Its `[branch]` names the recording it came from, B, and the cause (`lives`
  or `manual`). A second death inside the rewind window rewinds past the first branch: both
  clips stay, and the first's B is on the timeline the second rewind replaced.
- **Ring buffer**: a savestate every `SNAPSHOT_EVERY` = 30 frames while recording (and one
  at its start), keeping `RING_REWINDS` = 4 rewinds' worth (with 3 s, 25 savestates). A
  rewind loads the latest savestate at least 1 + `settle_frames` frames before B (or the
  start savestate, which serves any B: the recording began from that same settled load) and
  replays the logged inputs forward to B, then
  records the clip from there to D, then seeks to B again. Measured on Gunhed
  (mednafen_pce_fast, release, M-series Mac): a savestate is 80.6 KB and takes 0.009 ms to
  take and 0.07 ms to load, and a frame runs in 0.15-0.17 ms (about 6000 fps), so the ring
  costs 2 MB and under 0.001 ms a frame, and a 3 s rewind took 38 ms (the two replays of up
  to 30 frames are under 5 ms each; the clip's 180 frames and its checks are most of it). A
  savestate every frame would be as cheap to take but 30 times the memory, and a coarser
  grid only lengthens the replay, which is already well inside a frame's time at this
  interval. At start `gth-play` prints the ring's size: up to 4 x `--rewind-seconds` of
  savestates, so the 600 s cap is about 380 MB on Gunhed.
- A failed clip (a full disk, or its name taken) leaves the game where it was (replayed to
  D from the ring, so its picture is drawn), removes the clip if it made it, and warns; play
  goes on unrewound. A recording that can't be rewritten stops and stays incomplete, as any
  failed write does. A ring savestate that fails warns too, and rewinds reach less far.

## Reference credits

A reference credit is one credit played by a person under an evaluation protocol's rules:
it starts where the protocol's episodes start, the game's reset, and runs to game over, with
no rewinds. Record one with:

```sh
target/release/gth-play gunhed --games-dir games --data-dir <data_dir> --lock cores.lock \
    --out <recordings_dir> --player <your name> --license CC-BY-4.0 --checks-every 60 \
    --rewind-seconds 0 --record-from-reset
```

- `--record-from-reset` runs the game's `[reset]` as `RetroEnv.reset` does (the state just
  after loading, then the reset script, or the reset savestate from
  `<data_dir>/states/<game_id>/`, checked against its SHA-1), starts the recording there at
  once and pauses: press **P** to play. Pressing **R** later records from wherever play is,
  which is not a reference credit.
- `--rewind-seconds 0`: a rewound recording (`[[rewind]]`) or a clip (`[branch]`) is not a
  single credit, so `--record-from-reset` refuses any other length at launch. Loading the
  practice state (**2**) finishes the recording, so the rest of play is not in it.
- Play one credit to game over and keep playing a moment past it, then press **R** (or
  **Escape**). The score is read where the game's `episode.terminate` first holds, so what
  comes after doesn't count; a recording that stops before game over is refused.

## Bindings

One TOML file per controller in `<config dir>/bindings/`, next to the user config file
(`GTH_CONFIG`'s directory if set, else `~/.config/gtharness/`, or `%APPDATA%\gtharness\` on
Windows). A missing file is written from a commented template on first use, so the defaults
are always visible and editable in one place:

- `keyboard.toml`: RetroPad button = winit key names (`ArrowUp`, `KeyZ`, ...), and the
  hotkeys, one key each. A hotkey can't share a key with another hotkey or a button; the file
  is rejected with the clash named.
- `gamepad-<name>.toml`, one per controller model (`<name>` is the name the OS reports, with
  everything but letters, digits and `-` replaced by `_`): RetroPad button = gilrs button names
  (SDL layout: `South` is Xbox A), plus how the sticks drive the d-pad:
  - `deadzone` (0 to 1): stick distance from the centre below which it is neutral;
  - `diagonal_degrees` (0 to 90): the width of each diagonal's sector (45 makes all eight
    directions equal; narrower makes diagonals harder to hit by accident);
  - `sticks`: which sticks drive the d-pad (`["left"]`, `["left", "right"]` or `[]`).

  None of these has a right value for every stick, so they are required in the file rather
  than hidden defaults. The file's `deadzone` is the only one: gilrs's own per-axis deadzone
  filter, whose size the platform picks, is turned off (its hat-to-d-pad and jitter filters
  stay on), so a small deadzone works and the sectors stay radial down to it.

The keyboard, every gamepad and every stick are OR-ed into one RetroPad mask. A stick only
ever becomes d-pad bits: recordings store the 8-way d-pad a player could press, never an
analog value. Opposite directions held together (left and right on the keyboard, a stick and
the d-pad) cancel, so no recording holds both.

The default layout for the PC Engine: `X` / `East` is button I (RetroPad A), `Z` / `South`
is button II (RetroPad B), `Right Shift` is Select and `Enter` is Run (Start).

## Display

The window opens at the largest integer scale of the shown size that fits 90% of the screen,
and keeps integer scaling (centred, black borders) when resized. The shown image is the core's
frame with the game's `crop` (docs/games.md) applied, rotated for a vertical (tate) game
(`orientation` in `game.toml`), so the player sees what the model sees, minus the
resolution. The crop is display-only: recordings store inputs, not frames, and frames are
re-rendered from the core at export (docs/recording-format.md), so what the window shows
never changes a recording. (This is a deliberate choice over showing the core's whole frame:
the player shouldn't act on what the model can't see.)

The frame loop is paced by a timer at the core's own rate (`av_info().fps`). Softbuffer has no
vsync; macOS composites the window at the display's refresh, so a frame shows at the next
refresh after it is presented. When the loop falls more than a frame behind (the window was
dragged, the machine stalled), it skips ahead instead of running frames in a burst, and counts
a late frame (printed at exit).

## Audio

The core's samples go to the default output device through cpal, resampled (linearly) to the
device's rate. The core's clock and the sound card's drift apart, so the resampling ratio is
nudged by at most 0.5% to keep about 50 ms queued: too little would underrun, too much would
add latency. No frames are dropped or doubled for audio, so the game runs exactly at the
recorded rate. Without an output device the game plays silently. The underruns (in audio
frames) are printed at exit.

## Input latency

Latency from a press to its photons is the sum of:

1. **Waiting for the next frame**: input is read when a frame starts, so a press waits 0 to 1
   frame (0 to 16.7 ms at 60 fps) for the frame that sees it.
2. **The game's own frames of latency**: how many frames the game takes to show a press.
   `gth-play --measure-latency` measures it for each button from a savestate, by comparing
   frames run with and without the press (the idle run is run twice first, to check it is
   deterministic):

   ```sh
   gth-play --measure-latency gunhed --games-dir games --data-dir <data_dir> \
       --state <out>/gunhed.state --buttons a,b,up --frames 10
   ```

   Save the state (`1`) where a press visibly changes the screen (the ship in play). The test
   core shows a press on the frame it is pressed (1 frame:
   `crates/gth-play/tests/testcore.rs`); a real game typically takes 1 to 3.
3. **Frame run to present**: converting, drawing and presenting the frame, measured per frame
   and printed at exit (`frame run to present: p50 ...`). Each press that changes the mask
   is also timed from its window event to the present of the frame that took it
   (`input event to present`), which is parts 1 and 3 together. A key is timed from its
   window event; a gamepad press is timed from when the loop polls gilrs, at the start of
   the frame that takes it, so its number leaves out part 1 and understates by up to a
   frame.
4. **The display**: the compositor and the monitor, which the player can't see. Measure
   it end to end with a phone's slow-motion camera filming a key or button and the screen:
   count the camera frames from the press to the change, and subtract parts 1 to 3.

Measured on the Mac (Apple silicon, release build, testcore, a 3440x1440 display, window at
the default scale, `--quit-after 600`): frame run to present p50 5.5 ms, p95 6.3 ms; almost
all of it is softbuffer's `present` (about 4.6 ms: the copy into the window's surface), with
conversion 0.05 ms and the integer-scaled draw 0.6 ms. With the test core's 1 frame (the
frame run with the press shows it), a press reaches the screen 0 to 16.7 ms (part 1) plus
about 5.5 ms (part 3) after it happens, before the display's own latency. Each further frame
the game takes adds 16.7 ms. Run-ahead (hiding the game's own frames of latency) is out of scope.

`--quit-after FRAMES` quits after that many frames, for unattended timing runs like this one.

## Headless smoke

`gth-play --headless-smoke --core <testcore> --games-dir games --out DIR` runs the player's
frame loop without a window or devices: the test core, 600 frames of scripted input (a stick
turning through all eight directions in and out of the deadzone, held and tapped buttons,
opposite directions together), audio taken every frame, every frame converted and drawn into
an 800x600 buffer, and the hotkeys mid-recording (save, pause and a frame that doesn't run).
It records from a savestate after 30 frames of lead-in, as a player would, and prints the
recording's path. The recording is labelled `recorder = "scripted"`, since no human played
it. `crates/gth-play/tests/testcore.rs` runs it, checks the stored masks are exactly the
script's, and replays it with zero desyncs, through `gth_record::Replayer` and through
`gth-play --verify`. CI runs it on Linux, macOS and Windows (`cargo test`); CI opens no window.

## Choice of libraries

| | winit + softbuffer + cpal + gilrs (chosen) | SDL2 through `sdl2-sys` | minifb |
|---|---|---|---|
| Build | Pure Rust crates; Linux needs only `libasound2-dev` and `libudev-dev` headers (X11 and Wayland are loaded at run time) | Needs SDL2 built or installed on every OS (or the `bundled` feature, which builds SDL with CMake), and `unsafe` FFI for everything | Pure Rust, small |
| Window, scaling | winit: high-DPI, fullscreen, resize, monitor size; we do integer scaling ourselves into softbuffer | All of it, plus GPU scaling and vsync | A window and a buffer; fixed scale modes |
| Vsync | None in softbuffer: timer pacing, the compositor syncs | Yes (renderer) | None |
| Audio | cpal: every OS's native API | SDL audio | None: would still need cpal |
| Gamepads | gilrs: XInput, SDL's mapping database, hotplug | SDL's game controller API (the reference) | None: would still need gilrs |
| Keyboard | Physical key codes (layout-independent) | Scancodes | Keys, coarse |
| Maintenance | All actively maintained and widely used | SDL2 is maintained but superseded by SDL3; the Rust bindings lag | Maintained, few features |

Recommendation and choice: **winit + softbuffer + cpal + gilrs**. It builds with `cargo` alone
on all three OSes with no native library to install or bundle and no `unsafe` of our own, and
each crate is the standard one for its job. Its only real loss against SDL2 is vsync, which a
compositing desktop (macOS, Windows, most Wayland) mostly provides anyway; if tearing or pacing
proves a problem, the display half can move to a GPU surface (`wgpu`) without touching the
rest. minifb would need cpal and gilrs anyway and lacks monitor and DPI queries.

## Tests

- `crates/gth-play/src/*`: unit tests of the stick mapping, opposite cancelling, the
  bindings files (templates, clashes, bad values), blitting and scaling, the audio rate control
  and resampler, recording names and the latency summary.
- `crates/gth-play/tests/rewind.rs`: rewinds on the test core's lives byte
  (docs/libretro.md): an automatic rewind on a drop, with the spliced recording's inputs,
  `[[rewind]]` and replay checked and the clip replaying from the recording's state at the
  branch to the death; the hotkey on a game without `lives` and `gth-play --verify` on the
  result; a rewind stopping at the recording's start; a death one frame past the window,
  branching at frame 1 from the start savestate; a second death inside the window;
  rewinds stopping 1 + `settle_frames` frames after the ring's oldest savestate (the testcore
  game settles, with the test core's lag option on), and a recording finished there at
  once; and a failed clip that warns and leaves play to go on. All recordings, clips
  included, replay without desync. No ROM test: ROM tests are Python (docs/testing.md) and the session isn't
  bound to Python; a Gunhed death was rewound and verified by hand (the measurements above).
- `crates/gth-play/tests/testcore.rs`: the headless smoke and its replay, the session's
  hotkeys keeping recordings whole (pause adds no frame, loading a state finishes the
  recording, the next one starts from the loaded state), `[core.lock]` kept only with the
  pinned core (the test core standing in for a pinned build, in a lock written with its
  hash), `--rewind-seconds` refused past 600 (main.rs unit-tests the bounds and a length
  under one frame), a recording from the reset (`--record-from-reset`: its start state is a
  fresh core's after the reset script, whatever was played before; it starts paused; main.rs
  unit-tests that it refuses rewinds), and the latency probe on the test core.
- The window, the audio device and gamepads are checked by hand: CI has none of them.
