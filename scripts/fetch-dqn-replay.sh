#!/usr/bin/env bash
# Downloads one DQN-Replay checkpoint, the five files `gth import-dqn-replay` reads, after
# printing their sizes, and only when their total is within a budget (docs/recording-format.md,
# "DQN-Replay").
#
#   scripts/fetch-dqn-replay.sh <Game> <run> <checkpoint> <dest> <budget_bytes>
#
# <Game> is Dopamine's name (Pong, MsPacman), <run> 1-5. The files land in
# <dest>/<Game>/<run>/replay_logs. Needs gsutil and `gcloud auth login` (the bucket refuses
# anonymous reads). Exit status: 0 when copied, 1 when the total is over the budget (nothing
# is copied), 2 on a usage error.
set -euo pipefail

if [ $# -ne 5 ]; then
  echo "usage: $0 <Game> <run> <checkpoint> <dest> <budget_bytes>" >&2
  exit 2
fi
game=$1 run=$2 checkpoint=$3 dest=$4 budget=$5
if ! [[ $budget =~ ^[0-9]+$ && $checkpoint =~ ^[0-9]+$ ]]; then
  echo "$0: <checkpoint> and <budget_bytes> must be whole numbers" >&2
  exit 2
fi
src="gs://atari-replay-datasets/dqn/$game/$run/replay_logs"
files=()
for name in '$store$_observation' '$store$_action' '$store$_reward' '$store$_terminal' add_count; do
  files+=("$src/${name}_ckpt.$checkpoint.gz")
done

sizes=$(gsutil du -c "${files[@]}")
echo "$sizes"
total=$(awk '$2 == "total" { print $1 }' <<<"$sizes")
if ! [[ $total =~ ^[0-9]+$ ]]; then
  echo "$0: gsutil du gave no total" >&2
  exit 2
fi
if (( total > budget )); then
  echo "$total bytes is over the budget of $budget: not copying" >&2
  exit 1
fi
out="$dest/$game/$run/replay_logs"
mkdir -p "$out"
gsutil -m cp "${files[@]}" "$out/"
echo "$total bytes in $out"
