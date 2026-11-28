# Evaluation protocols

A score means something only next to the settings it was played under: frame skip, sticky
actions, the episode cap, how episodes start, how many were played. Published Atari numbers
differ in exactly these (sticky actions on in Machado et al., off in MGDT and JOWA; a cap of
18k or 108k frames), so the harness writes them all down. An evaluation protocol is a TOML
file that fixes every such setting, `gtharness.protocol` reads it, and every result stores
the protocol's canonical text and SHA-1 with it.

## The file

`gtharness.protocol.load(path)` reads a protocol into a frozen `EvalProtocol` (`loads(text)`
reads text). Every field is required: there are no defaults, so no result depends on a value
nobody wrote down. A missing field, an unknown field, a value of the wrong type or out of
range all raise `ConfigError` naming the file and the field (`protocols/x.toml: obs.size:
must have 2 integers, not 3`).

Loading checks the file alone. What needs a game or the env (the game exists, its content
SHA-1 matches its `game.toml`, it has the `score_var` RAM variable, the env supports the
setting) is checked when the protocol is used, by the runner (below, "Checks before a run").

### Fields

| Field | Values | Meaning |
|---|---|---|
| `backend` | `"libretro"` | What runs the games. Only libretro cores today; ALE runs as one (baseline-comparison plan, C10). |
| `games` | non-empty array of game ids, no repeats | The games evaluated, by their directory under `games/` (docs/games.md). Each game's content is found and checked by SHA-1 (docs/roms.md), so a protocol plus the game configs pins the exact dumps. Order is part of the protocol. |
| `frameskip` | integer >= 1 | Emulated frames per agent step, the action held for all of them. |
| `sticky_prob` | number in [0, 1) | Sticky actions with ALE's semantics: on each emulated frame, with this probability the previous frame's input repeats instead of the agent's. 0 turns them off. Written as a float in the canonical text (`0` and `-0.0` read as `0.0`). |
| `maxpool` | bool | Whether an observation is the pixel-wise maximum of a step's last two frames (removes sprite flicker), or its last frame alone. `true` needs `frameskip >= 2`, so both frames belong to the step. |
| `obs.size` | `[height, width]`, integers >= 1 | Observation size after crop and rotation. |
| `obs.crop` | `"game"` or `[x, y, width, height]` | The part of the core's frame kept, before rotation, in core pixels; `"game"` uses the game config's `crop` (docs/games.md). |
| `obs.orientation` | `"game"`, 0, 90 or 270 | Clockwise rotation; `"game"` uses the game config's. Multi-game protocols use `"game"`. |
| `obs.interpolation` | `gtharness.env.INTERPOLATIONS`: `"area"`, `"bilinear"` or `"bicubic"` | How frames are resized (the env's `interpolation`; `"bicubic"` is Pillow's `BICUBIC`, Catmull-Rom widened when shrinking, not OpenCV's `INTER_CUBIC`). The env, the shard exporter and protocols share the one tuple. |
| `obs.grayscale` | bool | Gray observations instead of RGB: one channel, `[H, W, 1]`, of BT.601 luma taken after max-pooling and before resizing; gray pixels keep their value. |
| `frame_stack` | integer >= 1 | How many observations the policy sees at once. The env doesn't stack: this is recorded for the policy, which must use this many. |
| `action_set` | `"harness"`, `"atari_full"`, `"atari_minimal"` | The policy's action space: the harness's direction + four slots (docs/actions.md), or Atari's 18 actions or a game's minimal set. |
| `start.reset` | `"game"` or a savestate's lowercase SHA-1 | Where an episode starts: the game config's reset (script or savestate), or a given savestate. |
| `start.noop_max` | integer >= 0, below `max_episode_frames` (the runner needs `noop_max + frameskip` below it) | Random no-op starts. 0: none, the first observation follows the reset directly. Otherwise each episode starts with N no-op frames (no input), N drawn uniformly from 1..noop_max with the env seed. This is the convention of OpenAI baselines' `NoopResetEnv`, which implements DQN's "up to 30 no-ops" (Mnih et al. 2015, p. 6) and which IRIS and many Atari codebases copy. N counts emulated frames, as `NoopResetEnv` does (it wraps the frame-skip-free env); ALE itself has no no-op starts. |
| `start.later_episodes` | `"reload"` or `"chain"` | How a pair of seeds' episodes after its first start. `"reload"`: from `start.reset`, as the first does, so every episode is a first episode. `"chain"`: the core's own soft reset (`retro_reset`) from wherever the last episode ended, then the game's reset script: on the ALE core that is ALE's `reset_game()`, which is how ALE v5's environments start every episode but the first (ale-py's `AtariEnv.reset` reloads the ROM only when given a seed), so an episode can start from a state the previous one left (docs/cores.md, "ALE parity"). A pair's first episode always starts from `start.reset`. No default: the two give different scores on some games. `"chain"` needs games reset by script, not a savestate. |
| `max_episode_frames` | integer >= 1 | The cap: an episode is truncated after this many emulated frames, counted from the reset (the end of the game's reset), no-ops included. Required, so an idle policy can't run forever. |
| `life_loss_terminal` | must be `false` | Episodes end at game over, never at a lost life (Machado et al. 2018, `machado2018revisiting` C8 in the research KB). The field is kept so the choice is visible in the text. |
| `reward_clip` | must be `"none"` | Scores are the game's own. Papers that clip rewards clip them for training; their reported scores aren't clipped. |
| `score_var` | a RAM variable name | The game config's RAM variable holding the score (`score` for our games). The episode's score is its value at the end, minus at the start. |
| `episodes_per_seed` | integer >= 1 | Episodes played for each pair of policy seed and env seed. |
| `env_seeds` | non-empty array of distinct integers >= 0 | Seeds for the env's randomness (sticky actions, no-ops). The env is seeded once per pair, and its RNG runs on from one episode to the next. |
| `policy_seeds` | non-empty array of distinct integers >= 0 | One per policy evaluated: a training run's seed for a trained model, the sampling seed for a stochastic policy. |

An evaluation plays `len(policy_seeds) * len(env_seeds) * episodes_per_seed` episodes per
game.

### Canonical text and SHA-1

`EvalProtocol.expand()` returns `Expanded(text, sha1)`: the protocol as TOML with no
comments, the top-level fields in the order above and then the `[obs]` and `[start]` tables,
one fixed form per value; `sha1` is the SHA-1 of the text's UTF-8 bytes. Equal protocols give
the same text whatever their file's comments or layout, and loading the text gives back an
equal protocol. Results store both: two results compare only when their SHA-1s match. The
tests pin each preset's SHA-1, so editing a preset fails them on purpose: a changed preset
is a new protocol, so give it a new name (`ours-v2`) rather than changing the old one's
numbers. Two exceptions, both made while no result was bound to the presets: the Atari presets
(`machado2018`, `mgdt2022`, `jowa2025`) first named bare ALE ids (`ms-pacman`) that no game
config had, and were changed in place to the `atari-<rom>` ids when the Atari games were added;
and `ours-v1` kept its name when `start.later_episodes` was added, its text gaining
`later_episodes = "reload"`, how it already played (its SHA-1 changed, from `7c4e3f0`). The
Atari presets themselves became `machado2018-v2`, `mgdt2022-v2` and `jowa2025-v2` then, with
the minimal action sets and chained episodes; the old names are gone.

## Presets

`protocols/` holds the presets (`gtharness.protocol.PROTOCOLS_DIR`). Each one cites, in a
comment above each field, the knowledge-base claim (`research/kb/papers/<id>.md`) it comes
from. A field its source leaves open is marked `# guessed:` with the reason for the value
chosen; the planned KB intake (DQN-Replay, DQN, rliable) can settle several of them. `jowa2025-v2`
also cites the paper's Table 16 (evaluation settings) and the JOWA evaluation code as the
research experiment `2026-11-24-jowa-mac-smoke` describes it.

| Preset | For | Skip | Sticky | Pool | Stack | Cap (frames) | No-ops | Later episodes | Episodes x seeds | Games |
|---|---|---|---|---|---|---|---|---|---|---|
| `ours-v1` | our games | 2* | 0.25 | no* | 2* | 216,000* | 0 | reload | 10 x 3 policy seeds* | gunhed, zanac* |
| `machado2018-v2` | robustness column | 5 | 0.25 | yes | 4 | 18,000 | 0 | chain | 100 x 5 policy seeds* | Atari pilot 12* |
| `mgdt2022-v2` | MGDT comparisons | 4* | 0 | yes* | 1 | 108,000* | 0* | chain | 16 x 1 | MGDT's 5 held-out |
| `jowa2025-v2` | JOWA comparisons | 4 | 0 | yes | 4 | 108,000 | 0* | chain | 16 x 1 | JOWA's 5 held-out |

(* guessed; the file gives the reason.) The Atari presets use 84x84 grayscale, each game's
minimal action set (`atari_minimal`) and `"game"` crop and orientation; `ours-v1` uses 128x128
RGB and the harness actions. Every preset has `life_loss_terminal = false` and `reward_clip =
"none"`.

The Atari presets' `atari_minimal` and `"chain"` are the user's decisions (2026-11-28), and
the `-v2` in their names: ALE v5's environments default to the minimal set and chain episodes,
so these presets play as the ALE v5 environment does. The papers' own models act in the full
18-action set (Machado et al. play it always; MGDT and JOWA's models output 18 actions), so a
policy for them maps its actions onto the game's minimal set with `GameInfo.atari_actions`
(the ALE action behind each index): it plays the index of its ALE action there, for example
restricting its output to those actions first.

- `ours-v1` follows Machado et al.'s sticky actions, since our cores are deterministic and a
  memorising policy collapses under them (`machado2018revisiting` C2); the rest follows the
  research plan's defaults. Its games are the configured ones with a `score` variable and an
  `episode.terminate` condition; games configured later go into a new version.
- `machado2018-v2` is the recommended ALE protocol (Table 5 of the paper), played by a trained
  policy for 100 episodes rather than averaged over the last 100 training episodes.
- `mgdt2022-v2` and `jowa2025-v2` evaluate on each paper's held-out games. A policy's own settings
  (JOWA's epsilon-greedy 0.001 and its restriction to each game's valid actions, MGDT's
  sampling) belong to the policy, not the protocol.
- The Atari game ids are `atari-` and ALE's ROM names with hyphens (`atari-ms-pacman`, so
  that an arcade game of the same name keeps its own id); their `score` is ALE's reward sum
  (docs/games.md, "Atari (ALE core)").

## Policies

A policy is what `gth evaluate` plays (below). `gtharness.policies.Policy` has:

| Member | Meaning |
|---|---|
| `id` | A name and version (`random-v1`) or the model and run; in `run.json`, every row and recordings' `player`. |
| `checkpoint_sha256` | The SHA-256 of the weights it loaded, or None for a policy without weights. |
| `obs_requirements` | An `ObsRequirements`: `size`, `grayscale`, `maxpool`, `frame_stack`, `frameskip` and `action_set`, each the protocol value the policy needs, or None for any. All are required, so "any" is written down. |
| `reset(seed, game)` | Starts an episode: `seed` an int seeds the policy's randomness, None lets it run on. `game` is a `GameInfo` (`game_id`, `n_actions`, `atari_actions`: the ALE action number behind each of the policy's actions under `atari_full` and `atari_minimal`, None under `harness`; `mapped_slots`). It clears the policy's per-episode state. |
| `act(obs)` | One action in `0..n_actions`, or a chunk: a non-empty 1-D sequence of them, played one per step before `act` is called again. An episode that ends mid-chunk drops the rest. Under `action_set = "harness"` an action is a flat action (`direction * 16 + buttons`, `n_actions` the game's); under `"atari_full"` it is ALE's action number (`n_actions` 18), which the runner turns into a flat action with `GameConfig.from_atari` (docs/actions.md, "Atari": direction and `shot`, never `bomb`, `aux1` or `aux2`); under `"atari_minimal"` it is an index into the game's minimal set (`n_actions` its size, `atari_minimal_actions` in its game.toml), whose ALE action goes through `from_atari` the same way. |

`MODULE:NAME` is called as `NAME(policy_seed=seed, **kwargs)`, once per policy seed. The
protocol's policy seeds stand for training runs (`policy_seeds` above), so a trained model
loads the checkpoint of that seed's run, for example from a `checkpoints` kwarg mapping
seeds to files, and reports its `checkpoint_sha256`; a policy without weights may use the
seed or ignore it. `id` and `checkpoint_sha256` may differ between seeds, and the results
keep them per seed. Making the policy may raise (a missing checkpoint, say): that refuses
the run.

Policy seeds that share one non-null `checkpoint_sha256` (one model played with several
seeds) are sampling seeds of that model, not training runs: the runner accepts them, but
their episodes measure only the evaluation's randomness. Aggregates over runs
(`gtharness.metrics.run_scores`, docs/metrics.md "Runs") refuse to treat them as independent
runs unless the caller says `seeds_are="sampling_seeds"`.

`gtharness.policies.random:RandomPolicy` (`random-v1`) draws each step's flat action
uniformly from `0..n_actions` with numpy's PCG64. That is uniform over what the actions do:
each distinct press appears 2^k times, k the game's unmapped slots. It is the "random" of
normalised scores and plays only the `harness` action set.

## Adapters

Released models played as policies. Each lives in `gtharness.policies`, imports its
framework only when made (an extra, so the base install and CI need neither the framework
nor the weights), and is tested in CI with a stand-in model.

### JAT

`gtharness.policies.jat:JatPolicy` (`jat-v1`) plays JAT (Gallouédec et al. 2024), the hub's
[`jat-project/jat`](https://huggingface.co/jat-project/jat), through the checkpoint's own
code (`trust_remote_code`, transformers 4.36). Setup, once per machine:

```sh
uv sync --extra jat            # torch 2.5.1, torchvision, transformers 4.36.2
uv run maturin develop --uv    # uv sync removed the extension module
uv run python -c "from huggingface_hub import snapshot_download as d; \
  print(d('jat-project/jat', revision='911dd7447b1c56bece63424c888d2217828c981e'))"
```

The last line downloads the weights (738 MB) into the hub cache, never the repo, and prints
the snapshot directory that `checkpoint` names. Its `model.safetensors` has SHA-256
`5b66502d0c5687998593d89582dc18697d3d144871b0905c345126e632c22508`, which the policy
reports as `checkpoint_sha256`. The policy refuses a directory where any other file (code,
configs, tokenizer) differs from that revision's (`CODE_SHA256`): loading runs that code, the
configs and tokenizer files name code to run, and the adapter drives it (`get_next_action`
and its KV-cache trimming). Every setting is a kwarg without a default:

| Kwarg | Meaning |
|---|---|
| `checkpoint` | The snapshot directory. |
| `action_set` | `"atari_minimal"` or `"atari_full"`, which the protocol must match. JAT was trained and evaluated on each Atari game's minimal set, so its action `i` means that set's `i`-th action: on an Atari game only `atari_minimal` plays it as trained. `"atari_full"` (ALE's 18) is for games without one, such as the arcade games. The action head is cut to the game's `n_actions`, as upstream cuts it to the env's. |
| `context_timesteps` | Past timesteps kept in the KV cache, two positions each: upstream's Atari evaluation keeps 16 (`context_window = 32`); the checkpoint allows up to `max_position_embeddings // 2` (256). |
| `deterministic` | The most likely action instead of a sample. Upstream samples (`false`). |
| `device` | `"cpu"`, `"mps"` or `"cuda"`. |
| `threads` | torch's CPU threads per process; each of `--jobs` processes holds a model. |

```sh
uv run gth evaluate data --protocol PROTOCOL --policy gtharness.policies.jat:JatPolicy \
    --policy-kwargs '{"checkpoint": "SNAPSHOT", "action_set": "atari_minimal",
      "context_timesteps": 16, "deterministic": false, "device": "cpu", "threads": 1}' \
    --jobs 4 --out results/jat
```

All policy seeds share the checkpoint, so they are sampling seeds (above). Sampling uses
torch's global RNG, which `reset` seeds; the runner plays one policy per process.

What JAT saw, from upstream's `make_atari` (jat repo `jat/eval/rl/core.py`, commit
`2e1ac33`), and how a protocol matches it:

| Upstream | Here |
|---|---|
| ALE v5, `frameskip=1`, `repeat_action_probability=0`, then `MaxAndSkipEnv(4)`: frame skip 4, max of the last two frames | `frameskip = 4`, `maxpool = true`, `sticky_prob = 0`: required by `obs_requirements` except `sticky_prob`, which the protocol chooses |
| Each game's minimal action set (ALE v5's default) | `action_set` above |
| `ResizeObservation((84, 84))` (OpenCV `INTER_AREA` on RGB), then `GrayScaleObservation` (`cv2.COLOR_RGB2GRAY`). The paper says bicubic; the code is area (the bicubic `resample` in `preprocessor_config.json` is the CLIP image processor's, for the text-and-image tasks; RL observations bypass it) | `size = [84, 84]`, `grayscale = true`, `interpolation = "area"`; the env makes the frame gray before resizing, which differs from upstream only in rounding. `obs_requirements` has no interpolation, so the protocol chooses it and the policy can't refuse another: use `"area"` (every preset does) |
| `FrameStack(4)`, oldest first, transposed to `[84, 84, 4]`; the processor scales to `[-1, 1]` | `frame_stack = 4`; the policy turns the runner's `uint8[4, 84, 84, 1]` into `[84, 84, 4]` |
| `NoopResetEnv(30)` | `noop_max`, the protocol's choice |
| `EpisodicLifeEnv`, `FireResetEnv` (training wrappers kept at evaluation) | not reproduced: the protocol's `life_loss_terminal` and reset decide |
| Rewards passed to the model | none: the model ignores rewards next to image observations (its `embed_rl`), and policies get none |

The Atari presets fit: their observations, and their `action_set = "atari_minimal"`, under
which JAT plays its Atari games as trained (pass `"action_set": "atari_minimal"`). Measured
on an Apple M4 (CPU, debug extension, `tests/test_jat_adapter.py`'s protocol on the test
core, 1015 steps): 26 ms per
step with 4 threads, 24 ms with 1, so more `--jobs` beat more threads; the chosen actions
were the same both times.

## Running an evaluation

`gth evaluate` (`gtharness.evaluate.evaluate`) plays a policy under a protocol:

```sh
uv run gth evaluate data --protocol ours-v1 --policy gtharness.policies.random:RandomPolicy \
    --out results/random-ours-v1 --jobs 8 [--record --license CC-BY-NC-4.0 --checks-every 600]
```

- `--protocol` is a file (a path ending in `.toml` or with a directory in it) or a preset's
  name in `protocols/`. There is no run without one.
- `--policy MODULE:NAME` names a callable (usually a class) that returns a `Policy` (above);
  `--policy-kwargs` passes it a JSON object of keyword arguments (checkpoint paths, say),
  which `run.json` keeps.
- `--jobs` (required) is how many pairs of seeds play at once. Each runs in its own process,
  which makes its own policy: a model policy holds `--jobs` copies of its weights, in RAM or
  on the GPU, so pick it for the policy and the machine (the CPU count for the random
  policy; what fits in memory for a model).
- `--out` must be new or empty: results are never overwritten.
- `--record` records every episode (docs/recording-format.md) under
  `OUT/recordings/<game>/`, with recorder `policy` and player
  `<policy id> checkpoint=<sha256 or none> seed=<policy seed>`; it needs `--license` and
  `--checks-every` (as in `gtharness.record.Record`).
- Files are found as `RetroEnv.from_game` finds them: `DATA_DIR`, `--rom-dir`,
  `--core-path` (only for protocols whose games share one core) and `--games-dir`.
- Exit status: 0 when the run finished, 1 when an episode's process failed, 2 for a refused
  run (a check below, or the policy can't be made) or a usage error.

### Seeds, processes and steps

Every game is played for every pair of policy seed and env seed, `episodes_per_seed`
episodes each, in a spawned process per pair (one core per process, docs/architecture.md),
`--jobs` pairs at a time. The results don't depend on `--jobs`. The first failure stops the
run at once: no further pair starts, the running ones are terminated, and the error names
the game and seeds. Within a pair:

- the policy is made as `NAME(policy_seed=policy_seed, **kwargs)` ("Policies" above);
- the first episode calls `env.reset(seed=env_seed)` and `policy.reset(seed, game)`, with
  `seed` derived from both seeds (`numpy.random.SeedSequence([policy_seed, env_seed])`, its
  first 32-bit word), so a stochastic policy plays a different stream under each env seed;
  later episodes call `reset(seed=None)` and `policy.reset(None, game)`, so both RNGs run on
  from one episode to the next (the protocol's seed rule above) and a pair's episodes
  differ;
- the env's `later_episodes` is the protocol's `start.later_episodes`: under `"chain"` each
  later episode starts with the core's soft reset where the previous one ended (a seeded
  reset, the pair's first, starts from the load state), so a pair's episode N depends on its
  episodes 1..N-1. The runner plays a pair's episodes in order in one process anyway, and
  pairs never share a core, so chaining changes nothing about `--jobs`: a pair is the unit of
  parallelism, and the results still don't depend on it;
- the env runs the protocol's frame skip, sticky actions, no-op starts, cap, observation size
  and interpolation, with flat actions (docs/actions.md) and the game's crop and orientation;
- the policy sees the last `frame_stack` observations, oldest first, as one
  `uint8[frame_stack, height, width, channels]` array; at an episode's start the first
  observation fills the stack (gymnasium's `FrameStackObservation` padding);
- an episode ends when the game's `episode.terminate` condition holds (terminated) or at the
  cap (truncated); both can hold on the same step.

The policy never sees RAM: `info["ram"]` gives the score and nothing else.

Before any episode the runner makes the policy for every seed in one spawned process and
checks it there (its type, `id`, `checkpoint_sha256`, and every `obs_requirements` field
against the protocol), so no weights stay loaded in the runner's own process. Each pair's
process then makes its seed's policy again, which must give the same `id` and checkpoint.
A policy with weights loads them once per pair of seeds.

### Checks before a run

Nothing is written, and no episode starts, until all of these pass (`ConfigError` naming the
field or game otherwise):

- The env supports every setting (`maxpool` and `obs.grayscale` go to `RetroEnv.from_game`
  as they are). `atari_minimal` needs every game to have a minimal action set
  (`atari_minimal_actions`, docs/games.md: the Atari games), `start.later_episodes = "chain"`
  needs every game to reset by script rather than from a savestate, `start.reset` must be
  `"game"`, and an explicit `obs.crop` or `obs.orientation` must equal the game config's, since the env
  uses the game's.
- `start.noop_max + frameskip < max_episode_frames`: the env's rule, so the reset (no-ops and
  the first observation's frames) leaves an episode at least one step. It is stricter than
  the protocol's own `noop_max < max_episode_frames`.
- Each game: its `games/<id>/game.toml` exists, its content is found by SHA-1 (so the
  protocol plus the game configs pin the dumps), its BIOS files, reset savestate (when it
  has one) and core library are there, and `score_var` is one of its RAM variables.
- The policy, for every policy seed: `MODULE:NAME` imports, makes a `Policy` without
  raising, and its `obs_requirements` match.

## Results

A run's directory holds three files; `gtharness.evaluate.check_results(dir)` checks one
against this schema (and that every row agrees with the protocol) and returns its contents.
`run.json` is written last, so a directory without it is an unfinished or failed run. Aggregates
over runs (normalised scores, IQM, bootstrap intervals) are in docs/metrics.md.

`protocol.toml` is the protocol's canonical text (`EvalProtocol.expand`), so it records
every setting the run played under, `start.later_episodes` included; `run.json`'s
`protocol_sha1` is its SHA-1. Two runs compare only when their SHA-1s match.

`episodes.jsonl` has one JSON object per episode, ordered by game (the protocol's order),
policy seed, env seed and episode, with these fields in this order:

| Field | Type | Meaning |
|---|---|---|
| `game` | string | The game id. |
| `env_seed`, `policy_seed` | integer | The pair of seeds. |
| `policy_id`, `checkpoint_sha256` | string; string or null | The policy made for `policy_seed`: its `id` and checkpoint, as in `run.json`. |
| `episode` | integer | The episode's index within its pair, from 0. |
| `score` | integer | `score_var` at the end minus at the start (after the reset and the first observation). |
| `frames` | integer | Emulated frames counted as the cap counts them: no-ops, the first observation's frames and every step's, not the reset script's. In `(frameskip, max_episode_frames]`. |
| `steps` | integer | Env steps (actions played), at least 1. |
| `terminated` | bool | The game's `episode.terminate` condition held at the end. |
| `truncated` | bool | The cap ended it: true exactly when `frames == max_episode_frames`. |
| `mapped_slots_used` | array of slot names | The game's mapped slots the policy pressed at least once, in slot order. A policy that never uses `bomb` (an Atari-trained one, say) shows here. |
| `recording` | string or null | The episode's recording, relative to the run's directory; null without `--record`. |

`run.json`:

| Field | Meaning |
|---|---|
| `format` | `1`; bumped whenever a results file's fields change. |
| `protocol_sha1` | The SHA-1 of `protocol.toml`. |
| `policy` | `spec` (`MODULE:NAME`), `kwargs`, and `seeds`: per policy seed (a string key, in the protocol's order), the `id` and `checkpoint_sha256` of the policy made for it. |
| `games` | Per game, in the protocol's order: `core` and `content_sha1`. |
| `recorded` | Whether episodes were recorded. |
| `harness` | The build (`_native.build_info()`: git SHA, dirty flag, profile, rustc, target). |
| `machine` | Platform, CPU, CPU count, Python (as in `gth bench`). |
| `started`, `wall_s` | The UTC start time and the run's wall time in seconds. |
| `jobs` | Pairs of seeds played at once. |
| `episodes`, `steps`, `policy_calls` | Totals over the run. |
| `policy_ms_per_step` | Time spent in `act`, over all steps, in milliseconds per step (a chunk's time is spread over its steps). |

## Human reference

A human reference score is what a person scores in one credit under a protocol, next to the
agents' scores under it. `gth reference-scores` (`gtharness.reference`) reads it from
recordings, by replay:

```sh
gth reference-scores <data_dir> <recordings_dir>/gunhed-*.gthrec --protocol ours-v1 --out ref.json
```

Record the credits with `gth-play --record-from-reset --rewind-seconds 0`
(docs/human-play.md, "Reference credits"). A recording is a reference credit when:

- its `recorder` is `human`, with no `[[rewind]]` and no `[branch]` (rewinds and their clips
  aren't one credit), and its game is one of the protocol's;
- it starts at the game's reset: its start state's SHA-1 equals a fresh core's after the
  game's reset script (or reset savestate), which is where `RetroEnv` starts an episode;
- it reaches the first frame where the game's `episode.terminate` holds, or the protocol's
  `max_episode_frames`, before it ends. The replay stops there; the frames after it don't
  count. A credit cut at the cap is `truncated`, as an agent's episode would be.

Anything else is refused with the recording named (exit status 2): the output only ever holds
credits that follow the rule. The score is the protocol's `score_var` where the replay
stopped minus at the start state, the reset. An agent's episode `score` is counted from its
first observation instead, after the no-ops a person doesn't get; the two differ only by
what a game scores during idle no-op frames. The game's
config is today's `games/<id>/game.toml` (its RAM variables and `episode.terminate`), the
one an evaluation uses; the replay verifies every desync check on the way.

What of the protocol applies to a person: the start (`start.reset`, which must be `"game"`),
the cap and `score_var`. A person plays every frame on the whole pad, so `frameskip`,
`sticky_prob`, `start.noop_max`, `start.later_episodes` (a credit is one episode from the
reset), the observation settings and `action_set` don't apply, and
a protocol with `life_loss_terminal = true` is refused, since a credit runs to game over. The
protocol's other checks are `check_protocol`'s ("Checks before a run"), for the games that
have credits: a game of the protocol with none is listed with `n` 0 and needs no files, so
credits of one game score without the other games' ROMs.

The output (`format` 1) is JSON:

| Field | Meaning |
|---|---|
| `format` | `1`; bumped whenever a field changes. |
| `protocol` | `sha1` and `text` (the canonical text, "Canonical text and SHA-1"), and `human`: `start_reset`, `max_episode_frames` and `score_var`, the settings that applied. |
| `games` | Per game of the protocol, in its order: `n` (credits), `mean` and `median` of their scores (null with none), `recordings` (the recordings' directory names, sorted), `credits` (per recording, in that order: `recording`, `player`, `score`, `frames` replayed, `terminated`, `truncated`), and `warning` when `n` is under 5 (a mean of so few is noisy; not an error). |
