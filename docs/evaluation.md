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
setting) is checked when the protocol is used.

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
| `obs.interpolation` | `gtharness.env.INTERPOLATIONS`: `"area"` or `"bilinear"` | How frames are resized (the env's `interpolation`). The env, the shard exporter and protocols share the one tuple. |
| `obs.grayscale` | bool | Gray observations instead of RGB. |
| `frame_stack` | integer >= 1 | How many observations the policy sees at once. The env doesn't stack: this is recorded for the policy, which must use this many. |
| `action_set` | `"harness"`, `"atari_full"`, `"atari_minimal"` | The policy's action space: the harness's direction + four slots (docs/actions.md), or Atari's 18 actions or a game's minimal set. |
| `start.reset` | `"game"` or a savestate's lowercase SHA-1 | Where an episode starts: the game config's reset (script or savestate), or a given savestate. |
| `start.noop_max` | integer >= 0, below `max_episode_frames` | Random no-op starts. 0: none, the first observation follows the reset directly. Otherwise each episode starts with N no-op frames (no input), N drawn uniformly from 1..noop_max with the env seed. This is the convention of OpenAI baselines' `NoopResetEnv`, which implements DQN's "up to 30 no-ops" (Mnih et al. 2015, p. 6) and which IRIS and many Atari codebases copy. N counts emulated frames, as `NoopResetEnv` does (it wraps the frame-skip-free env); ALE itself has no no-op starts. |
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
numbers.

## Presets

`protocols/` holds the presets (`gtharness.protocol.PROTOCOLS_DIR`). Each one cites, in a
comment above each field, the knowledge-base claim (`research/kb/papers/<id>.md`) it comes
from. A field its source leaves open is marked `# guessed:` with the reason for the value
chosen; the planned KB intake (DQN-Replay, DQN, rliable) can settle several of them. `jowa2025`
also cites the paper's Table 16 (evaluation settings) and the JOWA evaluation code as the
research experiment `2026-11-24-jowa-mac-smoke` describes it.

| Preset | For | Skip | Sticky | Pool | Stack | Cap (frames) | No-ops | Episodes x seeds | Games |
|---|---|---|---|---|---|---|---|---|---|
| `ours-v1` | our games | 2* | 0.25 | no* | 2* | 216,000* | 0 | 10 x 3 policy seeds* | gunhed, zanac* |
| `machado2018` | robustness column | 5 | 0.25 | yes | 4 | 18,000 | 0 | 100 x 5 policy seeds* | Atari pilot 12* |
| `mgdt2022` | MGDT comparisons | 4* | 0 | yes* | 1 | 108,000* | 0* | 16 x 1 | MGDT's 5 held-out |
| `jowa2025` | JOWA comparisons | 4 | 0 | yes | 4 | 108,000 | 0* | 16 x 1 | JOWA's 5 held-out |

(* guessed; the file gives the reason.) The Atari presets use 84x84 grayscale, the full
18-action set and `"game"` crop and orientation; `ours-v1` uses 128x128 RGB and the harness
actions. Every preset has `life_loss_terminal = false` and `reward_clip = "none"`.

- `ours-v1` follows Machado et al.'s sticky actions, since our cores are deterministic and a
  memorising policy collapses under them (`machado2018revisiting` C2); the rest follows the
  research plan's defaults. Its games are the configured ones with a `score` variable and an
  `episode.terminate` condition; games configured later go into a new version.
- `machado2018` is the recommended ALE protocol (Table 5 of the paper), played by a trained
  policy for 100 episodes rather than averaged over the last 100 training episodes.
- `mgdt2022` and `jowa2025` evaluate on each paper's held-out games. A policy's own settings
  (JOWA's epsilon-greedy 0.001 and its restriction to each game's valid actions, MGDT's
  sampling) belong to the policy, not the protocol.
- The Atari game ids are ALE's ROM names with hyphens (`ms-pacman`); they don't exist until
  the Atari game configs are added, and their `score_var` is a guess at the name the ALE
  core's score variable will get.

## Policies

A policy is what an evaluation plays. `gtharness.policies.Policy` has:

| Member | Meaning |
|---|---|
| `id` | A name and version (`random-v1`) or the model and run; in `run.json`, every row and recordings' `player`. |
| `checkpoint_sha256` | The SHA-256 of the weights it loaded, or None for a policy without weights. |
| `obs_requirements` | An `ObsRequirements`: `size`, `grayscale`, `maxpool`, `frame_stack`, `frameskip` and `action_set`, each the protocol value the policy needs, or None for any. All are required, so "any" is written down. |
| `reset(seed, game)` | Starts an episode: `seed` an int seeds the policy's randomness, None lets it run on. `game` is a `GameInfo` (`game_id`, `n_actions`, `mapped_slots`). It clears the policy's per-episode state. |
| `act(obs)` | One flat action (`direction * 16 + buttons`), or a chunk: a non-empty 1-D sequence of them, played one per step before `act` is called again. An episode that ends mid-chunk drops the rest. |

`MODULE:NAME` is called as `NAME(policy_seed=seed, **kwargs)`, once per policy seed. The
protocol's policy seeds stand for training runs (`policy_seeds` above), so a trained model
loads the checkpoint of that seed's run, for example from a `checkpoints` kwarg mapping
seeds to files, and reports its `checkpoint_sha256`; a policy without weights may use the
seed or ignore it. `id` and `checkpoint_sha256` may differ between seeds, and the results
keep them per seed. Making the policy may raise (a missing checkpoint, say): that refuses
the run.

`gtharness.policies.random:RandomPolicy` (`random-v1`) draws each step's flat action
uniformly from `0..n_actions` with numpy's PCG64. That is uniform over what the actions do:
each distinct press appears 2^k times, k the game's unmapped slots. It is the "random" of
normalised scores and plays only the `harness` action set.
