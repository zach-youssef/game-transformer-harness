# Training data

`gtharness.train` turns training shards (docs/recording-format.md, "Training shards") into
what a trainer consumes: sequence windows with actions, rewards, returns-to-go and timesteps.
It is numpy only, so it runs and is tested everywhere the harness is; the PyTorch side (P10's
dataset, the research repo's baselines) wraps it rather than reading shards itself.

RAM is for rewards and labels only. A window holds rewards computed from the shard's RAM
variables, never the variables themselves: no policy ever sees game RAM.

## Windows

`Windows(paths, length=, stride=, reward=, action_format=, cache_chunks=)` opens the shards
and indexes every window of `length` consecutive samples that lies within one `segment`:

- Windows start at each segment's first sample and then every `stride` samples
  (`stride = length` for windows that don't overlap, 1 for all of them). A window never
  crosses a segment gap, so it never pairs samples across dropped ones.
- A segment shorter than `length` gives no window, and the samples of a segment's tail that no
  stride reaches (fewer than `stride`) are in none. Models that need a short context at an
  episode's start pad it themselves; the windows are always full.
- Every argument is required: the window shape, the reward and the action format are the
  experiment's choices.

Each window (`windows.window(i)`, or one row of a batch) holds:

| Key | Type | Meaning |
|---|---|---|
| `frame` | `uint8[length, H, W, C]` | The observations: `C` is the shards' `channels` (3, or 1 for gray). |
| `action` (flat) | `int64[length]` | The action after each, `direction * 16 + buttons` (docs/actions.md). |
| `direction`, `buttons` (dict) | `uint8[length]`, `uint8[length, 4]` | The same as `RetroEnv`'s dict action: buttons one 0/1 per slot. |
| `reward` | `float64[length]` | Each sample's reward (below). |
| `return_to_go` | `float64[length]` | The sum of the rewards from that sample to the shard's end. |
| `timestep` | `int64[length]` | The sample's index in its shard. |
| `episode_frame`, `confidence` | `uint64[length]`, `float32[length]` | As in the shard. |
| `shard`, `sample` | `int64` | The index into `paths` and the window's first sample. |

`action_format` is `"flat"` (one categorical head, `Discrete(144)`) or `"dict"` (a direction
head and four button heads), the same choice as `RetroEnv`'s, so predictions go to the env
as they are.

`timestep` counts the shard's samples, not dropped ones: it is the agent step since the shard's
first sample when nothing was dropped. A sample filter that drops the idle start (`playing`)
makes it count from the observation the first input answers. `episode_frame` gives the exact
position in the episode when a model needs it.

### Rewards and returns-to-go

`sample_rewards(shard, reward)` gives sample i the reward `reward(ram[i], ram[i + 1])` of the
RAM variables at its observation and at the next sample's: the reward of the step that answers
the observation, as `RetroEnv` gives it (`gtharness.rewards`, same `Reward` signature). So:

- A reward across a segment gap covers the dropped samples' frames too, and the rewards of a
  shard sum to the whole episode's (from its first sample's observation to its last's).
- The last sample's reward is 0: a shard holds no RAM after its last sample's step. For
  `score_delta` this loses at most that last step's points, and a shard cut at game over has
  none to lose.
- Imported datasets (docs/recording-format.md, "Imported datasets") store each step's reward
  with the observation it answers, in their `reward` column: `rewards.dataset_reward` reads
  it (`previous["reward"]`). It reads only the RAM before the step (`rewards.PREVIOUS_ONLY`),
  so the last sample gets its reward too, and an episode's terminal step keeps its points.
- A built-in reward (in `REQUIRED_RAM`, also inside `functools.partial`) whose RAM variables a
  shard lacks raises `ConfigError` when the windows are made; any other reward raises
  `KeyError` at its first call.

`returns_to_go(rewards)` is `rtg[i] = rewards[i:].sum()` (including sample i's own reward, the
decision transformer's convention), over the whole shard, across segment gaps: the episode goes
on through a gap, so the return still to come does too. For `score_delta` it is the score
still to come, `score[last] - score[i]`. It is the episode's return to come only when the
shard ends at game over (`meta["terminated"]`); a shard whose recording stopped mid-game
(filter `all`) gives a truncated return, which understates the episode as a conditioning
target, so pick or weight shards by `terminated` where that matters.

Rewards are computed once per shard when the windows are made (one call of the reward per
sample, in Python, which is per-step work, not per-frame).

### Reading windows

- `window(i)` gives one window, decoding the frame chunks it needs through a cache of the
  `cache_chunks` most recently used chunks. A window spans at most
  `(length + chunk_samples - 2) // chunk_samples + 1` chunks, and `cache_chunks` must hold
  that many (`ValueError` otherwise). Windows are numbered by shard, then by first sample;
  `location(i)` gives a window's shard and first sample without decoding anything. This is
  the random-access path, for a map-style PyTorch dataset.
- `batches(batch_size, rng=, buffer_chunks=)` gives every window once, as batches of stacked
  windows (each key gains a leading batch axis; the last batch may be smaller). With `rng`,
  windows are grouped by the frame chunk they start in, the groups visited in random order,
  and windows shuffled within `buffer_chunks` groups at a time: more mixes better and holds
  more decoded frames. With `rng=None` they come in `window` order. It decodes the chunks a
  buffer needs once and keeps those the next buffer still needs. In order, that decodes each
  chunk about once per pass. Shuffled, a window that starts in chunk c also needs the chunks
  after it, which the buffer holding chunk c + 1's group rarely shares, so each chunk is
  decoded up to the span above times per pass (twice for most windows longer than 1 sample
  when `length` is near `chunk_samples`). zstd decoding dominates the read cost, so make
  `chunk_samples` large next to `length` to bring that toward once: fewer windows then cross
  a chunk end. Raising `buffer_chunks` helps only once it nears the number of start-chunk
  groups, that is, holding most of the data decoded. This is the path for an iterable
  dataset.

`window(i)`'s arrays are read-only views of arrays that overlapping windows and the chunk
cache share: writing to one (an in-place normalisation, say) raises `ValueError` rather than
silently changing every later window over those samples. Copy an array to change it. A
batch's arrays are new, stacked copies, and writable.

All shards must have the same observation size and `channels` (`HarnessError` otherwise). `Windows` holds
the shards open: close it, or use it as a context manager.

## PyTorch

The torch dataset (P10, the `[train]` extra) builds on `Windows` rather than on `Shard`: a
map-style dataset indexes `window(i)` and an iterable one wraps `batches`, converting the
arrays with `torch.from_numpy`. A window's arrays are read-only, which `torch.from_numpy`
warns about and an in-place op on the tensor would break: copy them first (`torch.tensor`,
or stack a batch as `batches` does). Cast the dtypes torch lacks ops for: `episode_frame` is
`uint64` (to `int64`), and `reward` and `return_to_go` are `float64` because scores can pass
float32's exact range (to `float32` once normalised). Frame stacks are windows with the policy's stack length, and
action-chunk targets are windows of the chunk length (or a longer window split into the
observation part and the target part).

Open item: shards labelled from video by an IDM carry no RAM (docs/recording-format.md), so
they can't give rewards, and `Windows` requires a reward. Windowing them needs a no-reward
mode (no `reward` or `return_to_go` keys), added with the first IDM shards.
