#!/usr/bin/env bash

set -euo pipefail

# Configurable values
TPS="${TPS:-1000}"
NETWORK="${NETWORK:-alice}"
LOG_TAG=${TAG}
MNEMONIC_START_INDEX=${MNEMONIC_START_INDEX:-30000}

# Find next available index based on existing log files
find_next_index() {
  local max_index=0
  for file in ${NETWORK}-${LOG_TAG}*.log; do
    [[ -e "$file" ]] || continue
    base=$(basename "$file")
    index_part="${base##*-}"
    index="${index_part%%.log}"
    if [[ "$index" =~ ^[0-9]+$ && "$index" -gt "$max_index" ]]; then
      max_index="$index"
    fi
  done
  echo $((max_index + 1))
}

INDEX=$(find_next_index)
LOGFILE="${NETWORK}-${LOG_TAG}-${INDEX}.log"

# Handle Ctrl+C cleanly
cleanup() {
  echo "Interrupted. Cleaning up..."
  kill 0
  exit 1
}
trap cleanup SIGINT

# Disable cargo color output completely
cargo run -p inu --release -- run --metrics --max-tps "$TPS" --network "$NETWORK" --mnemonic-start-index "$MNEMONIC_START_INDEX" -d "20min" 2>&1 | tee "$LOGFILE"